use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use anyhow::{bail, ensure, Result};
use naxus_core::{
    canonical_name, validate_name, ArtifactDigest, IndexEntry, RepositoryPolicy, Snapshot,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{instance::InstanceLock, objects, StoredObject};

/// 数据卷与单次存储操作的显式资源预算。
#[derive(Clone, Debug)]
pub struct StoreOptions {
    pub max_artifact_bytes: u64,
    pub reserve_free_bytes: u64,
    pub busy_timeout: Duration,
    pub max_snapshot_depth: usize,
}

impl Default for StoreOptions {
    /// 业务作用：为本机和服务器提供一致的有界存储默认值。
    /// 参数说明: 无。
    ///
    /// 返回：单制品 256 MiB、保留 2 GiB、SQLite 等待五秒和有界快照读取预算。
    fn default() -> Self {
        Self {
            max_artifact_bytes: 256 * 1024 * 1024,
            reserve_free_bytes: 2 * 1024 * 1024 * 1024,
            busy_timeout: Duration::from_secs(5),
            max_snapshot_depth: 1024,
        }
    }
}

/// 一次原子仓库变更；字节对象必须在调用前已经完成安装。
#[derive(Debug, Serialize, Deserialize)]
pub struct CommitRequest {
    pub repository: String,
    pub expected_head: String,
    pub idempotency_key: String,
    pub actor: String,
    pub entries: Vec<IndexEntry>,
}

/// 对外读取所需的仓库身份与当前策略，不包含本地数据路径。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepositoryInfo {
    pub id: String,
    pub name: String,
    pub policy: RepositoryPolicy,
    pub head: String,
}

/// 独占数据卷的仓库存储；同步接口由调用者放入有界阻塞执行域。
pub struct Store {
    pub(crate) root: PathBuf,
    connection: Mutex<Connection>,
    pub(crate) object_gate: Mutex<()>,
    pub(crate) options: StoreOptions,
    _instance_lock: Arc<InstanceLock>,
    pub(crate) readers: crate::lifecycle::ActiveReaders,
    service_id: String,
}

