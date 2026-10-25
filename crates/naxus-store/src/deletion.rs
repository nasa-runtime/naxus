use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};

use anyhow::{ensure, Context, Result};
use naxus_core::{ArtifactDigest, Coordinate};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{objects, resolution::revision_at, Store};

/// 界面选择的精确物理清除范围，公开缓存清理不隐含封禁。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeletionScope {
    pub kind: String,
    pub repository: String,
    pub name: String,
    pub version: Option<String>,
    #[serde(default)]
    pub all_versions: bool,
    pub revision: Option<String>,
    #[serde(default)]
    pub revoke_history: bool,
}

/// 一个精确存储代次的清理影响，不按重复引用累加空间。
#[derive(Clone, Serialize, Deserialize)]
pub struct DeletionObject {
    pub digest: ArtifactDigest,
    pub generation: u64,
    pub size: u64,
    pub state: String,
    pub shared: bool,
    pub revisions: Vec<String>,
}

/// 带保护阻挡、代次和可释放字节的物理清除预览。
#[derive(Serialize, Deserialize)]
pub struct DeletionPreview {
    pub id: String,
    pub scope: DeletionScope,
    pub objects: Vec<DeletionObject>,
    pub blockers: Vec<String>,
    pub reclaimable_bytes: u64,
    pub fingerprint: ArtifactDigest,
    pub expires_at: i64,
    pub refetch_allowed: bool,
}

/// 持久清除任务，只在实际文件处理完成后记录释放空间。
#[derive(Serialize, Deserialize)]
pub struct DeletionTask {
    pub id: String,
    pub state: String,
    pub released_bytes: u64,
    pub scope: DeletionScope,
    pub attempts: u64,
    pub next_attempt_at: i64,
    pub last_error_code: Option<String>,
}

struct ScopeIdentity {
    repository_id: String,
    revisions: BTreeSet<String>,
}

