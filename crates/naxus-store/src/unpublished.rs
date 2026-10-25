use std::fs;

use anyhow::{ensure, Result};
use naxus_core::ArtifactDigest;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use uuid::Uuid;

use crate::{objects, DeletionScope, Store};

impl Store {
    /// 业务作用：为超过宽限期且从未被业务修订引用的对象建立持久清理任务，防止失败上传长期占用空间。
    /// 参数说明: 无。
    ///
    /// 返回：至多一项任务，每项最多十六对象和 512 MiB；活动读取、批次、公开来源及固定视图均受保护。
    pub fn collect_unpublished(&self) -> Result<Option<String>> {
        let _gate = self
            .object_gate
            .lock()
            .map_err(|_| anyhow::anyhow!("object lifecycle unavailable"))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pending: u64 = transaction.query_row(
            "SELECT count(*) FROM object_deletions WHERE state IN ('accepted', 'running')",
            [],
            |row| row.get(0),
        )?;
        if pending >= 4 {
            // 为用户明确提交的清除保留队列空间，维护不能占满所有删除权威。
            return Ok(None);
        }
        let unmapped: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM proxy_session_entries e JOIN proxy_sessions s ON s.id = e.session_id WHERE s.expires_at > unixepoch() AND NOT EXISTS(SELECT 1 FROM proxy_session_objects o WHERE o.session_id = e.session_id AND o.name = e.name))", [], |row| row.get(0))?;
        if unmapped {
            // 升级前尚无精确对象映射的活动视图仍然保有读取承诺，不能据此推断任何对象可回收。
            return Ok(None);
        }
        let mut statement = transaction.prepare("SELECT o.sha256, o.generation, o.size FROM objects o WHERE o.state = 'present' AND o.installed_at <= unixepoch() - 3600 AND NOT EXISTS(SELECT 1 FROM revisions r WHERE r.sha256 = o.sha256) AND NOT EXISTS(SELECT 1 FROM proxy_objects p WHERE p.digest = o.sha256) AND NOT EXISTS(SELECT 1 FROM materialization_items i JOIN proxy_materializations m ON m.id = i.materialization_id WHERE i.digest = o.sha256 AND m.valid = 1) AND NOT EXISTS(SELECT 1 FROM batch_items i JOIN publish_batches b ON b.id = i.batch_id WHERE json_extract(i.entry_json, '$.cksum') = o.sha256 AND (b.state = 'validating' OR (b.state = 'open' AND b.expires_at > unixepoch()))) AND NOT EXISTS(SELECT 1 FROM proxy_session_objects p JOIN proxy_sessions s ON s.id = p.session_id WHERE p.digest = o.sha256 AND s.expires_at > unixepoch()) ORDER BY o.installed_at, o.sha256 LIMIT 64")?;
        let candidates = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, u64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let readers = self
            .readers
            .lock()
            .map_err(|_| anyhow::anyhow!("object readers unavailable"))?;
        let mut selected = Vec::new();
        let mut bytes = 0u64;
        for (digest, generation, size) in candidates {
            let digest: ArtifactDigest = digest.parse()?;
            if readers.contains_key(&digest)
                || selected.len() >= 16
                || bytes.saturating_add(size) > 512 * 1024 * 1024
            {
                continue;
            }
            objects::verify_object(&self.root, &digest, Some(size))?;
            bytes += size;
            selected.push((digest, generation, size));
        }
        if selected.is_empty() {
            return Ok(None);
        }
        let id = Uuid::new_v4().to_string();
        let scope = DeletionScope {
            kind: "unpublished".to_owned(),
            repository: self.service_id().to_owned(),
            name: "unpublished-objects".to_owned(),
            version: None,
            all_versions: true,
            revision: None,
            revoke_history: false,
        };
        // 只有完整数据库引用复验后才提交清理权；文件动作继续复用代次隔离和崩溃恢复。
        transaction.execute("INSERT INTO object_deletions(id, actor, scope, state) VALUES (?1, 'maintenance', ?2, 'accepted')", params![id, serde_json::to_string(&scope)?])?;
        for (digest, generation, size) in selected {
            ensure!(transaction.execute("UPDATE objects SET state = 'deleting' WHERE sha256 = ?1 AND generation = ?2 AND state = 'present'", params![digest.as_str(), generation])? == 1, "unpublished object authority changed");
            transaction.execute("INSERT INTO deletion_items(deletion_id, digest, generation, size, state) VALUES (?1, ?2, ?3, ?4, 'deleting')", params![id, digest.as_str(), generation, size])?;
        }
        transaction.execute("INSERT INTO security_audit(actor, action, target) VALUES ('maintenance', 'collect-unpublished', ?1)", [&id])?;
        transaction.commit()?;
        Ok(Some(id))
    }

    /// 业务作用：发现正式文件落盘而对象登记未完成的崩溃窗口，为其补齐可审计的未发布对象身份。
    /// 参数说明: 无。
    ///
    /// 返回：仅接纳摘要、实体文件和大小均可核对的对象；陌生路径拒绝，不凭文件名公开业务引用。
    pub(crate) fn recover_unpublished(&self) -> Result<()> {
        let connection = self.connection()?;
        for entry in fs::read_dir(self.root.join("objects/sha256"))? {
            let entry = entry?;
            let digest: ArtifactDigest = entry
                .file_name()
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid object filename"))?
                .parse()?;
            ensure!(
                entry.file_type()?.is_file(),
                "unexpected object directory entry"
            );
            let known: Option<(u64, String)> = connection
                .query_row(
                    "SELECT size, state FROM objects WHERE sha256 = ?1",
                    [digest.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if known.is_none() {
                let object = objects::verify_object(&self.root, &digest, None)?;
                connection.execute(
                    "INSERT INTO objects(sha256, size, installed_at) VALUES (?1, ?2, unixepoch())",
                    params![digest.as_str(), object.size],
                )?;
            } else if let Some((size, state)) = known {
                if state == "purged" {
                    objects::verify_object(&self.root, &digest, Some(size))?;
                    // 同摘要重装已完成文件同步但未登记时，恢复新的代次；旧修订撤销记录保持原值。
                    connection.execute("UPDATE objects SET generation = generation + 1, state = 'present', installed_at = unixepoch() WHERE sha256 = ?1 AND state = 'purged'", [digest.as_str()])?;
                }
            }
        }
        Ok(())
    }
}