impl Store {
    /// 业务作用：取得数据卷独占权，完成迁移及已提交对象校验后开放存储。
    /// 参数说明：
    /// - `root`：仅供本实例使用的本地持久目录。
    /// - `options`：对象、容量和数据库预算。
    ///
    /// 返回：文件系统受支持、数据库与对象一致才返回；已有实例、未知 schema 或对象缺失均拒绝启动。
    pub fn open(root: impl AsRef<Path>, options: StoreOptions) -> Result<Self> {
        ensure!(
            options.max_artifact_bytes > 0 && options.max_snapshot_depth > 0,
            "invalid store limits"
        );
        naxus_core::create_private_directory(root.as_ref())?;
        let root = root.as_ref().canonicalize()?;
        crate::filesystem::validate_filesystem(&root)?;
        // 数据库迁移和对象安装共享一个实例所有者，第二个进程不能绕过生命周期并发写同一卷。
        let instance_lock = InstanceLock::acquire(&root)?;
        for directory in ["objects", "objects/sha256", "staging", "trash"] {
            let path = root.join(directory);
            fs::create_dir_all(&path)?;
            ensure!(
                fs::symlink_metadata(&path)?.file_type().is_dir(),
                "data directory cannot be a symlink"
            );
            objects::sync_directory(&path)?;
        }
        objects::sync_directory(&root.join("objects"))?;
        objects::sync_directory(&root)?;
        objects::recover_staging(&root)?;
        let mut connection = Connection::open(root.join("naxus.sqlite3"))?;
        connection.busy_timeout(options.busy_timeout)?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        let schema: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        ensure!(schema <= 13, "unsupported repository schema");
        if schema == 0 {
            // 在同一事务提交 schema 与稳定身份，启动中断不能产生已迁移但身份缺失的仓库。
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!("../migrations/repository.sql"))?;
            transaction.execute(
                "INSERT INTO metadata(key, value) VALUES ('service_id', ?1)",
                [Uuid::new_v4().to_string()],
            )?;
            transaction.commit()?;
            objects::sync_directory(&root)?;
        }
        if schema < 2 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!("../migrations/snapshot_checkpoints.sql"))?;
            transaction.commit()?;
        }
        if schema < 3 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!("../migrations/publish_batches.sql"))?;
            transaction.commit()?;
        }
        if schema < 4 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!("../migrations/retention_leases.sql"))?;
            transaction.commit()?;
        }
        if schema < 5 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!("../migrations/proxy_cache.sql"))?;
            transaction.commit()?;
        }
        if schema < 6 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!("../migrations/proxy_resolution.sql"))?;
            transaction.commit()?;
        }
        if schema < 7 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!("../migrations/access_control.sql"))?;
            transaction.commit()?;
        }
        if schema < 8 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!("../migrations/hosted_management.sql"))?;
            transaction.commit()?;
        }
        if schema < 9 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!("../migrations/object_lifecycle.sql"))?;
            transaction.commit()?;
        }
        if schema < 10 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!("../migrations/proxy_views.sql"))?;
            transaction.commit()?;
        }
        if schema < 11 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!("../migrations/deletion_retries.sql"))?;
            transaction.commit()?;
        }
        if schema < 12 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!("../migrations/token_publish_scope.sql"))?;
            transaction.commit()?;
        }
        if schema < 13 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!("../migrations/unpublished_objects.sql"))?;
            transaction.commit()?;
        }
        let service_id = connection.query_row(
            "SELECT value FROM metadata WHERE key = 'service_id'",
            [],
            |row| row.get(0),
        )?;
        let store = Self {
            root,
            connection: Mutex::new(connection),
            object_gate: Mutex::new(()),
            options,
            _instance_lock: Arc::new(instance_lock),
            readers: Arc::new(Mutex::new(BTreeMap::new())),
            service_id,
        };
        store.recover_batches()?;
        store.recover_deletions()?;
        store.recover_unpublished()?;
        store.check_integrity()?;
        Ok(store)
    }

    /// 业务作用：提供跨进程重启稳定的服务身份，供客户端检测错误服务器或错误恢复。
    /// 参数说明: 无。
    ///
    /// 返回：持久卷中的服务 UUID；不是网络地址，也不随进程启动重新生成。
    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    /// 业务作用：在首次发布前建立具有固定策略和空快照的 hosted 仓库。
    /// 参数说明：
    /// - `name`：唯一仓库名称。
    /// - `policy`：相同版本再次发布的内容策略。
    ///
    /// 返回：新仓库或同策略已有仓库的当前快照；不能通过重复创建更改已有策略。
    pub fn create_repository(&self, name: &str, policy: RepositoryPolicy) -> Result<Snapshot> {
        validate_name(name)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let policy_text = match policy {
            RepositoryPolicy::Mutable => "mutable",
            RepositoryPolicy::Immutable => "immutable",
        };
        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT policy, head FROM repositories WHERE name = ?1",
                [name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((existing_policy, head)) = existing {
            // 仓库策略是已建立的发布边界，不能因为启动配置变化而静默放宽覆盖权限。
            ensure!(
                existing_policy == policy_text,
                "repository policy differs from persisted policy"
            );
            return read_snapshot(&transaction, &head);
        }
        let snapshot = Snapshot {
            id: Uuid::new_v4().to_string(),
            repository_id: Uuid::new_v4().to_string(),
            parent_id: None,
            manifest_sha256: ArtifactDigest::of(b"[]"),
        };
        transaction.execute(
            "INSERT INTO repositories(id, name, policy, head) VALUES (?1, ?2, ?3, ?4)",
            params![snapshot.repository_id, name, policy_text, snapshot.id],
        )?;
        insert_snapshot(&transaction, &snapshot)?;
        transaction.execute("INSERT INTO audit(actor, action, repository_id, snapshot_id) VALUES ('system', 'create_repository', ?1, ?2)", params![snapshot.repository_id, snapshot.id])?;
        transaction.commit()?;
        Ok(snapshot)
    }

    /// 业务作用：把流式制品保存为不可变对象，仓库可见性仍由后续提交控制。
    /// 参数说明：
    /// - `reader`：原始制品字节流。
    /// - `expected`：调用方静态校验后取得的预期摘要。
    ///
    /// 返回：字节和目录持久化完成的对象；摘要或容量不满足时不产生可见仓库引用。
    pub fn install_object(
        &self,
        reader: &mut dyn Read,
        expected: &ArtifactDigest,
    ) -> Result<StoredObject> {
        Ok(self
            .install_validated(reader, Some(expected), |_, _| Ok(()))?
            .0)
    }

    /// 业务作用：接收 Cargo 原生长度帧中的完整包体，以实际字节计算服务器权威摘要。
    /// 参数说明：
    /// - `reader`：已按协议限定到包体长度的原始流。
    ///
    /// 返回：持久化对象及其 SHA-256；不建立发布引用，后续仍须静态检查归档和依赖闭包。
    pub fn install_cargo_object(&self, reader: &mut dyn Read) -> Result<StoredObject> {
        Ok(self.install_validated(reader, None, |_, _| Ok(()))?.0)
    }

    /// 业务作用：先在不可见暂存上完成协议与归档校验，再安装正式对象并登记存储代次。
    /// 参数说明：
    /// - `reader`：原始完整字节流。
    /// - `expected`：协议可选声明的 SHA-256。
    /// - `validate`：只读暂存文件与实际对象描述的业务校验，失败不安装正式对象。
    ///
    /// 返回：持久对象及校验结果；业务校验失败只清理暂存，安装阶段中断由对象恢复流程处理。
    pub fn install_validated<T>(
        &self,
        reader: &mut dyn Read,
        expected: Option<&ArtifactDigest>,
        validate: impl FnOnce(&mut File, &StoredObject) -> Result<T>,
    ) -> Result<(StoredObject, T)> {
        objects::install_object(
            &self.root,
            reader,
            objects::InstallationPolicy {
                max_bytes: self.options.max_artifact_bytes,
                reserve_free_bytes: self.options.reserve_free_bytes,
                expected: expected.cloned(),
            },
            &self.object_gate,
            validate,
            |expected| {
                let state: Option<String> = self
                    .connection()?
                    .query_row(
                        "SELECT state FROM objects WHERE sha256 = ?1",
                        [expected.as_str()],
                        |row| row.get(0),
                    )
                    .optional()?;
                ensure!(
                    state.as_deref() != Some("deleting"),
                    "object is being deleted"
                );
                Ok(())
            },
            |object| {
                self.connection()?.execute("INSERT INTO objects(sha256, size, installed_at) VALUES (?1, ?2, unixepoch()) ON CONFLICT(sha256) DO UPDATE SET size = excluded.size, generation = CASE WHEN objects.state = 'purged' THEN objects.generation + 1 ELSE objects.generation END, state = 'present', installed_at = unixepoch()", params![object.sha256.as_str(), object.size])?;
                Ok(())
            },
        )
    }

    /// 业务作用：在一个 SQLite 事务中提交完整批次及 HEAD，确保读者不能看到部分组件。
    /// 参数说明：
    /// - `request`：已经完成归档校验与授权的固定批次、预期 HEAD 和幂等身份。
    ///
    /// 返回：提交后的不可变快照；幂等重试返回原结果，冲突或对象不完整时不修改 HEAD。
    pub fn commit(&self, request: &CommitRequest) -> Result<Snapshot> {
        self.commit_inner(request, None, None)
    }

    /// 业务作用：以当前受管 token 权限提交坐标变化，权限撤销与 HEAD 发布使用同一事务顺序。
    /// 参数说明：
    /// - `request`：已经静态校验的完整变更。
    /// - `token`：受管 token 摘要；可信本机或单一部署凭据为 None。
    ///
    /// 返回：账号、令牌和仓库权限仍有效才公开快照。
    pub fn commit_authorized(
        &self,
        request: &CommitRequest,
        token: Option<&ArtifactDigest>,
    ) -> Result<Snapshot> {
        self.commit_inner(request, None, token)
    }

    /// 业务作用：把批次终态与修订、快照和 HEAD 纳入同一提交边界。
    /// 参数说明：
    /// - `request`：已校验且固定的发布输入。
    /// - `batch`：持久批次 ID；独立的内核调用不绑定批次。
    /// - `token`：可选受管凭据摘要，在 HEAD 事务内复验当前权限。
    ///
    /// 返回：整个事务成功后的快照；失败不留下部分发布或假 committed 状态。
    pub(crate) fn commit_inner(
        &self,
        request: &CommitRequest,
        batch: Option<&str>,
        token: Option<&ArtifactDigest>,
    ) -> Result<Snapshot> {
        let _objects = self
            .object_gate
            .lock()
            .map_err(|_| anyhow::anyhow!("object lifecycle unavailable"))?;
        ensure!(
            !request.entries.is_empty() && request.entries.len() <= 4096,
            crate::RepositoryFailure::InvalidRequest("invalid commit size")
        );
        ensure!(
            !request.actor.is_empty() && request.actor.len() <= 256,
            crate::RepositoryFailure::InvalidRequest("invalid actor")
        );
        ensure!(
            !request.idempotency_key.is_empty() && request.idempotency_key.len() <= 256,
            crate::RepositoryFailure::InvalidRequest("invalid idempotency key")
        );
        let mut entries = request.entries.iter().collect::<Vec<_>>();
        entries.sort_by(|left, right| (&left.name, &left.vers).cmp(&(&right.name, &right.vers)));
        let mut coordinates = BTreeSet::new();
        let mut object_sizes = BTreeMap::new();
        for entry in &entries {
            entry.validate()?;
            ensure!(
                coordinates.insert((canonical_name(&entry.name), &entry.vers)),
                "duplicate commit coordinate"
            );
            let object = objects::verify_object(&self.root, &entry.cksum, None)?;
            object_sizes.insert(entry.cksum.clone(), object.size);
        }
        let request_digest = ArtifactDigest::of(&serde_json::to_vec(&(
            &request.repository,
            &request.expected_head,
            &request.actor,
            &entries,
        ))?);
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (repository_id, policy, head): (String, String, String) = transaction.query_row(
            "SELECT id, policy, head FROM repositories WHERE name = ?1 OR id = ?1",
            [&request.repository],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let write_scope = if let Some(token) = token {
            // 发布时重新读取权威账号与 token 状态；入站认证不能跨越撤销后继续产生新 HEAD。
            let identity = crate::security::token_identity_on(&transaction, token)?.ok_or(
                crate::RepositoryFailure::Forbidden("publish token permission revoked"),
            )?;
            ensure!(
                identity.subject == request.actor
                    && identity.can_publish
                    && identity
                        .repositories
                        .iter()
                        .any(|grant| grant == "*" || grant == &repository_id),
                crate::RepositoryFailure::Forbidden("publish token permission denied")
            );
            let (overwrite, crates): (bool, String) = transaction.query_row(
                "SELECT allow_overwrite, publish_crates FROM access_tokens WHERE digest = ?1",
                [token.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            Some((overwrite, serde_json::from_str::<Vec<String>>(&crates)?))
        } else {
            None
        };
        for digest in object_sizes.keys() {
            let state: String = transaction.query_row(
                "SELECT state FROM objects WHERE sha256 = ?1",
                [digest.as_str()],
                |row| row.get(0),
            )?;
            // 清除权已经提交的存储代次不得再接收引用，发布者必须等后续合法安装的新代次。
            ensure!(
                state == "present",
                crate::RepositoryFailure::Conflict(
                    "object is being deleted or is unavailable".to_owned()
                )
            );
        }
        let existing: Option<(String, String)> = transaction.query_row("SELECT request_sha256, snapshot_id FROM commits WHERE repository_id = ?1 AND idempotency_key = ?2", params![repository_id, request.idempotency_key], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
        if let Some((digest, snapshot)) = existing {
            ensure!(
                digest == request_digest.as_str(),
                crate::RepositoryFailure::Conflict(
                    "idempotency key was used for different content".to_owned()
                )
            );
            return read_snapshot(&transaction, &snapshot);
        }
        // 在持有写事务时复验 HEAD，上传期间发生的其他提交必须形成可见冲突，不能覆盖它。
        ensure!(
            head == request.expected_head,
            crate::RepositoryFailure::Conflict("repository HEAD changed".to_owned())
        );
        let parent = read_snapshot(&transaction, &head)?;
        let mut changes = Vec::new();
        for entry in entries {
            let canonical = canonical_name(&entry.name);
            let owner: Option<String> = transaction
                .query_row(
                    "SELECT name FROM crate_names WHERE repository_id = ?1 AND canonical = ?2",
                    params![repository_id, canonical],
                    |row| row.get(0),
                )
                .optional()?;
            ensure!(
                owner.as_deref().is_none_or(|name| name == entry.name),
                crate::RepositoryFailure::Conflict(
                    "crate name conflicts with persisted ownership".to_owned()
                )
            );
            let current = read_entry(
                &transaction,
                &head,
                &entry.name,
                &entry.vers,
                self.options.max_snapshot_depth,
            )?;
            if let Some((overwrite, crates)) = &write_scope {
                ensure!(
                    crates.is_empty() || crates.contains(&entry.name),
                    crate::RepositoryFailure::Forbidden("crate publish scope permission denied")
                );
                if current.as_ref().is_none_or(|current| current != entry) {
                    let existed: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM revisions WHERE repository_id = ?1 AND name = ?2 AND version = ?3)", params![repository_id, entry.name, entry.vers], |row| row.get(0))?;
                    // 历史坐标也不是首次发布，逻辑移除不能让普通发布权限隐含获得覆盖或重新开放权。
                    ensure!(
                        !existed || *overwrite,
                        crate::RepositoryFailure::Forbidden(
                            "coordinate overwrite permission denied"
                        )
                    );
                }
            }
            if current.is_none() && policy == "immutable" {
                let previous: Option<String> = transaction.query_row("SELECT entry_json FROM revisions WHERE repository_id = ?1 AND name = ?2 AND version = ?3 LIMIT 1", params![repository_id, entry.name, entry.vers], |row| row.get(0)).optional()?;
                if let Some(previous) = previous {
                    let mut previous: IndexEntry = serde_json::from_str(&previous)?;
                    let mut candidate = (*entry).clone();
                    previous.yanked = false;
                    candidate.yanked = false;
                    // 逻辑移除不撤销 immutable 承诺，重新发布仍必须保持原始字节与依赖语义。
                    ensure!(
                        previous == candidate,
                        crate::RepositoryFailure::Conflict(
                            "immutable repository coordinate already exists".to_owned()
                        )
                    );
                }
            }
            if let Some(current) = current {
                if current == *entry {
                    continue;
                }
                let mut original = current.clone();
                let mut candidate = (*entry).clone();
                original.yanked = false;
                candidate.yanked = false;
                // immutable 保持包体和依赖语义，Cargo 的 yank 标记只改变新解析的可选性。
                ensure!(
                    policy == "mutable" || original == candidate,
                    crate::RepositoryFailure::Conflict(
                        "immutable repository coordinate already exists".to_owned()
                    )
                );
            }
            transaction.execute("INSERT OR IGNORE INTO crate_names(repository_id, canonical, name) VALUES (?1, ?2, ?3)", params![repository_id, canonical, entry.name])?;
            transaction.execute(
                "INSERT OR IGNORE INTO objects(sha256, size) VALUES (?1, ?2)",
                params![entry.cksum.as_str(), object_sizes[&entry.cksum]],
            )?;
            let revision_id = Uuid::new_v4().to_string();
            let metadata = entry.metadata_digest()?;
            transaction.execute("INSERT INTO revisions(id, repository_id, name, version, sha256, metadata_sha256, entry_json, actor) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)", params![revision_id, repository_id, entry.name, entry.vers, entry.cksum.as_str(), metadata.as_str(), serde_json::to_string(entry)?, request.actor])?;
            changes.push((
                entry.name.clone(),
                entry.vers.clone(),
                revision_id,
                metadata,
            ));
        }
        let snapshot = if changes.is_empty() {
            parent
        } else {
            let snapshot = Snapshot {
                id: Uuid::new_v4().to_string(),
                repository_id: repository_id.clone(),
                parent_id: Some(head.clone()),
                manifest_sha256: ArtifactDigest::of(&serde_json::to_vec(&(
                    repository_id.clone(),
                    parent.manifest_sha256,
                    &changes,
                ))?),
            };
            insert_snapshot(&transaction, &snapshot)?;
            for (name, version, revision_id, _) in changes {
                transaction.execute("INSERT INTO snapshot_changes(snapshot_id, name, version, revision_id) VALUES (?1, ?2, ?3, ?4)", params![snapshot.id, name, version, revision_id])?;
            }
            // 周期性保存等价读取检查点，把长期发布的父链成本限制在固定窗口内。
            materialize_checkpoint(&transaction, &snapshot.id, self.options.max_snapshot_depth)?;
            // 所有修订与快照变更已经进入同一事务，最后才发布新的当前入口。
            transaction.execute(
                "UPDATE repositories SET head = ?1 WHERE id = ?2 AND head = ?3",
                params![snapshot.id, repository_id, head],
            )?;
            snapshot
        };
        transaction.execute("INSERT INTO commits(repository_id, idempotency_key, request_sha256, snapshot_id, actor) VALUES (?1, ?2, ?3, ?4, ?5)", params![repository_id, request.idempotency_key, request_digest.as_str(), snapshot.id, request.actor])?;
        transaction.execute("INSERT INTO audit(actor, action, repository_id, snapshot_id) VALUES (?1, 'commit', ?2, ?3)", params![request.actor, repository_id, snapshot.id])?;
        if let Some(batch) = batch {
            let changed = transaction.execute("UPDATE publish_batches SET state = 'committed', snapshot_id = ?1 WHERE id = ?2 AND repository_id = ?3 AND actor = ?4 AND expected_head = ?5 AND state = 'validating'", params![snapshot.id, batch, repository_id, request.actor, request.expected_head])?;
            ensure!(
                changed == 1,
                crate::RepositoryFailure::Conflict("publish batch authority changed".to_owned())
            );
        }
        transaction.commit()?;
        // 仅在数据库确认提交后回读快照，调用方才能向发布者报告成功。
        read_snapshot(&connection, &snapshot.id)
    }

    /// 业务作用：读取一个仓库当前已提交的快照入口。
    /// 参数说明：
    /// - `repository`：逻辑仓库名。
    ///
    /// 返回：同一查询观察到的 HEAD 描述；不存在时返回错误。
    pub fn head(&self, repository: &str) -> Result<Snapshot> {
        let connection = self.connection()?;
        let head: String = connection.query_row(
            "SELECT head FROM repositories WHERE name = ?1 OR id = ?1",
            [repository],
            |row| row.get(0),
        )?;
        read_snapshot(&connection, &head)
    }

    /// 业务作用：在固定快照中读取某个坐标，不受后续同版本发布影响。
    /// 参数说明：
    /// - `snapshot`：不可变快照 ID。
    /// - `name`：实际 package 名称。
    /// - `version`：精确 SemVer 文本。
    ///
    /// 返回：该快照可见的完整索引条目；坐标不存在返回 None，链异常或超预算返回错误。
    pub fn entry(&self, snapshot: &str, name: &str, version: &str) -> Result<Option<IndexEntry>> {
        validate_name(name)?;
        let connection = self.connection()?;
        read_entry(
            &connection,
            snapshot,
            name,
            version,
            self.options.max_snapshot_depth,
        )
    }

    /// 业务作用：列出固定服务下的仓库身份，调用者据此执行仓库授权与地址映射。
    /// 参数说明: 无。
    ///
    /// 返回：按名称排序的仓库描述，不隐式合并同名包来源。
    pub fn repositories(&self) -> Result<Vec<RepositoryInfo>> {
        let connection = self.connection()?;
        let mut statement =
            connection.prepare("SELECT id, name, policy, head FROM repositories ORDER BY name")?;
        let rows = statement.query_map([], |row| {
            let policy: String = row.get(2)?;
            Ok(RepositoryInfo {
                id: row.get(0)?,
                name: row.get(1)?,
                policy: if policy == "mutable" {
                    RepositoryPolicy::Mutable
                } else {
                    RepositoryPolicy::Immutable
                },
                head: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// 业务作用：复验固定快照属于请求仓库，防止跨仓库 UUID 被用于读取其他来源。
    /// 参数说明：
    /// - `repository`：仓库名称或稳定 UUID。
    /// - `snapshot`：期望读取的不可变快照。
    ///
    /// 返回：归属一致的快照描述；不存在或串仓时拒绝读取。
    pub fn snapshot(&self, repository: &str, snapshot: &str) -> Result<Snapshot> {
        let connection = self.connection()?;
        let repository_id: String = connection.query_row(
            "SELECT id FROM repositories WHERE name = ?1 OR id = ?1",
            [repository],
            |row| row.get(0),
        )?;
        let snapshot = read_snapshot(&connection, snapshot)?;
        ensure!(
            snapshot.repository_id == repository_id,
            crate::RepositoryFailure::Forbidden("snapshot belongs to a different repository")
        );
        Ok(snapshot)
    }

    /// 业务作用：为 sparse index 生成目标快照下一个包的全部可见版本。
    /// 参数说明：
    /// - `snapshot`：不可变快照 ID。
    /// - `name`：实际 package 名称。
    ///
    /// 返回：按 SemVer 排序的完整索引条目；找不到包时返回空集合。
    pub fn index_entries(&self, snapshot: &str, name: &str) -> Result<Vec<IndexEntry>> {
        validate_name(name)?;
        let connection = self.connection()?;
        let repository_id = read_snapshot(&connection, snapshot)?.repository_id;
        let actual_name: Option<String> = connection
            .query_row(
                "SELECT name FROM crate_names WHERE repository_id = ?1 AND canonical = ?2",
                params![repository_id, canonical_name(name)],
                |row| row.get(0),
            )
            .optional()?;
        let Some(name) = actual_name else {
            return Ok(Vec::new());
        };
        let mut statement = connection.prepare(
            "SELECT DISTINCT version FROM revisions WHERE repository_id = ?1 AND name = ?2",
        )?;
        let versions = statement
            .query_map(params![repository_id, name], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut entries = Vec::new();
        for version in versions {
            if let Some(entry) = read_entry(
                &connection,
                snapshot,
                &name,
                &version,
                self.options.max_snapshot_depth,
            )? {
                entries.push(entry);
            }
        }
        entries.sort_by_cached_key(|entry| {
            semver::Version::parse(&entry.vers).expect("validated index version")
        });
        Ok(entries)
    }

    /// 业务作用：在下载前证明该对象曾作为指定仓库的正式修订提交。
    /// 参数说明：
    /// - `repository`：已授权的仓库名称或 UUID。
    /// - `digest`：请求对象的精确摘要。
    ///
    /// 返回：该仓库拥有正式读取引用时为 true；不开放未提交的暂存对象。
    pub fn contains_object(&self, repository: &str, digest: &ArtifactDigest) -> Result<bool> {
        let connection = self.connection()?;
        // 摘要路由表达同仓任一有效正式引用；一个历史撤销不能越过仍有效的共享引用。
        let available: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM revisions v JOIN repositories r ON r.id = v.repository_id
             WHERE (r.name = ?1 OR r.id = ?1) AND v.sha256 = ?2
               AND NOT EXISTS(SELECT 1 FROM artifact_access_revocations a WHERE a.revision_id = v.id))",
            params![repository, digest.as_str()],
            |row| row.get(0),
        )?;
        if available {
            return Ok(true);
        }
        let revoked: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM artifact_access_revocations a JOIN revisions v ON v.id = a.revision_id JOIN repositories r ON r.id = v.repository_id WHERE (r.name = ?1 OR r.id = ?1) AND v.sha256 = ?2)", params![repository, digest.as_str()], |row| row.get(0))?;
        if revoked {
            return Err(crate::ArtifactGone.into());
        }
        Ok(false)
    }

    /// 业务作用：在同一对象门禁内核对仓库引用并登记读取，避免撤销与打开之间出现空窗。
    /// 参数说明：
    /// - `repository`：已经授权的仓库名称或 UUID。
    /// - `digest`：请求的原始内容摘要。
    ///
    /// 返回：同仓仍有有效正式引用时返回受保护读取句柄；仅剩撤销引用返回 Gone，未知对象拒绝。
    pub fn open_repository_object(
        &self,
        repository: &str,
        digest: &ArtifactDigest,
    ) -> Result<crate::ObjectReader> {
        let _gate = self
            .object_gate
            .lock()
            .map_err(|_| anyhow::anyhow!("object lifecycle unavailable"))?;
        // 读取登记与撤销/清除共用权威；一旦登记，后续清除必须观察活动读者。
        ensure!(
            self.contains_object(repository, digest)?,
            crate::RepositoryFailure::NotFound("unknown object")
        );
        self.open_reader(digest)
    }

    /// 业务作用：提供已经持久化且再次通过完整性校验的对象读取句柄。
    /// 参数说明：
    /// - `digest`：调用方已根据仓库读取权限确认可访问的对象摘要。
    ///
    /// 返回：指向精确内容的只读文件；授权由调用者完成，缺失或损坏不替换成其他修订。
    pub fn open_object(&self, digest: &ArtifactDigest) -> Result<crate::ObjectReader> {
        let _gate = self
            .object_gate
            .lock()
            .map_err(|_| anyhow::anyhow!("object lifecycle unavailable"))?;
        self.open_reader(digest)
    }

    /// 业务作用：按固定快照的真实修订裁决下载，撤销状态不因同摘要新上传而消失。
    /// 参数说明：
    /// - `repository`：已授权仓库。
    /// - `snapshot`：下载模板绑定的不可变快照。
    /// - `coordinate`：索引行中的精确坐标。
    /// - `digest`：索引行声明的原始对象摘要。
    ///
    /// 返回：未撤销修订的活动读取保护；已清除历史返回 ArtifactGone。
    pub fn open_snapshot_object(
        &self,
        repository: &str,
        snapshot: &str,
        coordinate: &naxus_core::Coordinate,
        digest: &ArtifactDigest,
    ) -> Result<crate::ObjectReader> {
        let _gate = self
            .object_gate
            .lock()
            .map_err(|_| anyhow::anyhow!("object lifecycle unavailable"))?;
        {
            let connection = self.connection()?;
            let allowed: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM snapshots s JOIN repositories r ON r.id = s.repository_id WHERE s.id = ?1 AND (r.id = ?2 OR r.name = ?2))", params![snapshot, repository], |row| row.get(0))?;
            ensure!(
                allowed,
                crate::RepositoryFailure::Forbidden("snapshot belongs to a different repository")
            );
            let revision = crate::resolution::revision_at(
                &connection,
                snapshot,
                coordinate,
                self.options.max_snapshot_depth,
            )?
            .ok_or_else(|| anyhow::anyhow!(crate::RepositoryFailure::NotFound("unknown object")))?;
            ensure!(
                revision.entry.cksum == *digest,
                "snapshot object checksum differs"
            );
            let revoked: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM artifact_access_revocations WHERE revision_id = ?1)",
                [revision.id],
                |row| row.get(0),
            )?;
            if revoked {
                return Err(crate::ArtifactGone.into());
            }
        }
        self.open_reader(digest)
    }

    /// 业务作用：在调用方持有生命周期门禁时打开并登记活动读取，消除校验与清除之间的窗口。
    /// 参数说明：
    /// - `digest`：已经完成来源与修订授权的精确摘要。
    ///
    /// 返回：文件句柄与数据卷所有权共同持有的读取守卫。
    fn open_reader(&self, digest: &ArtifactDigest) -> Result<crate::ObjectReader> {
        let state: Option<String> = self
            .connection()?
            .query_row(
                "SELECT state FROM objects WHERE sha256 = ?1",
                [digest.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        ensure!(
            state.as_deref().is_none_or(|state| state == "present"),
            "object is unavailable in this storage generation"
        );
        objects::verify_object(&self.root, digest, None)?;
        let file = File::open(objects::object_path(&self.root, digest))?;
        let mut readers = self
            .readers
            .lock()
            .map_err(|_| anyhow::anyhow!("object readers unavailable"))?;
        *readers.entry(digest.clone()).or_default() += 1;
        Ok(crate::ObjectReader {
            file,
            lease: crate::ObjectReadLease {
                digest: digest.clone(),
                readers: self.readers.clone(),
                _instance: self._instance_lock.clone(),
            },
        })
    }

    /// 业务作用：核对 SQLite 引用及所有已提交对象，决定数据卷能否安全开放读取。
    /// 参数说明: 无。
    ///
    /// 返回：引用完整且所有受保护对象摘要正确时成功；未经授权的缺失或损坏拒绝就绪。
    pub fn check_integrity(&self) -> Result<()> {
        let connection = self.connection()?;
        let status: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        ensure!(status == "ok", "repository database integrity check failed");
        let foreign_failure: Option<String> = connection
            .query_row("PRAGMA foreign_key_check", [], |row| row.get(0))
            .optional()?;
        ensure!(
            foreign_failure.is_none(),
            "repository foreign key check failed"
        );
        let invalid_scope: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM repositories r LEFT JOIN snapshots s ON s.id = r.head WHERE s.id IS NULL OR s.repository_id <> r.id) OR EXISTS(SELECT 1 FROM snapshots s JOIN snapshots p ON p.id = s.parent_id WHERE s.repository_id <> p.repository_id) OR EXISTS(SELECT 1 FROM snapshot_changes c JOIN snapshots s ON s.id = c.snapshot_id JOIN revisions r ON r.id = c.revision_id WHERE r.repository_id <> s.repository_id OR r.name <> c.name OR r.version <> c.version) OR EXISTS(SELECT 1 FROM checkpoint_entries c JOIN snapshots s ON s.id = c.snapshot_id JOIN revisions r ON r.id = c.revision_id WHERE r.repository_id <> s.repository_id OR r.name <> c.name OR r.version <> c.version)",
            [],
            |row| row.get(0),
        )?;
        // SQL 外键只证明记录存在，归属和坐标仍须复验，避免合法行被错误拼成跨仓库读取图。
        ensure!(!invalid_scope, "repository reference scope mismatch");
        let invalid_lifecycle: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM revisions r JOIN objects o ON o.sha256 = r.sha256 WHERE o.state != 'present' AND NOT EXISTS(SELECT 1 FROM artifact_access_revocations a WHERE a.revision_id = r.id)) OR EXISTS(SELECT 1 FROM proxy_objects p JOIN objects o ON o.sha256 = p.digest WHERE o.state != 'present') OR EXISTS(SELECT 1 FROM proxy_materializations m JOIN materialization_items i ON i.materialization_id = m.id WHERE m.valid = 1 AND NOT EXISTS(SELECT 1 FROM proxy_objects p JOIN objects o ON o.sha256 = p.digest WHERE p.proxy_id = m.proxy_id AND p.digest = i.digest AND o.state = 'present'))", [], |row| row.get(0))?;
        ensure!(
            !invalid_lifecycle,
            "live object references differ from storage lifecycle"
        );
        let mut statement =
            connection.prepare("SELECT sha256, size, state, generation FROM objects")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, u64>(3)?,
            ))
        })?;
        for row in rows {
            let (digest, size, state, generation) = row?;
            if state == "present" {
                objects::verify_object(&self.root, &digest.parse()?, Some(size))?;
            } else {
                let authorized: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM deletion_items i JOIN object_deletions d ON d.id = i.deletion_id WHERE i.digest = ?1 AND i.generation = ?2 AND i.state = 'purged' AND d.state = 'completed')", params![digest, generation], |row| row.get(0))?;
                // 文件缺失只有在匹配的代次清除任务完成后才是合法状态，不能掩盖未经授权的数据丢失。
                ensure!(
                    state == "purged" && authorized,
                    "object lifecycle has no completed deletion evidence"
                );
            }
        }
        let mut statement = connection
            .prepare("SELECT name, version, sha256, metadata_sha256, entry_json FROM revisions")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (name, version, digest, metadata_digest, json) = row?;
            let entry: IndexEntry = serde_json::from_str(&json)?;
            entry.validate()?;
            ensure!(
                entry.name == name && entry.vers == version && entry.cksum.as_str() == digest,
                "revision index identity mismatch"
            );
            ensure!(
                entry.metadata_digest()?.as_str() == metadata_digest,
                "revision metadata checksum mismatch"
            );
        }
        Ok(())
    }

    /// 业务作用：为同步存储操作取得唯一 SQLite 连接，避免跨请求交错使用事务。
    /// 参数说明: 无。
    ///
    /// 返回：受互斥保护的连接；先前持有者异常时拒绝继续操作。
    pub(crate) fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow::anyhow!("repository connection is unavailable"))
    }
}

