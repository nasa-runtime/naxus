use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{ensure, Context, Result};
use naxus_core::{
    canonical_name, IndexEntry, Snapshot, MAX_BATCH_ITEMS, MAX_PUBLICATION_METADATA_BYTES,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{CommitRequest, Store};

/// 固定发布目标、主体及预期 HEAD 的持久上传批次。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublishBatch {
    pub id: String,
    pub repository_id: String,
    pub actor: String,
    pub expected_head: String,
    pub state: String,
    pub snapshot_id: Option<String>,
    pub expires_at: u64,
    pub entries: Vec<IndexEntry>,
}

impl Store {
    /// 业务作用：在上传前绑定仓库、发布主体和并发前提，支持请求丢失后的同键查询。
    /// 参数说明：
    /// - `repository`：已授权的仓库名称或 UUID。
    /// - `actor`：经过认证的稳定主体。
    /// - `expected_head`：准备发布时固定的 HEAD。
    /// - `key`：此主体在仓库内的幂等键。
    ///
    /// 返回：新建或相同请求已有的批次；HEAD、键或资源预算冲突时拒绝。
    pub fn begin_batch(
        &self,
        repository: &str,
        actor: &str,
        expected_head: &str,
        key: &str,
    ) -> Result<PublishBatch> {
        ensure!(
            !actor.is_empty() && actor.len() <= 256 && !key.is_empty() && key.len() <= 256,
            crate::RepositoryFailure::InvalidRequest("invalid publish identity")
        );
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (repository_id, head): (String, String) = transaction.query_row(
            "SELECT id, head FROM repositories WHERE name = ?1 OR id = ?1",
            [repository],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let existing: Option<String> = transaction.query_row("SELECT id FROM publish_batches WHERE repository_id = ?1 AND actor = ?2 AND idempotency_key = ?3", params![repository_id, actor, key], |row| row.get(0)).optional()?;
        if let Some(id) = existing {
            let batch = read_batch(&transaction, &id)?;
            ensure!(
                batch.expected_head == expected_head,
                crate::RepositoryFailure::Conflict(
                    "idempotency key was used for a different HEAD".to_owned()
                )
            );
            return Ok(batch);
        }
        ensure!(
            head == expected_head,
            crate::RepositoryFailure::Conflict("repository HEAD changed".to_owned())
        );
        transaction.execute("UPDATE publish_batches SET state = 'expired' WHERE state = 'open' AND expires_at <= ?1", [now()?])?;
        let active: u64 = transaction.query_row("SELECT COUNT(*) FROM publish_batches WHERE actor = ?1 AND state IN ('open', 'validating')", [actor], |row| row.get(0))?;
        ensure!(
            active < 64,
            crate::ResourceLimitExceeded("publish batch budget exceeded")
        );
        let id = Uuid::new_v4().to_string();
        transaction.execute("INSERT INTO publish_batches(id, repository_id, actor, idempotency_key, expected_head, state, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6)", params![id, repository_id, actor, key, expected_head, now()?.saturating_add(3600)])?;
        let batch = read_batch(&transaction, &id)?;
        transaction.commit()?;
        Ok(batch)
    }

    /// 业务作用：读取批次并复验其仓库与主体归属，避免枚举批次 ID 越权。
    /// 参数说明：
    /// - `repository`：已授权的仓库。
    /// - `id`：持久批次 ID。
    /// - `actor`：当前认证主体。
    ///
    /// 返回：归属一致的批次；不存在、跨仓或跨主体时返回错误。
    pub fn batch(&self, repository: &str, id: &str, actor: &str) -> Result<PublishBatch> {
        let connection = self.connection()?;
        let batch = read_batch(&connection, id)?;
        let repository_id: String = connection.query_row(
            "SELECT id FROM repositories WHERE name = ?1 OR id = ?1",
            [repository],
            |row| row.get(0),
        )?;
        ensure!(
            batch.repository_id == repository_id && batch.actor == actor,
            crate::RepositoryFailure::Forbidden(
                "publish batch belongs to another principal or repository"
            )
        );
        Ok(batch)
    }

    /// 业务作用：只向仍开放且未过期的批次登记已完成归档校验的固定制品。
    /// 参数说明：
    /// - `repository`：批次所属仓库。
    /// - `id`：批次 ID。
    /// - `actor`：经过认证的上传者。
    /// - `entry`：与已安装对象匹配的索引元数据。
    ///
    /// 返回：条目可重复登记相同内容；同坐标变更、终态或超限均拒绝。
    pub fn add_batch_item(
        &self,
        repository: &str,
        id: &str,
        actor: &str,
        entry: &IndexEntry,
    ) -> Result<()> {
        entry.validate()?;
        let batch = self.batch(repository, id, actor)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read_batch(&transaction, id)?;
        ensure!(
            current.state == "open" && current.expires_at > now()?,
            crate::RepositoryFailure::Conflict("publish batch is not open".to_owned())
        );
        ensure!(
            current.repository_id == batch.repository_id && current.actor == actor,
            crate::RepositoryFailure::Conflict("publish batch authority changed".to_owned())
        );
        if let Some(existing) = current.entries.iter().find(|item| {
            canonical_name(&item.name) == canonical_name(&entry.name) && item.vers == entry.vers
        }) {
            ensure!(
                existing == entry,
                crate::RepositoryFailure::Conflict(
                    "batch coordinate already contains different content".to_owned()
                )
            );
            return Ok(());
        }
        ensure!(
            current.entries.len() < MAX_BATCH_ITEMS,
            crate::PublishBatchTooLarge::ItemCount
        );
        let encoded = serde_json::to_string(entry)?;
        let bytes: u64 = transaction.query_row(
            "SELECT COALESCE(SUM(length(CAST(entry_json AS BLOB))), 0) FROM batch_items WHERE batch_id = ?1",
            [id],
            |row| row.get(0),
        )?;
        // 在同一写事务内限制完整数组，含 UTF-8 字节和 JSON 分隔符；已接受批次必须始终可被恢复客户端读取。
        ensure!(
            bytes + encoded.len() as u64 + current.entries.len() as u64 + 2
                <= MAX_PUBLICATION_METADATA_BYTES as u64,
            crate::PublishBatchTooLarge::MetadataBytes
        );
        transaction.execute(
            "INSERT INTO batch_items(batch_id, name, version, entry_json) VALUES (?1, ?2, ?3, ?4)",
            params![id, entry.name, entry.vers, encoded],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// 业务作用：封闭批次后验证私有依赖闭包，将批次终态与 HEAD 原子提交。
    /// 参数说明：
    /// - `repository`：批次所属仓库。
    /// - `id`：批次 ID。
    /// - `actor`：经过认证的提交者。
    ///
    /// 返回：完整已提交快照；校验失败终止批次，历史图和 HEAD 保持原状态。
    pub fn commit_batch(&self, repository: &str, id: &str, actor: &str) -> Result<Snapshot> {
        self.commit_batch_authorized(repository, id, actor, None)
    }

    /// 业务作用：在批次 HEAD 原子发布时复验受管 token 的当前授权，避免长上传沿用过期权限。
    /// 参数说明：
    /// - `repository`：目标仓库。
    /// - `id`：已经完成上传的批次 UUID。
    /// - `actor`：原提交主体。
    /// - `token`：可选的受管 token 摘要。
    ///
    /// 返回：授权、依赖闭包及 HEAD 均有效才提交，拒绝时不公开新快照。
    pub fn commit_batch_authorized(
        &self,
        repository: &str,
        id: &str,
        actor: &str,
        token: Option<&naxus_core::ArtifactDigest>,
    ) -> Result<Snapshot> {
        let mut batch = self.batch(repository, id, actor)?;
        if batch.state == "committed" {
            return self.snapshot(
                repository,
                batch
                    .snapshot_id
                    .as_deref()
                    .context("committed batch has no snapshot")?,
            );
        }
        {
            let mut connection = self.connection()?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // 封闭上传与读取最终条目共用写事务，不能提交封闭前取得的旧条目集合。
            let changed = transaction.execute("UPDATE publish_batches SET state = 'validating' WHERE id = ?1 AND state = 'open' AND expires_at > ?2", params![id, now()?])?;
            batch = read_batch(&transaction, id)?;
            ensure!(
                changed == 1 && !batch.entries.is_empty(),
                crate::RepositoryFailure::Conflict(
                    "publish batch cannot enter validation".to_owned()
                )
            );
            transaction.commit()?;
        }
        let result = (|| {
            for entry in &batch.entries {
                for dependency in &entry.deps {
                    if dependency.registry.is_some() || dependency.kind.as_deref() == Some("dev") {
                        continue;
                    }
                    let name = dependency.package.as_deref().unwrap_or(&dependency.name);
                    let requirement = semver::VersionReq::parse(&dependency.req)?;
                    let mut candidates = self.index_entries(&batch.expected_head, name)?;
                    // 同一批次的坐标会遮蔽旧索引；依赖闭包必须按提交后的状态判定，不能借用已被替代的旧行。
                    candidates.retain(|candidate| {
                        !batch.entries.iter().any(|replacement| {
                            replacement.name == candidate.name && replacement.vers == candidate.vers
                        })
                    });
                    candidates.extend(
                        batch
                            .entries
                            .iter()
                            .filter(|candidate| candidate.name == name)
                            .cloned(),
                    );
                    ensure!(
                        candidates.iter().any(|candidate| !candidate.yanked
                            && semver::Version::parse(&candidate.vers)
                                .is_ok_and(|version| requirement.matches(&version))),
                        crate::RepositoryFailure::Conflict(format!(
                            "private dependency closure is incomplete: {name}"
                        ))
                    );
                }
            }
            self.commit_inner(
                &CommitRequest {
                    repository: batch.repository_id.clone(),
                    expected_head: batch.expected_head,
                    idempotency_key: format!("batch:{}", batch.id),
                    actor: actor.to_owned(),
                    entries: batch.entries,
                },
                Some(id),
                token,
            )
        })();
        if result.is_err() {
            self.connection()?.execute("UPDATE publish_batches SET state = 'aborted' WHERE id = ?1 AND state = 'validating'", [id])?;
        }
        result
    }

    /// 业务作用：停止尚未提交的批次，不逆转已经成功提交的仓库快照。
    /// 参数说明：
    /// - `repository`：批次所属仓库。
    /// - `id`：批次 ID。
    /// - `actor`：经过认证的操作者。
    ///
    /// 返回：更新后的批次；终态保持原值，校验中的批次不能被上传者并发撤销。
    pub fn abort_batch(&self, repository: &str, id: &str, actor: &str) -> Result<PublishBatch> {
        self.batch(repository, id, actor)?;
        self.connection()?.execute(
            "UPDATE publish_batches SET state = 'aborted' WHERE id = ?1 AND state = 'open'",
            [id],
        )?;
        self.batch(repository, id, actor)
    }

    /// 业务作用：重启后终止没有提交证据的校验批次，禁止对不完整上传自动发布。
    /// 参数说明: 无。
    ///
    /// 返回：未提交校验转为 aborted，过期开放批次转为 expired；已提交批次保持不变。
    pub(crate) fn recover_batches(&self) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE publish_batches SET state = 'aborted' WHERE state = 'validating'",
            [],
        )?;
        connection.execute("UPDATE publish_batches SET state = 'expired' WHERE state = 'open' AND expires_at <= ?1", [now()?])?;
        Ok(())
    }
}

/// 业务作用：在同一数据库读取视图中取得批次状态及完整上传条目。
/// 参数说明：
/// - `connection`：当前连接或事务。
/// - `id`：批次 ID。
///
/// 返回：确定性条目顺序的批次；未知批次或损坏 JSON 返回错误。
fn read_batch(connection: &Connection, id: &str) -> Result<PublishBatch> {
    let mut batch = connection.query_row("SELECT repository_id, actor, expected_head, state, snapshot_id, expires_at FROM publish_batches WHERE id = ?1", [id], |row| {
        Ok(PublishBatch {
            id: id.to_owned(),
            repository_id: row.get(0)?,
            actor: row.get(1)?,
            expected_head: row.get(2)?,
            state: row.get(3)?,
            snapshot_id: row.get(4)?,
            expires_at: row.get(5)?,
            entries: Vec::new(),
        })
    })?;
    let mut statement = connection
        .prepare("SELECT entry_json FROM batch_items WHERE batch_id = ?1 ORDER BY name, version")?;
    let rows = statement.query_map([id], |row| row.get::<_, String>(0))?;
    for row in rows {
        batch.entries.push(serde_json::from_str(&row?)?);
    }
    Ok(batch)
}

/// 业务作用：为批次期限使用统一 UTC Unix 秒，避免与客户端时钟混用。
/// 参数说明: 无。
///
/// 返回：服务器当前秒数；系统时钟早于 Unix 纪元时拒绝产生期限。
fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
