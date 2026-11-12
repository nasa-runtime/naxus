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
            crate::statements::Statement::MaintenanceMaintainQueryMetadata,
            crate::database::params![],
            |_| Ok(()),
        )?;
        for statement in [
            crate::statements::Statement::MaintenanceMaintainUpdatePublishBatches,
            crate::statements::Statement::MaintenanceMaintainDeleteProxySessions,
            crate::statements::Statement::MaintenanceMaintainDeleteRetentionLeases,
            crate::statements::Statement::MaintenanceMaintainDeleteAdminSessions,
            crate::statements::Statement::MaintenanceMaintainDeleteProxyMissing,
            crate::statements::Statement::MaintenanceMaintainDeleteManagementPreviews,
            crate::statements::Statement::MaintenanceMaintainDeleteDeletionPreviews,
        ] {
            connection.execute(statement, crate::database::params![])?;
        }
        let mut next_verified_digest = verify_after.unwrap_or_default().to_owned();
        if let Some(after) = verify_after {
            use crate::database::OptionalExtension;
            let object: Option<(String, u64)> = connection
                .query_row(
                    crate::statements::Statement::MaintenanceMaintainQueryObjects,
                    [after],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((digest, size)) = object {
                objects::verify_object(&self.root, &digest.parse()?, Some(size))?;
                next_verified_digest = digest;
            } else {
                next_verified_digest.clear();
            }
        }
        connection.checkpoint()?;
        let free_bytes = fs2::available_space(&self.root)?;
        Ok(StorageStatus {
            free_bytes,
            reserve_free_bytes: self.options.reserve_free_bytes,
            writable: free_bytes > self.options.reserve_free_bytes,
            next_verified_digest,
        })
    }
}