/// 业务作用：在调用者事务内写入不可变快照记录。
/// 参数说明：
/// - `connection`：当前数据库事务或连接。
/// - `snapshot`：尚未插入的快照身份及父关系。
///
/// 返回：写入成功；重复身份或引用冲突返回错误，不覆盖原有记录。
pub(crate) fn insert_snapshot(connection: &Connection, snapshot: &Snapshot) -> Result<()> {
    connection.execute("INSERT INTO snapshots(id, repository_id, parent_id, manifest_sha256) VALUES (?1, ?2, ?3, ?4)", params![snapshot.id, snapshot.repository_id, snapshot.parent_id, snapshot.manifest_sha256.as_str()])?;
    Ok(())
}

/// 业务作用：从固定快照读取已持久化的身份与内容指纹。
/// 参数说明：
/// - `connection`：当前读取视图。
/// - `id`：不可变快照 ID。
///
/// 返回：经过摘要格式校验的描述；不存在或内容损坏时返回错误。
pub(crate) fn read_snapshot(connection: &Connection, id: &str) -> Result<Snapshot> {
    let (repository_id, parent_id, digest): (String, Option<String>, String) = connection
        .query_row(
            "SELECT repository_id, parent_id, manifest_sha256 FROM snapshots WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    Ok(Snapshot {
        id: id.to_owned(),
        repository_id,
        parent_id,
        manifest_sha256: digest.parse()?,
    })
}