impl Store {
    /// 业务作用：提供有界的持久删除任务列表，供受管工作者与管理界面查询。
    /// 参数说明: 无。
    ///
    /// 返回：最近二百个任务，不从文件系统推断任务结果。
    pub fn deletion_tasks(&self) -> Result<Vec<DeletionTask>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id FROM object_deletions ORDER BY created_at DESC, rowid DESC LIMIT 200",
        )?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter().map(|id| read_task(&connection, id)).collect()
    }

    /// 业务作用：为实例唯一受管工作者读取下一项持久任务，客户端断开不影响继续执行。
    /// 参数说明: 无。
    ///
    /// 返回：最早仍未完成的任务 UUID，没有任务则为 None。
    pub fn pending_deletion(&self) -> Result<Option<String>> {
        Ok(self.connection()?.query_row("SELECT id FROM object_deletions WHERE state IN ('accepted', 'running') AND next_attempt_at <= unixepoch() ORDER BY next_attempt_at, created_at, rowid LIMIT 1", [], |row| row.get(0)).optional()?)
    }

    /// 业务作用：为未完成的清除保留失败分类并退避，避免一项故障永久阻塞后续任务。
    /// 参数说明：
    /// - `id`：已受理的精确任务。
    /// - `error`：本次文件或持久化动作的失败，不保存未经处理的错误正文。
    ///
    /// 返回：记录次数、固定错误类别及最长一小时退避；已经完成的任务不被改回运行态。
    pub fn defer_deletion(&self, id: &str, error: &anyhow::Error) -> Result<()> {
        let code = if error.chain().any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
        }) {
            "filesystem_permission_denied"
        } else if error.to_string().contains("checksum") {
            "object_checksum_mismatch"
        } else if error.to_string().contains("generation") {
            "object_generation_mismatch"
        } else {
            "storage_operation_failed"
        };
        self.connection()?.execute("UPDATE object_deletions SET attempts = attempts + 1, last_error_code = ?1, next_attempt_at = unixepoch() + min(3600, 5 * (1 << min(attempts, 10))) WHERE id = ?2 AND state IN ('accepted', 'running')", params![code, id])?;
        Ok(())
    }

    /// 业务作用：运维明确请求立即重试已经持久受理的清除，不创建新的对象删除权威。
    /// 参数说明：
    /// - `actor`：请求认证时固定的浏览器身份及账号代次。
    /// - `id`：原有任务 UUID。
    ///
    /// 返回：保留原代次与进度，仅解除退避；已完成任务保持终态。
    pub fn retry_deletion(&self, actor: &crate::AccessIdentity, id: &str) -> Result<DeletionTask> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actor = crate::security::browser_actor(&transaction, actor)?;
        let task = read_task(&transaction, id)?;
        if task.scope.kind == "unpublished" {
            let grants: String = transaction.query_row("SELECT repositories FROM admin_users WHERE id = ?1 AND role IN ('admin', 'operator') AND enabled = 1 AND must_change = 0", [actor], |row| row.get(0)).optional()?.context(crate::RepositoryFailure::Forbidden("maintenance retry permission denied"))?;
            ensure!(
                serde_json::from_str::<Vec<String>>(&grants)?
                    .iter()
                    .any(|grant| grant == "*"),
                crate::RepositoryFailure::Forbidden("maintenance retry permission denied")
            );
        } else {
            deletion_permission(&transaction, actor, &task.scope)?;
        }
        transaction.execute("UPDATE object_deletions SET next_attempt_at = 0 WHERE id = ?1 AND state IN ('accepted', 'running')", [id])?;
        transaction.execute(
            "INSERT INTO security_audit(actor, action, target) VALUES (?1, 'retry-clear', ?2)",
            params![actor, id],
        )?;
        transaction.commit()?;
        read_task(&connection, id)
    }
    /// 业务作用：为物理清除计算精确代次、活跃引用和共享阻挡，不立即改变可用性。
    /// 参数说明：
    /// - `actor`：请求认证时固定的浏览器身份及账号代次。
    /// - `scope`：界面显式选择的来源、坐标与历史撤销意图。
    ///
    /// 返回：五分钟内有效的影响预览；存在保护时仍展示原因，但不允许提交。
    pub fn preview_deletion(
        &self,
        actor: &crate::AccessIdentity,
        scope: &DeletionScope,
    ) -> Result<DeletionPreview> {
        let _gate = self
            .object_gate
            .lock()
            .map_err(|_| anyhow::anyhow!("object lifecycle unavailable"))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actor = crate::security::browser_actor(&transaction, actor)?;
        let mut preview = self.deletion_view(&transaction, actor, scope)?;
        transaction.execute("DELETE FROM deletion_previews WHERE id IN (SELECT id FROM deletion_previews WHERE state = 'prepared' AND expires_at <= unixepoch() LIMIT 256)", [])?;
        let count: u64 = transaction.query_row(
            "SELECT count(*) FROM deletion_previews WHERE actor = ?1 AND state = 'prepared'",
            [actor],
            |row| row.get(0),
        )?;
        ensure!(
            count < 128,
            crate::ResourceLimitExceeded("deletion preview budget exceeded")
        );
        preview.id = Uuid::new_v4().to_string();
        transaction.execute("INSERT INTO deletion_previews(id, actor, scope, fingerprint, expires_at) VALUES (?1, ?2, ?3, ?4, ?5)", params![preview.id, actor, serde_json::to_string(scope)?, preview.fingerprint.as_str(), preview.expires_at])?;
        transaction.commit()?;
        Ok(preview)
    }

    /// 业务作用：复验预览、权限与全部保护后，先提交删除权威再允许处理正式文件。
    /// 参数说明：
    /// - `actor`：请求认证时固定的浏览器身份及账号代次。
    /// - `preview_id`：已显示并确认的预览身份。
    /// - `fingerprint`：用户确认的影响范围指纹。
    ///
    /// 返回：持久任务身份；重复提交同一预览返回原任务，保护引用阻挡时返回状态冲突，不撤销历史或累加空间。
    pub fn accept_deletion(
        &self,
        actor: &crate::AccessIdentity,
        preview_id: &str,
        fingerprint: &ArtifactDigest,
    ) -> Result<DeletionTask> {
        let _gate = self
            .object_gate
            .lock()
            .map_err(|_| anyhow::anyhow!("object lifecycle unavailable"))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actor = crate::security::browser_actor(&transaction, actor)?;
        let (scope, digest, active, task): (String, String, bool, Option<String>) = transaction.query_row("SELECT scope, fingerprint, expires_at > unixepoch(), task_id FROM deletion_previews WHERE id = ?1 AND actor = ?2", params![preview_id, actor], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?;
        let scope: DeletionScope = serde_json::from_str(&scope)?;
        let identity = deletion_permission(&transaction, actor, &scope)?;
        ensure!(
            digest == fingerprint.as_str(),
            crate::RepositoryFailure::Conflict("deletion preview fingerprint changed".to_owned())
        );
        if let Some(task) = task {
            return read_task(&transaction, &task);
        }
        ensure!(
            active,
            crate::RepositoryFailure::Conflict("deletion preview expired".to_owned())
        );
        let view = self.deletion_view(&transaction, actor, &scope)?;
        // 活动读者、租约、HEAD 或对象代次变化都使旧预览失去权威，必须重新确认。
        ensure!(
            view.fingerprint == *fingerprint,
            crate::RepositoryFailure::Conflict(
                "deletion references changed since preview".to_owned()
            )
        );
        // 保护引用仍存在时不得受理清除；这是当前业务状态冲突，不应促使客户端重试为依赖故障。
        ensure!(
            view.blockers.is_empty(),
            crate::RepositoryFailure::Conflict(
                "deletion is blocked by active or retained references".to_owned()
            )
        );
        let pending: u64 = transaction.query_row(
            "SELECT count(*) FROM object_deletions WHERE state IN ('accepted', 'running')",
            [],
            |row| row.get(0),
        )?;
        ensure!(
            pending < 8,
            crate::ResourceLimitExceeded("deletion task budget exceeded")
        );
        for object in &view.objects {
            if object.state == "present" && !object.shared {
                objects::verify_object(&self.root, &object.digest, Some(object.size))?;
            }
        }
        let id = Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO object_deletions(id, actor, scope, state) VALUES (?1, ?2, ?3, 'accepted')",
            params![id, actor, serde_json::to_string(&scope)?],
        )?;
        for object in &view.objects {
            if scope.kind == "hosted" {
                for revision in &object.revisions {
                    transaction.execute("INSERT OR IGNORE INTO artifact_access_revocations(revision_id, deletion_id, actor) VALUES (?1, ?2, ?3)", params![revision, id, actor])?;
                }
            } else {
                transaction.execute("UPDATE proxy_materializations SET valid = 0 WHERE proxy_id = ?1 AND id IN (SELECT materialization_id FROM materialization_items WHERE digest = ?2)", params![identity.repository_id, object.digest.as_str()])?;
                transaction.execute(
                    "DELETE FROM proxy_objects WHERE proxy_id = ?1 AND digest = ?2",
                    params![identity.repository_id, object.digest.as_str()],
                )?;
            }
            let state = if object.shared || object.state == "purged" {
                "shared"
            } else {
                "deleting"
            };
            transaction.execute("INSERT INTO deletion_items(deletion_id, digest, generation, size, state) VALUES (?1, ?2, ?3, ?4, ?5)", params![id, object.digest.as_str(), object.generation, object.size, state])?;
            if state == "deleting" {
                ensure!(transaction.execute("UPDATE objects SET state = 'deleting' WHERE sha256 = ?1 AND generation = ?2 AND state = 'present'", params![object.digest.as_str(), object.generation])? == 1, crate::RepositoryFailure::Conflict("object generation changed".to_owned()));
            }
        }
        if scope.kind == "proxy" {
            // 清缓存不删除来源或历史摘要证据，也不创建任何包名或版本封禁。
            transaction.execute(
                "DELETE FROM proxy_indexes WHERE proxy_id = ?1 AND name = ?2",
                params![identity.repository_id, scope.name],
            )?;
            transaction.execute(
                "DELETE FROM proxy_validators WHERE proxy_id = ?1 AND name = ?2",
                params![identity.repository_id, scope.name],
            )?;
            transaction.execute(
                "DELETE FROM proxy_missing WHERE proxy_id = ?1 AND name = ?2",
                params![identity.repository_id, scope.name],
            )?;
        }
        transaction.execute(
            "UPDATE deletion_previews SET state = 'accepted', task_id = ?1 WHERE id = ?2",
            params![id, preview_id],
        )?;
        transaction.execute("INSERT INTO security_audit(actor, action, target) VALUES (?1, 'accept_physical_clear', ?2)", params![actor, id])?;
        transaction.commit()?;
        read_task(&connection, &id)
    }

    /// 业务作用：按已提交的删除任务与对象代次完成隔离、同步、删除及最终状态发布。
    /// 参数说明：
    /// - `id`：已持久受理的任务 UUID。
    ///
    /// 返回：实际完成的任务与唯一释放字节；任何异常保留阶段证据供重启继续。
    pub fn run_deletion(&self, id: &str) -> Result<DeletionTask> {
        Uuid::parse_str(id)?;
        let items = {
            let connection = self.connection()?;
            let task = read_task(&connection, id)?;
            if task.state == "completed" {
                return Ok(task);
            }
            let mut statement = connection.prepare("SELECT digest, generation, size FROM deletion_items WHERE deletion_id = ?1 AND state = 'deleting' ORDER BY digest")?;
            let items = statement
                .query_map([id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, u64>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            connection.execute("UPDATE object_deletions SET state = 'running' WHERE id = ?1 AND state = 'accepted'", [id])?;
            items
        };
        for (digest, generation, size) in items {
            let digest: ArtifactDigest = digest.parse()?;
            let _gate = self
                .object_gate
                .lock()
                .map_err(|_| anyhow::anyhow!("object lifecycle unavailable"))?;
            {
                let connection = self.connection()?;
                let item: String = connection.query_row(
                    "SELECT state FROM deletion_items WHERE deletion_id = ?1 AND digest = ?2",
                    params![id, digest.as_str()],
                    |row| row.get(0),
                )?;
                if item != "deleting" {
                    continue;
                }
                let valid: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM objects WHERE sha256 = ?1 AND generation = ?2 AND state = 'deleting')", params![digest.as_str(), generation], |row| row.get(0))?;
                // 旧任务只能处理自己取得删除权的代次，不能碰后来重新安装的同摘要文件。
                ensure!(
                    valid,
                    crate::RepositoryFailure::Conflict(
                        "deletion object generation changed".to_owned()
                    )
                );
                ensure!(
                    !self
                        .readers
                        .lock()
                        .map_err(|_| anyhow::anyhow!("object readers unavailable"))?
                        .contains_key(&digest),
                    "deletion has active readers"
                );
            }
            let directory = self.root.join("trash").join(id);
            fs::create_dir_all(&directory)?;
            ensure!(
                fs::symlink_metadata(&directory)?.file_type().is_dir(),
                "deletion trash directory cannot be a symlink"
            );
            objects::sync_directory(&self.root.join("trash"))?;
            let source = objects::object_path(&self.root, &digest);
            let trash = directory.join(digest.as_str());
            if source.try_exists()? {
                ensure!(
                    !trash.try_exists()?,
                    "object exists in formal and trash locations"
                );
                objects::verify_path(&source, &digest, Some(size))?;
                fs::rename(&source, &trash)?;
                objects::sync_directory(&self.root.join("objects/sha256"))?;
                objects::sync_directory(&directory)?;
            }
            if trash.try_exists()? {
                objects::verify_path(&trash, &digest, Some(size))?;
                fs::remove_file(&trash)?;
                objects::sync_directory(&directory)?;
            }
            let mut connection = self.connection()?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure!(transaction.execute("UPDATE objects SET state = 'purged' WHERE sha256 = ?1 AND generation = ?2 AND state = 'deleting'", params![digest.as_str(), generation])? == 1, crate::RepositoryFailure::Conflict("deletion object generation changed".to_owned()));
            transaction.execute("UPDATE deletion_items SET state = 'purged' WHERE deletion_id = ?1 AND digest = ?2 AND generation = ?3 AND state = 'deleting'", params![id, digest.as_str(), generation])?;
            transaction.execute(
                "UPDATE object_deletions SET released_bytes = released_bytes + ?1 WHERE id = ?2",
                params![size, id],
            )?;
            transaction.commit()?;
        }
        let connection = self.connection()?;
        let pending: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM deletion_items WHERE deletion_id = ?1 AND state = 'deleting')", [id], |row| row.get(0))?;
        if !pending {
            let directory = self.root.join("trash").join(id);
            if directory.try_exists()? {
                fs::remove_dir(&directory)?;
                objects::sync_directory(&self.root.join("trash"))?;
            }
            connection.execute(
                "UPDATE object_deletions SET state = 'completed', last_error_code = NULL, next_attempt_at = 0 WHERE id = ?1",
                [id],
            )?;
        }
        read_task(&connection, id)
    }

    /// 业务作用：在完整性检查前继续有持久删除证据的任务，合法缺失不被误判为丢失数据。
    /// 参数说明: 无。
    ///
    /// 返回：全部已受理任务达到可解释终态；未知代次或文件不一致拒绝开放存储。
    pub(crate) fn recover_deletions(&self) -> Result<()> {
        let pending = {
            let connection = self.connection()?;
            let mut statement = connection.prepare("SELECT id FROM object_deletions WHERE state IN ('accepted', 'running') ORDER BY created_at LIMIT 9")?;
            let pending = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            pending
        };
        ensure!(
            pending.len() <= 8,
            crate::ResourceLimitExceeded("deletion recovery budget exceeded")
        );
        for id in pending {
            self.run_deletion(&id)?;
        }
        Ok(())
    }

    /// 业务作用：向管理界面提供已受理任务状态，页面关闭不取消后台已经提交的删除权威。
    /// 参数说明：
    /// - `id`：任务 UUID。
    ///
    /// 返回：任务阶段、精确范围与实际释放字节。
    pub fn deletion_task(&self, id: &str) -> Result<DeletionTask> {
        let connection = self.connection()?;
        read_task(&connection, id)
    }

    /// 业务作用：在同一数据库视图与对象门禁下计算本次清除的可达引用与保护。
    /// 参数说明：
    /// - `connection`：当前一致事务。
    /// - `actor`：管理主体。
    /// - `scope`：已选择范围。
    ///
    /// 返回：稳定排序的代次与阻挡指纹；不改变对象状态。
    fn deletion_view(
        &self,
        connection: &Connection,
        actor: &str,
        scope: &DeletionScope,
    ) -> Result<DeletionPreview> {
        let identity = deletion_permission(connection, actor, scope)?;
        let mut objects = BTreeMap::<ArtifactDigest, DeletionObject>::new();
        if scope.kind == "hosted" {
            let mut statement = connection.prepare("SELECT r.id, o.sha256, o.generation, o.size, o.state FROM revisions r JOIN objects o ON o.sha256 = r.sha256 WHERE r.repository_id = ?1 AND r.name = ?2 AND (?3 IS NULL OR r.version = ?3) AND (?4 IS NULL OR r.id = ?4) LIMIT 1025")?;
            let rows = statement
                .query_map(
                    params![
                        identity.repository_id,
                        scope.name,
                        scope.version,
                        scope.revision
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, u64>(2)?,
                            row.get::<_, u64>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ensure!(
                rows.len() <= 1024,
                crate::ResourceLimitExceeded("deletion selection exceeds limit")
            );
            ensure!(
                !rows.is_empty(),
                crate::RepositoryFailure::NotFound("unknown deletion revision selection")
            );
            for (revision, digest, generation, size, state) in rows {
                let digest: ArtifactDigest = digest.parse()?;
                objects
                    .entry(digest.clone())
                    .or_insert_with(|| DeletionObject {
                        digest,
                        generation,
                        size,
                        state,
                        shared: false,
                        revisions: Vec::new(),
                    })
                    .revisions
                    .push(revision);
            }
        } else {
            let mut statement = connection.prepare("SELECT DISTINCT o.sha256, o.generation, o.size, o.state FROM proxy_coordinates c JOIN proxy_objects p ON p.proxy_id = c.proxy_id AND p.digest = c.digest JOIN objects o ON o.sha256 = c.digest WHERE c.proxy_id = ?1 AND c.name = ?2 AND (?3 IS NULL OR c.version = ?3) LIMIT 257")?;
            let rows = statement
                .query_map(
                    params![identity.repository_id, scope.name, scope.version],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, u64>(1)?,
                            row.get::<_, u64>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (digest, generation, size, state) in rows {
                let digest: ArtifactDigest = digest.parse()?;
                objects.insert(
                    digest.clone(),
                    DeletionObject {
                        digest,
                        generation,
                        size,
                        state,
                        shared: false,
                        revisions: Vec::new(),
                    },
                );
            }
        }
        ensure!(
            objects.len() <= 256
                && objects.values().map(|object| object.size).sum::<u64>() <= 512 * 1024 * 1024,
            crate::ResourceLimitExceeded("deletion byte or object budget exceeded")
        );
        let readers = self
            .readers
            .lock()
            .map_err(|_| anyhow::anyhow!("object readers unavailable"))?;
        let mut blockers = BTreeSet::new();
        let mut reclaimable_bytes = 0_u64;
        let mut retention_budget = 4096usize;
        for object in objects.values_mut() {
            object.revisions.sort();
            if object.state == "deleting" {
                blockers.insert(format!("对象已有清除任务：{}", object.digest));
            }
            if readers.contains_key(&object.digest) {
                blockers.insert(format!("对象存在活动读取：{}", object.digest));
            }
            let publishing: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM batch_items i JOIN publish_batches b ON b.id = i.batch_id WHERE b.state IN ('open', 'validating') AND b.expires_at > unixepoch() AND json_extract(i.entry_json, '$.cksum') = ?1)", [object.digest.as_str()], |row| row.get(0))?;
            if publishing {
                blockers.insert(format!("发布批次仍引用对象：{}", object.digest));
            }
            let resolving: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM proxy_session_objects o JOIN proxy_sessions s ON s.id = o.session_id WHERE s.expires_at > unixepoch() AND o.digest = ?1) OR EXISTS(SELECT 1 FROM proxy_session_entries e JOIN proxy_sessions s ON s.id = e.session_id WHERE s.expires_at > unixepoch() AND NOT EXISTS(SELECT 1 FROM proxy_session_objects o WHERE o.session_id = e.session_id AND o.name = e.name))", [object.digest.as_str()], |row| row.get(0))?;
            if resolving {
                blockers.insert(format!("公开解析视图仍保护对象：{}", object.digest));
            }
            let mut statement = connection.prepare("SELECT r.id FROM revisions r WHERE r.sha256 = ?1 AND NOT EXISTS(SELECT 1 FROM artifact_access_revocations a WHERE a.revision_id = r.id) LIMIT 1025")?;
            let references = statement
                .query_map([object.digest.as_str()], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ensure!(
                references.len() <= 1024,
                crate::ResourceLimitExceeded("deletion reference budget exceeded")
            );
            object.shared = references
                .iter()
                .any(|revision| !identity.revisions.contains(revision));
            let proxy_shared: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM proxy_objects WHERE digest = ?1 AND (?2 != 'proxy' OR proxy_id != ?3))", params![object.digest.as_str(), scope.kind, identity.repository_id], |row| row.get(0))?;
            object.shared |= proxy_shared;
            if scope.kind == "hosted" {
                for revision in references
                    .iter()
                    .filter(|revision| identity.revisions.contains(*revision))
                {
                    if !scope.revoke_history {
                        blockers.insert(format!("历史修订仍被保留：{revision}"));
                    }
                    protect_revision(
                        connection,
                        revision,
                        &mut blockers,
                        self.options.max_snapshot_depth,
                        &mut retention_budget,
                    )?;
                }
            }
            if !object.shared && object.state == "present" {
                reclaimable_bytes = reclaimable_bytes.saturating_add(object.size);
            }
        }
        let objects = objects.into_values().collect::<Vec<_>>();
        let blockers = blockers.into_iter().collect::<Vec<_>>();
        let fingerprint = ArtifactDigest::of(&serde_json::to_vec(&(
            scope,
            &objects,
            &blockers,
            reclaimable_bytes,
        ))?);
        Ok(DeletionPreview {
            id: String::new(),
            scope: scope.clone(),
            objects,
            blockers,
            reclaimable_bytes,
            fingerprint,
            expires_at: connection.query_row("SELECT unixepoch() + 300", [], |row| row.get(0))?,
            refetch_allowed: scope.kind == "proxy",
        })
    }
}

