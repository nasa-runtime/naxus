use crate::database::{params, Connection, OptionalExtension};
use anyhow::{ensure, Result};
/// 业务作用：读取受保护的 schema 进度，兼容旧数据库的持久格式标识。
/// 参数说明：
/// - `connection`：已持有数据卷独占权的元数据执行域。
///
/// 返回：已确认的迁移进度；未知格式或完整性失败时拒绝。
pub(crate) fn revision(connection: &Connection) -> Result<u32> {
    let legacy = connection.inspect()?;
    ensure!(legacy <= 13, "unsupported repository schema");
    let exists: bool = connection.query_row(
        crate::statements::Statement::SchemaRevisionQuerySqliteSchema,
        crate::database::params![],
        |row| row.get(0),
    )?;
    if !exists {
        ensure!(legacy == 0, "repository schema metadata is missing");
        return Ok(0);
    }
    let revision: Option<String> = connection
        .query_row(
            crate::statements::Statement::SchemaRevisionQueryMetadata,
            crate::database::params![],
            |row| row.get(0),
        )
        .optional()?;
    Ok(revision
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(legacy))
}

/// 业务作用：原子建立仓库、修订、快照与对象基本结构并推进结构迁移进度。
/// 参数说明：
/// - `connection`：调用者持有的写事务。
///
/// 返回：所有结构与进度写入完成；任一步失败交由外层回滚。
pub(crate) fn repository(connection: &Connection) -> Result<()> {
    connection.execute(
        crate::statements::Statement::SchemaRepositoryCreateMetadata,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaRepositoryCreateRepositories,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaRepositoryCreateSnapshots,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaRepositoryCreateObjects,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaRepositoryCreateCrateNames,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaRepositoryCreateRevisions,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaRepositoryCreateSnapshotChanges,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaRepositoryCreateCommits,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaRepositoryCreateAudit,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaRepositoryCreateValue,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaRepositoryInsertMetadata,
        params![1.to_string()],
    )?;
    Ok(())
}

/// 业务作用：原子建立快照读取检查点并推进结构迁移进度。
/// 参数说明：
/// - `connection`：调用者持有的写事务。
///
/// 返回：所有结构与进度写入完成；任一步失败交由外层回滚。
pub(crate) fn snapshot_checkpoints(connection: &Connection) -> Result<()> {
    connection.execute(
        crate::statements::Statement::SchemaSnapshotCheckpointsCreateSnapshotCheckpoints,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaSnapshotCheckpointsCreateCheckpointEntries,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaSnapshotCheckpointsInsertMetadata,
        params![2.to_string()],
    )?;
    Ok(())
}

/// 业务作用：原子建立原子发布批次并推进结构迁移进度。
/// 参数说明：
/// - `connection`：调用者持有的写事务。
///
/// 返回：所有结构与进度写入完成；任一步失败交由外层回滚。
pub(crate) fn publish_batches(connection: &Connection) -> Result<()> {
    connection.execute(
        crate::statements::Statement::SchemaPublishBatchesCreatePublishBatches,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaPublishBatchesCreateBatchItems,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaPublishBatchesCreateValue,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaPublishBatchesInsertMetadata,
        params![3.to_string()],
    )?;
    Ok(())
}

/// 业务作用：原子建立构建保护租约并推进结构迁移进度。
/// 参数说明：
/// - `connection`：调用者持有的写事务。
///
/// 返回：所有结构与进度写入完成；任一步失败交由外层回滚。
pub(crate) fn retention_leases(connection: &Connection) -> Result<()> {
    connection.execute(
        crate::statements::Statement::SchemaRetentionLeasesCreateRetentionLeases,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaRetentionLeasesCreateValue,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaRetentionLeasesInsertMetadata,
        params![4.to_string()],
    )?;
    Ok(())
}

/// 业务作用：原子建立公开来源索引与对象缓存并推进结构迁移进度。
/// 参数说明：
/// - `connection`：调用者持有的写事务。
///
/// 返回：所有结构与进度写入完成；任一步失败交由外层回滚。
pub(crate) fn proxy_cache(connection: &Connection) -> Result<()> {
    connection.execute(
        crate::statements::Statement::SchemaProxyCacheCreateProxyRepositories,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaProxyCacheCreateProxyIndexes,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaProxyCacheCreateProxyCoordinates,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaProxyCacheCreateProxyObjects,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaProxyCacheInsertMetadata,
        params![5.to_string()],
    )?;
    Ok(())
}

/// 业务作用：原子建立固定代理解析视图及物化集合并推进结构迁移进度。
/// 参数说明：
/// - `connection`：调用者持有的写事务。
///
/// 返回：所有结构与进度写入完成；任一步失败交由外层回滚。
pub(crate) fn proxy_resolution(connection: &Connection) -> Result<()> {
    connection.execute(
        crate::statements::Statement::SchemaProxyResolutionCreateProxySessions,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaProxyResolutionCreateProxySessionEntries,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaProxyResolutionCreateProxyValidators,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaProxyResolutionCreateProxyMissing,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaProxyResolutionCreateProxyMaterializations,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaProxyResolutionCreateValue,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaProxyResolutionCreateMaterializationItems,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaProxyResolutionInsertMetadata,
        params![6.to_string()],
    )?;
    Ok(())
}