/// 业务作用：沿不可变父链查找最近的坐标变更，并阻止循环或跨仓库历史串接。
/// 参数说明：
/// - `connection`：当前一致读取视图。
/// - `snapshot`：目标快照 ID。
/// - `name`：实际包名。
/// - `version`：精确版本。
/// - `max_depth`：单次读取允许的最大父链长度。
///
/// 返回：最近条目或 None；循环、错误归属及超预算不解释为坐标不存在。
pub(crate) fn read_entry(
    connection: &Connection,
    snapshot: &str,
    name: &str,
    version: &str,
    max_depth: usize,
) -> Result<Option<IndexEntry>> {
    let mut current = Some(snapshot.to_owned());
    let repository_id = read_snapshot(connection, snapshot)?.repository_id;
    let mut visited = BTreeSet::new();
    while let Some(id) = current {
        ensure!(
            visited.len() < max_depth && visited.insert(id.clone()),
            "snapshot lineage exceeds limit or contains a cycle"
        );
        let snapshot = read_snapshot(connection, &id)?;
        ensure!(
            snapshot.repository_id == repository_id,
            "snapshot lineage crosses repositories"
        );
        let removed: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM snapshot_removals WHERE snapshot_id = ?1 AND name = ?2 AND version = ?3)", params![id, name, version], |row| row.get(0))?;
        if removed {
            return Ok(None);
        }
        let json: Option<String> = connection.query_row("SELECT r.entry_json FROM snapshot_changes c JOIN revisions r ON r.id = c.revision_id WHERE c.snapshot_id = ?1 AND c.name = ?2 AND c.version = ?3 AND r.repository_id = ?4", params![id, name, version, repository_id], |row| row.get(0)).optional()?;
        if let Some(json) = json {
            let entry: IndexEntry = serde_json::from_str(&json)?;
            entry.validate()?;
            ensure!(
                entry.name == name && entry.vers == version,
                "revision coordinate differs from snapshot reference"
            );
            return Ok(Some(entry));
        }
        let checkpoint: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM snapshot_checkpoints WHERE snapshot_id = ?1)",
            [&id],
            |row| row.get(0),
        )?;
        if checkpoint {
            let json: Option<String> = connection.query_row("SELECT r.entry_json FROM checkpoint_entries c JOIN revisions r ON r.id = c.revision_id WHERE c.snapshot_id = ?1 AND c.name = ?2 AND c.version = ?3 AND r.repository_id = ?4", params![id, name, version, repository_id], |row| row.get(0)).optional()?;
            return json
                .map(|json| {
                    let entry: IndexEntry = serde_json::from_str(&json)?;
                    entry.validate()?;
                    ensure!(
                        entry.name == name && entry.vers == version,
                        "checkpoint coordinate mismatch"
                    );
                    Ok(entry)
                })
                .transpose();
        }
        current = snapshot.parent_id;
    }
    if visited.is_empty() {
        bail!("snapshot does not exist");
    }
    Ok(None)
}