/// 业务作用：核对当前运维权限与精确清除范围，公开缓存与私有历史采用不同撤销语义。
/// 参数说明：
/// - `connection`：当前事务。
/// - `actor`：管理主体。
/// - `scope`：明确选择的范围。
///
/// 返回：稳定来源与将被撤销的 hosted 修订集合；非法选择为输入拒绝，越权为授权拒绝，选集超限为配额拒绝。
fn deletion_permission(
    connection: &Connection,
    actor: &str,
    scope: &DeletionScope,
) -> Result<ScopeIdentity> {
    ensure!(
        matches!(scope.kind.as_str(), "hosted" | "proxy")
            && scope.all_versions != scope.version.is_some(),
        crate::RepositoryFailure::InvalidRequest("invalid deletion scope")
    );
    naxus_core::validate_name(&scope.name).context(crate::RepositoryFailure::InvalidRequest(
        "invalid deletion name",
    ))?;
    if let Some(version) = &scope.version {
        semver::Version::parse(version).context(crate::RepositoryFailure::InvalidRequest(
            "invalid deletion version",
        ))?;
    }
    ensure!(
        scope.revision.is_none() || (scope.kind == "hosted" && scope.version.is_some()),
        crate::RepositoryFailure::InvalidRequest("invalid deletion revision scope")
    );
    ensure!(
        scope.kind != "proxy" || !scope.revoke_history,
        crate::RepositoryFailure::InvalidRequest(
            "public cache clearing cannot revoke hosted history"
        )
    );
    let grants: String = connection.query_row("SELECT repositories FROM admin_users WHERE id = ?1 AND role IN ('admin', 'operator') AND enabled = 1 AND must_change = 0", [actor], |row| row.get(0)).optional()?.context(crate::RepositoryFailure::Forbidden("physical clear permission denied"))?;
    let grants: Vec<String> = serde_json::from_str(&grants)?;
    let repository_id: String = if scope.kind == "hosted" {
        connection.query_row(
            "SELECT id FROM repositories WHERE id = ?1 OR name = ?1",
            [&scope.repository],
            |row| row.get(0),
        )?
    } else {
        connection.query_row(
            "SELECT id FROM proxy_repositories WHERE id = ?1 OR name = ?1",
            [&scope.repository],
            |row| row.get(0),
        )?
    };
    ensure!(
        grants
            .iter()
            .any(|grant| grant == "*" || (scope.kind == "hosted" && grant == &repository_id)),
        crate::RepositoryFailure::Forbidden("physical clear repository permission denied")
    );
    let mut revisions = BTreeSet::new();
    if scope.kind == "hosted" {
        let mut statement = connection.prepare("SELECT id FROM revisions WHERE repository_id = ?1 AND name = ?2 AND (?3 IS NULL OR version = ?3) AND (?4 IS NULL OR id = ?4) LIMIT 1025")?;
        let selected = statement
            .query_map(
                params![repository_id, scope.name, scope.version, scope.revision],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ensure!(
            selected.len() <= 1024,
            crate::ResourceLimitExceeded("deletion selection exceeds limit")
        );
        revisions.extend(selected);
    }
    Ok(ScopeIdentity {
        repository_id,
        revisions,
    })
}

/// 业务作用：保护当前 HEAD、显式保留点和活动租约正在使用的 hosted 修订。
/// 参数说明：
/// - `connection`：当前清除事务。
/// - `revision`：希望撤销的真实修订。
/// - `blockers`：本次预览的保护原因集合。
/// - `depth`：快照父链读取预算。
/// - `budget`：整次预览剩余的快照与修订复验次数。
///
/// 返回：命中实际修订引用时加入阻挡；不会用相同摘要的其他修订冒充引用。
fn protect_revision(
    connection: &Connection,
    revision: &str,
    blockers: &mut BTreeSet<String>,
    depth: usize,
    budget: &mut usize,
) -> Result<()> {
    let (repository, name, version): (String, String, String) = connection.query_row(
        "SELECT repository_id, name, version FROM revisions WHERE id = ?1",
        [revision],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let coordinate = Coordinate {
        name,
        version,
    };
    let mut statement = connection.prepare("SELECT head, 'HEAD' FROM repositories WHERE id = ?1 UNION SELECT l.snapshot_id, '活动租约' FROM retention_leases l JOIN snapshots s ON s.id = l.snapshot_id WHERE s.repository_id = ?1 AND l.expires_at > unixepoch() UNION SELECT p.snapshot_id, '保留点' FROM retention_pins p JOIN snapshots s ON s.id = p.snapshot_id WHERE s.repository_id = ?1 LIMIT 4097")?;
    let snapshots = statement
        .query_map([repository], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        snapshots.len() <= 4096,
        crate::ResourceLimitExceeded("deletion retention budget exceeded")
    );
    // 多个修订与多个保留点的乘积也必须受限，不能仅分别限制两个列表而形成长时间全局阻塞。
    ensure!(
        snapshots.len() <= *budget,
        crate::ResourceLimitExceeded(
            "deletion retention evaluation budget exceeded; select a smaller revision scope"
        )
    );
    *budget -= snapshots.len();
    for (snapshot, kind) in snapshots {
        if revision_at(connection, &snapshot, &coordinate, depth)?
            .is_some_and(|selected| selected.id == revision)
        {
            blockers.insert(format!("{kind}仍引用修订：{revision}"));
        }
    }
    Ok(())
}

/// 业务作用：读取任务的持久业务状态，不从文件是否存在猜测是否已经完成。
/// 参数说明：
/// - `connection`：当前一致视图。
/// - `id`：任务 UUID。
///
/// 返回：阶段、范围与已提交释放字节；未知任务返回错误。
fn read_task(connection: &Connection, id: &str) -> Result<DeletionTask> {
    let (state, released_bytes, scope): (String, u64, String) = connection.query_row(
        "SELECT state, released_bytes, scope FROM object_deletions WHERE id = ?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    Ok(DeletionTask {
        id: id.to_owned(),
        state,
        released_bytes,
        scope: serde_json::from_str(&scope)?,
        attempts: connection.query_row(
            "SELECT attempts FROM object_deletions WHERE id = ?1",
            [id],
            |row| row.get(0),
        )?,
        next_attempt_at: connection.query_row(
            "SELECT next_attempt_at FROM object_deletions WHERE id = ?1",
            [id],
            |row| row.get(0),
        )?,
        last_error_code: connection.query_row(
            "SELECT last_error_code FROM object_deletions WHERE id = ?1",
            [id],
            |row| row.get(0),
        )?,
    })
}