/// 业务作用：原子建立账户、会话、令牌与安全审计并推进结构迁移进度。
/// 参数说明：
/// - `connection`：调用者持有的写事务。
///
/// 返回：所有结构与进度写入完成；任一步失败交由外层回滚。
pub(crate) fn access_control(connection: &Connection) -> Result<()> {
    connection.execute(
        crate::statements::Statement::SchemaAccessControlCreateAdminUsers,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaAccessControlCreateAdminSessions,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaAccessControlCreateAccessTokens,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaAccessControlCreateSecurityAudit,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaAccessControlInsertMetadata,
        params![7.to_string()],
    )?;
    Ok(())
}

/// 业务作用：原子建立包版本管理与逻辑移除并推进结构迁移进度。
/// 参数说明：
/// - `connection`：调用者持有的写事务。
///
/// 返回：所有结构与进度写入完成；任一步失败交由外层回滚。
pub(crate) fn hosted_management(connection: &Connection) -> Result<()> {
    connection.execute(
        crate::statements::Statement::SchemaHostedManagementCreateSnapshotRemovals,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaHostedManagementCreateManagementPreviews,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaHostedManagementInsertMetadata,
        params![8.to_string()],
    )?;
    Ok(())
}

/// 业务作用：原子建立对象代次、清除权威与保留点并推进结构迁移进度。
/// 参数说明：
/// - `connection`：调用者持有的写事务。
///
/// 返回：所有结构与进度写入完成；任一步失败交由外层回滚。
pub(crate) fn object_lifecycle(connection: &Connection) -> Result<()> {
    connection.execute(
        crate::statements::Statement::SchemaObjectLifecycleAlterObjects,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaObjectLifecycleAlterObjects2,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaObjectLifecycleCreateArtifactAccessRevocations,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaObjectLifecycleCreateRetentionPins,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaObjectLifecycleCreateDeletionPreviews,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaObjectLifecycleCreateObjectDeletions,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaObjectLifecycleCreateDeletionItems,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaObjectLifecycleCreateProxySessionObjects,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaObjectLifecycleInsertMetadata,
        params![9.to_string()],
    )?;
    Ok(())
}

/// 业务作用：原子建立固定代理镜像起点并推进结构迁移进度。
/// 参数说明：
/// - `connection`：调用者持有的写事务。
///
/// 返回：所有结构与进度写入完成；任一步失败交由外层回滚。
pub(crate) fn proxy_views(connection: &Connection) -> Result<()> {
    connection.execute(
        crate::statements::Statement::SchemaProxyViewsAlterProxySessions,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaProxyViewsInsertMetadata,
        params![10.to_string()],
    )?;
    Ok(())
}

/// 业务作用：原子建立清除重试与退避状态并推进结构迁移进度。
/// 参数说明：
/// - `connection`：调用者持有的写事务。
///
/// 返回：所有结构与进度写入完成；任一步失败交由外层回滚。
pub(crate) fn deletion_retries(connection: &Connection) -> Result<()> {
    connection.execute(
        crate::statements::Statement::SchemaDeletionRetriesAlterObjectDeletions,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaDeletionRetriesAlterObjectDeletions2,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaDeletionRetriesAlterObjectDeletions3,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaDeletionRetriesCreateValue,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaDeletionRetriesInsertMetadata,
        params![11.to_string()],
    )?;
    Ok(())
}

/// 业务作用：原子建立发布令牌的覆盖能力与包范围并推进结构迁移进度。
/// 参数说明：
/// - `connection`：调用者持有的写事务。
///
/// 返回：所有结构与进度写入完成；任一步失败交由外层回滚。
pub(crate) fn token_publish_scope(connection: &Connection) -> Result<()> {
    connection.execute(
        crate::statements::Statement::SchemaTokenPublishScopeAlterAccessTokens,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaTokenPublishScopeAlterAccessTokens2,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaTokenPublishScopeInsertMetadata,
        params![12.to_string()],
    )?;
    Ok(())
}

/// 业务作用：原子建立未引用对象的安装时间与引用索引并推进结构迁移进度。
/// 参数说明：
/// - `connection`：调用者持有的写事务。
///
/// 返回：所有结构与进度写入完成；任一步失败交由外层回滚。
pub(crate) fn unpublished_objects(connection: &Connection) -> Result<()> {
    connection.execute(
        crate::statements::Statement::SchemaUnpublishedObjectsAlterObjects,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaUnpublishedObjectsUpdateObjects,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaUnpublishedObjectsCreateValue,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaUnpublishedObjectsCreateValue2,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaUnpublishedObjectsCreateValue3,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaUnpublishedObjectsCreateValue4,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaUnpublishedObjectsCreateValue5,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaUnpublishedObjectsCreateValue6,
        crate::database::params![],
    )?;
    connection.execute(
        crate::statements::Statement::SchemaUnpublishedObjectsInsertMetadata,
        params![13.to_string()],
    )?;
    Ok(())
}
