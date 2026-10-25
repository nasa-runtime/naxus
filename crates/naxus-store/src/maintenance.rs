use anyhow::Result;
use serde::Serialize;

use crate::{objects, Store};

/// 常驻服务读取、写入与容量观测；低空闲空间不改变已有对象的读取合同。
#[derive(Clone, Serialize)]
pub struct StorageStatus {
    pub free_bytes: u64,
    pub reserve_free_bytes: u64,
    pub writable: bool,
    pub next_verified_digest: String,
}

impl Store {
    /// 业务作用：分批过期短期控制记录并检查数据库可读性，避免长期在线积累无限会话。
    /// 参数说明：
    /// - `verify_after`：需要轮转验证对象时的摘要游标，None 时只检查元数据与容量。
    ///
    /// 返回：当前容量和写入门禁观测；每次最多清理各类二百五十六条，最多核验一个对象。
    pub fn maintain(&self, verify_after: Option<&str>) -> Result<StorageStatus> {
        let connection = self.connection()?;
        connection.query_row(
            "SELECT 1 FROM metadata WHERE key = 'service_id'",
            [],
            |_| Ok(()),
        )?;
        for statement in [
            "UPDATE publish_batches SET state = 'expired' WHERE id IN (SELECT id FROM publish_batches WHERE state = 'open' AND expires_at <= unixepoch() LIMIT 256)",
            "DELETE FROM proxy_sessions WHERE id IN (SELECT id FROM proxy_sessions WHERE expires_at <= unixepoch() LIMIT 256)",
            "DELETE FROM retention_leases WHERE id IN (SELECT id FROM retention_leases WHERE expires_at <= unixepoch() LIMIT 256)",
            "DELETE FROM admin_sessions WHERE digest IN (SELECT digest FROM admin_sessions WHERE expires_at <= unixepoch() LIMIT 256)",
            "DELETE FROM proxy_missing WHERE rowid IN (SELECT rowid FROM proxy_missing WHERE expires_at <= unixepoch() LIMIT 256)",
            "DELETE FROM management_previews WHERE id IN (SELECT id FROM management_previews WHERE state = 'prepared' AND expires_at <= unixepoch() LIMIT 256)",
            "DELETE FROM deletion_previews WHERE id IN (SELECT id FROM deletion_previews WHERE state = 'prepared' AND expires_at <= unixepoch() LIMIT 256)",
        ] {
            connection.execute(statement, [])?;
        }
        let mut next_verified_digest = verify_after.unwrap_or_default().to_owned();
        if let Some(after) = verify_after {
            use rusqlite::OptionalExtension;
            let object: Option<(String, u64)> = connection.query_row("SELECT sha256, size FROM objects WHERE state = 'present' AND sha256 > ?1 ORDER BY sha256 LIMIT 1", [after], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
            if let Some((digest, size)) = object {
                objects::verify_object(&self.root, &digest.parse()?, Some(size))?;
                next_verified_digest = digest;
            } else {
                next_verified_digest.clear();
            }
        }
        connection.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |_| Ok(()))?;
        let free_bytes = fs2::available_space(&self.root)?;
        Ok(StorageStatus {
            free_bytes,
            reserve_free_bytes: self.options.reserve_free_bytes,
            writable: free_bytes > self.options.reserve_free_bytes,
            next_verified_digest,
        })
    }
}