/// 业务作用：把父链上的最近变更折叠为等价的不可变读取检查点，避免持续发布积累无界查询成本。
/// 参数说明：
/// - `connection`：尚未发布 HEAD 的提交事务。
/// - `snapshot`：本次新建快照。
/// - `max_depth`：配置允许的读取窗口上限。
///
/// 返回：未达到阈值时不写检查点；达到阈值时完整保存可见坐标后才登记完成标记。
pub(crate) fn materialize_checkpoint(
    connection: &Connection,
    snapshot: &str,
    max_depth: usize,
) -> Result<()> {
    let threshold = max_depth.min(32);
    let mut current = Some(snapshot.to_owned());
    let mut visible = BTreeMap::new();
    let mut depth = 0;
    while let Some(id) = current {
        depth += 1;
        let mut statement = connection
            .prepare("SELECT name, version FROM snapshot_removals WHERE snapshot_id = ?1")?;
        let removals = statement.query_map([&id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for removal in removals {
            visible.entry(removal?).or_insert(None);
        }
        let mut statement = connection.prepare(
            "SELECT name, version, revision_id FROM snapshot_changes WHERE snapshot_id = ?1",
        )?;
        let changes = statement.query_map([&id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for change in changes {
            let (name, version, revision) = change?;
            visible.entry((name, version)).or_insert(Some(revision));
        }
        let checkpoint: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM snapshot_checkpoints WHERE snapshot_id = ?1)",
            [&id],
            |row| row.get(0),
        )?;
        if checkpoint {
            if depth < threshold {
                return Ok(());
            }
            let mut statement = connection.prepare(
                "SELECT name, version, revision_id FROM checkpoint_entries WHERE snapshot_id = ?1",
            )?;
            let rows = statement.query_map([&id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (name, version, revision) = row?;
                visible.entry((name, version)).or_insert(Some(revision));
            }
            break;
        }
        current = read_snapshot(connection, &id)?.parent_id;
    }
    if depth < threshold {
        return Ok(());
    }
    for ((name, version), revision) in visible {
        if let Some(revision) = revision {
            connection.execute("INSERT INTO checkpoint_entries(snapshot_id, name, version, revision_id) VALUES (?1, ?2, ?3, ?4)", params![snapshot, name, version, revision])?;
        }
    }
    // 标记只在完整映射进入同一事务后写入；读者不能把半份检查点当成完整快照。
    connection.execute(
        "INSERT INTO snapshot_checkpoints(snapshot_id) VALUES (?1)",
        [snapshot],
    )?;
    Ok(())
}
