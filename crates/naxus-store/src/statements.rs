use crate::database::{Row, Value};
use anyhow::{ensure, Result};
use nasa::mapper::sqlite::Mapper;

/// 持久管理预览的选集、预期 HEAD 和授权主体。
struct ManagementPreviewWrite<'a> {
    id: &'a Value,
    actor: &'a Value,
    repository_id: &'a Value,
    expected_head: &'a Value,
    operation: &'a Value,
    selection: &'a Value,
    fingerprint: &'a Value,
    expires_at: &'a Value,
}

/// 不可变修订的仓库坐标、内容摘要、索引和审计主体。
struct RevisionWrite<'a> {
    id: &'a Value,
    repository_id: &'a Value,
    name: &'a Value,
    version: &'a Value,
    sha256: &'a Value,
    metadata_sha256: &'a Value,
    entry_json: &'a Value,
    actor: &'a Value,
}

/// 受限发布令牌的账户身份、授权范围和到期策略。
struct TokenWrite<'a> {
    id: &'a Value,
    digest: &'a Value,
    user_id: &'a Value,
    label: &'a Value,
    permission: &'a Value,
    repositories: &'a Value,
    auth_version: &'a Value,
    expires_at: &'a Value,
    allow_overwrite: &'a Value,
    publish_crates: &'a Value,
}

#[Mapper(datasource = "naxus", cache = false)]
trait BackupMapper {
    /// 业务作用：为 backup 读取 objects 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT sha256, size FROM objects WHERE state = 'present' ORDER BY sha256")]
    async fn backup_query_objects(&self) -> Result<Vec<Row>>;
}

#[Mapper(datasource = "naxus", cache = false)]
trait BatchesMapper {
    /// 业务作用：为 begin_batch 读取 repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id, head FROM repositories WHERE name = #{p1} OR id = #{p1}")]
    async fn begin_batch_query_repositories(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 begin_batch 读取 publish_batches 的仓库状态。
    /// 参数说明：
    /// - `p1`：`repository_id` 对应的仓库稳定身份。
    /// - `p2`：`actor` 对应的经过认证的操作主体。
    /// - `p3`：`idempotency_key` 对应的固定业务意图的幂等身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id FROM publish_batches WHERE repository_id = #{p1} AND actor = #{p2} AND idempotency_key = #{p3}")]
    async fn begin_batch_query_publish_batches(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 begin_batch 持久化 publish_batches 的仓库状态。
    /// 参数说明：
    /// - `p1`：`expires_at` 对应的保护或授权的截止时刻。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update(
        "UPDATE publish_batches SET state = 'expired' WHERE state = 'open' AND expires_at <= #{p1}"
    )]
    async fn begin_batch_update_publish_batches(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 begin_batch 读取 publish_batches 的仓库状态。
    /// 参数说明：
    /// - `p1`：`actor` 对应的经过认证的操作主体。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT COUNT(*) FROM publish_batches WHERE actor = #{p1} AND state IN ('open', 'validating')")]
    async fn begin_batch_query_publish_batches_2(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 begin_batch 持久化 publish_batches 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`repository_id` 对应的仓库稳定身份。
    /// - `p3`：`actor` 对应的经过认证的操作主体。
    /// - `p4`：`idempotency_key` 对应的固定业务意图的幂等身份。
    /// - `p5`：`expected_head` 对应的发布时固定的预期 HEAD。
    /// - `p6`：`expires_at` 对应的保护或授权的截止时刻。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO publish_batches(id, repository_id, actor, idempotency_key, expected_head, state, expires_at) VALUES (#{p1}, #{p2}, #{p3}, #{p4}, #{p5}, 'open', #{p6})")]
    async fn begin_batch_insert_publish_batches(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
        p5: &Value,
        p6: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 batch 读取 repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id FROM repositories WHERE name = #{p1} OR id = #{p1}")]
    async fn batch_query_repositories(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 add_batch_item 读取 batch_items 的仓库状态。
    /// 参数说明：
    /// - `p1`：`batch_id` 对应的业务筛选或持久状态值。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT COALESCE(SUM(length(CAST(entry_json AS BLOB))), 0) FROM batch_items WHERE batch_id = #{p1}")]
    async fn add_batch_item_query_batch_items(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 add_batch_item 持久化 batch_items 的仓库状态。
    /// 参数说明：
    /// - `p1`：`batch_id` 对应的业务筛选或持久状态值。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    /// - `p4`：`entry_json` 对应的已经验证的索引元数据。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO batch_items(batch_id, name, version, entry_json) VALUES (#{p1}, #{p2}, #{p3}, #{p4})")]
    async fn add_batch_item_insert_batch_items(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 commit_batch_authorized 持久化 publish_batches 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`expires_at` 对应的保护或授权的截止时刻。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE publish_batches SET state = 'validating' WHERE id = #{p1} AND state = 'open' AND expires_at > #{p2}")]
    async fn commit_batch_authorized_update_publish_batches(
        &self,
        p1: &Value,
        p2: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 commit_batch_authorized 持久化 publish_batches 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update(
        "UPDATE publish_batches SET state = 'aborted' WHERE id = #{p1} AND state = 'validating'"
    )]
    async fn commit_batch_authorized_update_publish_batches_2(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 abort_batch 持久化 publish_batches 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE publish_batches SET state = 'aborted' WHERE id = #{p1} AND state = 'open'")]
    async fn abort_batch_update_publish_batches(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 recover_batches 持久化 publish_batches 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE publish_batches SET state = 'aborted' WHERE state = 'validating'")]
    async fn recover_batches_update_publish_batches(&self) -> Result<u64>;

    /// 业务作用：为 recover_batches 持久化 publish_batches 的仓库状态。
    /// 参数说明：
    /// - `p1`：`expires_at` 对应的保护或授权的截止时刻。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update(
        "UPDATE publish_batches SET state = 'expired' WHERE state = 'open' AND expires_at <= #{p1}"
    )]
    async fn recover_batches_update_publish_batches_2(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 read_batch 读取 publish_batches 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT repository_id, actor, expected_head, state, snapshot_id, expires_at FROM publish_batches WHERE id = #{p1}")]
    async fn read_batch_query_publish_batches(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 read_batch 读取 batch_items 的仓库状态。
    /// 参数说明：
    /// - `p1`：`batch_id` 对应的业务筛选或持久状态值。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT entry_json FROM batch_items WHERE batch_id = #{p1} ORDER BY name, version")]
    async fn read_batch_query_batch_items(&self, p1: &Value) -> Result<Vec<Row>>;
}

#[Mapper(datasource = "naxus", cache = false)]
trait DeletionMapper {
    /// 业务作用：为 deletion_tasks 读取 object_deletions 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id FROM object_deletions ORDER BY created_at DESC, rowid DESC LIMIT 200")]
    async fn deletion_tasks_query_object_deletions(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 pending_deletion 读取 object_deletions 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id FROM object_deletions WHERE state IN ('accepted', 'running') AND next_attempt_at <= unixepoch() ORDER BY next_attempt_at, created_at, rowid LIMIT 1")]
    async fn pending_deletion_query_object_deletions(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 defer_deletion 持久化 object_deletions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`last_error_code` 对应的上次持久任务失败的稳定类别。
    /// - `p2`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE object_deletions SET attempts = attempts + 1, last_error_code = #{p1}, next_attempt_at = unixepoch() + min(3600, 5 * (1 << min(attempts, 10))) WHERE id = #{p2} AND state IN ('accepted', 'running')")]
    async fn defer_deletion_update_object_deletions(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 retry_deletion 读取 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT repositories FROM admin_users WHERE id = #{p1} AND role IN ('admin', 'operator') AND enabled = 1 AND must_change = 0")]
    async fn retry_deletion_query_admin_users(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 retry_deletion 持久化 object_deletions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE object_deletions SET next_attempt_at = 0 WHERE id = #{p1} AND state IN ('accepted', 'running')")]
    async fn retry_deletion_update_object_deletions(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 retry_deletion 持久化 security_audit 的仓库状态。
    /// 参数说明：
    /// - `p1`：`actor` 对应的经过认证的操作主体。
    /// - `p2`：`target` 对应的业务筛选或持久状态值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert(
        "INSERT INTO security_audit(actor, action, target) VALUES (#{p1}, 'retry-clear', #{p2})"
    )]
    async fn retry_deletion_insert_security_audit(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 preview_deletion 持久化 deletion_previews 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM deletion_previews WHERE id IN (SELECT id FROM deletion_previews WHERE state = 'prepared' AND expires_at <= unixepoch() LIMIT 256)")]
    async fn preview_deletion_delete_deletion_previews(&self) -> Result<u64>;

    /// 业务作用：为 preview_deletion 读取 deletion_previews 的仓库状态。
    /// 参数说明：
    /// - `p1`：`actor` 对应的经过认证的操作主体。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT count(*) FROM deletion_previews WHERE actor = #{p1} AND state = 'prepared'")]
    async fn preview_deletion_query_deletion_previews(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 preview_deletion 持久化 deletion_previews 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`actor` 对应的经过认证的操作主体。
    /// - `p3`：`scope` 对应的已经验证的管理范围。
    /// - `p4`：`fingerprint` 对应的预览选集及保护状态的摘要。
    /// - `p5`：`expires_at` 对应的保护或授权的截止时刻。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO deletion_previews(id, actor, scope, fingerprint, expires_at) VALUES (#{p1}, #{p2}, #{p3}, #{p4}, #{p5})")]
    async fn preview_deletion_insert_deletion_previews(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
        p5: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 accept_deletion 读取 deletion_previews 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`actor` 对应的经过认证的操作主体。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT scope, fingerprint, expires_at > unixepoch(), task_id FROM deletion_previews WHERE id = #{p1} AND actor = #{p2}")]
    async fn accept_deletion_query_deletion_previews(
        &self,
        p1: &Value,
        p2: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 accept_deletion 读取 object_deletions 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT count(*) FROM object_deletions WHERE state IN ('accepted', 'running')")]
    async fn accept_deletion_query_object_deletions(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 accept_deletion 持久化 object_deletions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`actor` 对应的经过认证的操作主体。
    /// - `p3`：`scope` 对应的已经验证的管理范围。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO object_deletions(id, actor, scope, state) VALUES (#{p1}, #{p2}, #{p3}, 'accepted')")]
    async fn accept_deletion_insert_object_deletions(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 accept_deletion 持久化 artifact_access_revocations 的仓库状态。
    /// 参数说明：
    /// - `p1`：`revision_id` 对应的不可变修订身份。
    /// - `p2`：`deletion_id` 对应的持久清除任务身份。
    /// - `p3`：`actor` 对应的经过认证的操作主体。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT OR IGNORE INTO artifact_access_revocations(revision_id, deletion_id, actor) VALUES (#{p1}, #{p2}, #{p3})")]
    async fn accept_deletion_insert_artifact_access_revocations(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 accept_deletion 持久化 proxy_materializations 的仓库状态。
    /// 参数说明：
    /// - `p1`：`proxy_id` 对应的公开来源的稳定身份。
    /// - `p2`：`digest` 对应的完整字节或凭据摘要。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE proxy_materializations SET valid = 0 WHERE proxy_id = #{p1} AND id IN (SELECT materialization_id FROM materialization_items WHERE digest = #{p2})")]
    async fn accept_deletion_update_proxy_materializations(
        &self,
        p1: &Value,
        p2: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 accept_deletion 持久化 proxy_objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`proxy_id` 对应的公开来源的稳定身份。
    /// - `p2`：`digest` 对应的完整字节或凭据摘要。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM proxy_objects WHERE proxy_id = #{p1} AND digest = #{p2}")]
    async fn accept_deletion_delete_proxy_objects(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 accept_deletion 持久化 deletion_items 的仓库状态。
    /// 参数说明：
    /// - `p1`：`deletion_id` 对应的持久清除任务身份。
    /// - `p2`：`digest` 对应的完整字节或凭据摘要。
    /// - `p3`：`generation` 对应的对象的持久存储代次。
    /// - `p4`：`size` 对应的对象字节数。
    /// - `p5`：`state` 对应的允许推进的生命周期状态。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO deletion_items(deletion_id, digest, generation, size, state) VALUES (#{p1}, #{p2}, #{p3}, #{p4}, #{p5})")]
    async fn accept_deletion_insert_deletion_items(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
        p5: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 accept_deletion 持久化 objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`sha256` 对应的完整字节摘要。
    /// - `p2`：`generation` 对应的对象的持久存储代次。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE objects SET state = 'deleting' WHERE sha256 = #{p1} AND generation = #{p2} AND state = 'present'")]
    async fn accept_deletion_update_objects(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 accept_deletion 持久化 proxy_indexes 的仓库状态。
    /// 参数说明：
    /// - `p1`：`proxy_id` 对应的公开来源的稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM proxy_indexes WHERE proxy_id = #{p1} AND name = #{p2}")]
    async fn accept_deletion_delete_proxy_indexes(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 accept_deletion 持久化 proxy_validators 的仓库状态。
    /// 参数说明：
    /// - `p1`：`proxy_id` 对应的公开来源的稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM proxy_validators WHERE proxy_id = #{p1} AND name = #{p2}")]
    async fn accept_deletion_delete_proxy_validators(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 accept_deletion 持久化 proxy_missing 的仓库状态。
    /// 参数说明：
    /// - `p1`：`proxy_id` 对应的公开来源的稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM proxy_missing WHERE proxy_id = #{p1} AND name = #{p2}")]
    async fn accept_deletion_delete_proxy_missing(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 accept_deletion 持久化 deletion_previews 的仓库状态。
    /// 参数说明：
    /// - `p1`：`task_id` 对应的已受理的持久任务身份。
    /// - `p2`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE deletion_previews SET state = 'accepted', task_id = #{p1} WHERE id = #{p2}")]
    async fn accept_deletion_update_deletion_previews(&self, p1: &Value, p2: &Value)
        -> Result<u64>;

    /// 业务作用：为 accept_deletion 持久化 security_audit 的仓库状态。
    /// 参数说明：
    /// - `p1`：`actor` 对应的经过认证的操作主体。
    /// - `p2`：`target` 对应的业务筛选或持久状态值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO security_audit(actor, action, target) VALUES (#{p1}, 'accept_physical_clear', #{p2})")]
    async fn accept_deletion_insert_security_audit(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 run_deletion 读取 deletion_items 的仓库状态。
    /// 参数说明：
    /// - `p1`：`deletion_id` 对应的持久清除任务身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT digest, generation, size FROM deletion_items WHERE deletion_id = #{p1} AND state = 'deleting' ORDER BY digest")]
    async fn run_deletion_query_deletion_items(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 run_deletion 持久化 object_deletions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update(
        "UPDATE object_deletions SET state = 'running' WHERE id = #{p1} AND state = 'accepted'"
    )]
    async fn run_deletion_update_object_deletions(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 run_deletion 读取 deletion_items 的仓库状态。
    /// 参数说明：
    /// - `p1`：`deletion_id` 对应的持久清除任务身份。
    /// - `p2`：`digest` 对应的完整字节或凭据摘要。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT state FROM deletion_items WHERE deletion_id = #{p1} AND digest = #{p2}")]
    async fn run_deletion_query_deletion_items_2(&self, p1: &Value, p2: &Value)
        -> Result<Vec<Row>>;

    /// 业务作用：为 run_deletion 读取 objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`sha256` 对应的完整字节摘要。
    /// - `p2`：`generation` 对应的对象的持久存储代次。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM objects WHERE sha256 = #{p1} AND generation = #{p2} AND state = 'deleting')")]
    async fn run_deletion_query_objects(&self, p1: &Value, p2: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 run_deletion 持久化 objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`sha256` 对应的完整字节摘要。
    /// - `p2`：`generation` 对应的对象的持久存储代次。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE objects SET state = 'purged' WHERE sha256 = #{p1} AND generation = #{p2} AND state = 'deleting'")]
    async fn run_deletion_update_objects(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 run_deletion 持久化 deletion_items 的仓库状态。
    /// 参数说明：
    /// - `p1`：`deletion_id` 对应的持久清除任务身份。
    /// - `p2`：`digest` 对应的完整字节或凭据摘要。
    /// - `p3`：`generation` 对应的对象的持久存储代次。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE deletion_items SET state = 'purged' WHERE deletion_id = #{p1} AND digest = #{p2} AND generation = #{p3} AND state = 'deleting'")]
    async fn run_deletion_update_deletion_items(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 run_deletion 持久化 object_deletions 的仓库状态。
    /// 参数说明：
    /// - `p1`：当前接口的边界或条件值，参与本方法声明的筛选与授权范围。
    /// - `p2`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update(
        "UPDATE object_deletions SET released_bytes = released_bytes + #{p1} WHERE id = #{p2}"
    )]
    async fn run_deletion_update_object_deletions_2(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 run_deletion 读取 deletion_items 的仓库状态。
    /// 参数说明：
    /// - `p1`：`deletion_id` 对应的持久清除任务身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM deletion_items WHERE deletion_id = #{p1} AND state = 'deleting')")]
    async fn run_deletion_query_deletion_items_3(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 run_deletion 持久化 object_deletions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE object_deletions SET state = 'completed', last_error_code = NULL, next_attempt_at = 0 WHERE id = #{p1}")]
    async fn run_deletion_update_object_deletions_3(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 recover_deletions 读取 object_deletions 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id FROM object_deletions WHERE state IN ('accepted', 'running') ORDER BY created_at LIMIT 9")]
    async fn recover_deletions_query_object_deletions(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 deletion_view 读取 revisions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`repository_id` 对应的仓库稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    /// - `p4`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT r.id, o.sha256, o.generation, o.size, o.state FROM revisions r JOIN objects o ON o.sha256 = r.sha256 WHERE r.repository_id = #{p1} AND r.name = #{p2} AND (#{p3} IS NULL OR r.version = #{p3}) AND (#{p4} IS NULL OR r.id = #{p4}) LIMIT 1025")]
    async fn deletion_view_query_revisions(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 deletion_view 读取 proxy_coordinates 的仓库状态。
    /// 参数说明：
    /// - `p1`：`proxy_id` 对应的公开来源的稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT DISTINCT o.sha256, o.generation, o.size, o.state FROM proxy_coordinates c JOIN proxy_objects p ON p.proxy_id = c.proxy_id AND p.digest = c.digest JOIN objects o ON o.sha256 = c.digest WHERE c.proxy_id = #{p1} AND c.name = #{p2} AND (#{p3} IS NULL OR c.version = #{p3}) LIMIT 257")]
    async fn deletion_view_query_proxy_coordinates(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 deletion_view 读取 batch_items 的仓库状态。
    /// 参数说明：
    /// - `p1`：当前接口的边界或条件值，参与本方法声明的筛选与授权范围。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM batch_items i JOIN publish_batches b ON b.id = i.batch_id WHERE b.state IN ('open', 'validating') AND b.expires_at > unixepoch() AND json_extract(i.entry_json, '$.cksum') = #{p1})")]
    async fn deletion_view_query_batch_items(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 deletion_view 读取 proxy_session_objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`digest` 对应的完整字节或凭据摘要。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM proxy_session_objects o JOIN proxy_sessions s ON s.id = o.session_id WHERE s.expires_at > unixepoch() AND o.digest = #{p1}) OR EXISTS(SELECT 1 FROM proxy_session_entries e JOIN proxy_sessions s ON s.id = e.session_id WHERE s.expires_at > unixepoch() AND NOT EXISTS(SELECT 1 FROM proxy_session_objects o WHERE o.session_id = e.session_id AND o.name = e.name))")]
    async fn deletion_view_query_proxy_session_objects(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 deletion_view 读取 revisions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`sha256` 对应的完整字节摘要。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT r.id FROM revisions r WHERE r.sha256 = #{p1} AND NOT EXISTS(SELECT 1 FROM artifact_access_revocations a WHERE a.revision_id = r.id) LIMIT 1025")]
    async fn deletion_view_query_revisions_2(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 deletion_view 读取 proxy_objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`digest` 对应的完整字节或凭据摘要。
    /// - `p2`：当前接口的边界或条件值，参与本方法声明的筛选与授权范围。
    /// - `p3`：当前接口的边界或条件值，参与本方法声明的筛选与授权范围。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM proxy_objects WHERE digest = #{p1} AND (#{p2} != 'proxy' OR proxy_id != #{p3}))")]
    async fn deletion_view_query_proxy_objects(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 deletion_view 读取 value 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT unixepoch() + 300")]
    async fn deletion_view_query_value(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 deletion_permission 读取 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT repositories FROM admin_users WHERE id = #{p1} AND role IN ('admin', 'operator') AND enabled = 1 AND must_change = 0")]
    async fn deletion_permission_query_admin_users(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 deletion_permission 读取 repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id FROM repositories WHERE id = #{p1} OR name = #{p1}")]
    async fn deletion_permission_query_repositories(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 deletion_permission 读取 proxy_repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id FROM proxy_repositories WHERE id = #{p1} OR name = #{p1}")]
    async fn deletion_permission_query_proxy_repositories(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 deletion_permission 读取 revisions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`repository_id` 对应的仓库稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    /// - `p4`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id FROM revisions WHERE repository_id = #{p1} AND name = #{p2} AND (#{p3} IS NULL OR version = #{p3}) AND (#{p4} IS NULL OR id = #{p4}) LIMIT 1025")]
    async fn deletion_permission_query_revisions(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 protect_revision 读取 revisions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT repository_id, name, version FROM revisions WHERE id = #{p1}")]
    async fn protect_revision_query_revisions(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 protect_revision 读取 repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT head, 'HEAD' FROM repositories WHERE id = #{p1} UNION SELECT l.snapshot_id, '活动租约' FROM retention_leases l JOIN snapshots s ON s.id = l.snapshot_id WHERE s.repository_id = #{p1} AND l.expires_at > unixepoch() UNION SELECT p.snapshot_id, '保留点' FROM retention_pins p JOIN snapshots s ON s.id = p.snapshot_id WHERE s.repository_id = #{p1} LIMIT 4097")]
    async fn protect_revision_query_repositories(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 read_task 读取 object_deletions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT state, released_bytes, scope FROM object_deletions WHERE id = #{p1}")]
    async fn read_task_query_object_deletions(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 read_task 读取 object_deletions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT attempts FROM object_deletions WHERE id = #{p1}")]
    async fn read_task_query_object_deletions_2(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 read_task 读取 object_deletions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT next_attempt_at FROM object_deletions WHERE id = #{p1}")]
    async fn read_task_query_object_deletions_3(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 read_task 读取 object_deletions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT last_error_code FROM object_deletions WHERE id = #{p1}")]
    async fn read_task_query_object_deletions_4(&self, p1: &Value) -> Result<Vec<Row>>;
}

#[Mapper(datasource = "naxus", cache = false)]
trait MaintenanceMapper {
    /// 业务作用：为 maintain 读取 metadata 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT 1 FROM metadata WHERE key = 'service_id'")]
    async fn maintain_query_metadata(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 maintain 持久化 publish_batches 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE publish_batches SET state = 'expired' WHERE id IN (SELECT id FROM publish_batches WHERE state = 'open' AND expires_at <= unixepoch() LIMIT 256)")]
    async fn maintain_update_publish_batches(&self) -> Result<u64>;

    /// 业务作用：为 maintain 持久化 proxy_sessions 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM proxy_sessions WHERE id IN (SELECT id FROM proxy_sessions WHERE expires_at <= unixepoch() LIMIT 256)")]
    async fn maintain_delete_proxy_sessions(&self) -> Result<u64>;

    /// 业务作用：为 maintain 持久化 retention_leases 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM retention_leases WHERE id IN (SELECT id FROM retention_leases WHERE expires_at <= unixepoch() LIMIT 256)")]
    async fn maintain_delete_retention_leases(&self) -> Result<u64>;

    /// 业务作用：为 maintain 持久化 admin_sessions 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM admin_sessions WHERE digest IN (SELECT digest FROM admin_sessions WHERE expires_at <= unixepoch() LIMIT 256)")]
    async fn maintain_delete_admin_sessions(&self) -> Result<u64>;

    /// 业务作用：为 maintain 持久化 proxy_missing 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM proxy_missing WHERE rowid IN (SELECT rowid FROM proxy_missing WHERE expires_at <= unixepoch() LIMIT 256)")]
    async fn maintain_delete_proxy_missing(&self) -> Result<u64>;

    /// 业务作用：为 maintain 持久化 management_previews 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM management_previews WHERE id IN (SELECT id FROM management_previews WHERE state = 'prepared' AND expires_at <= unixepoch() LIMIT 256)")]
    async fn maintain_delete_management_previews(&self) -> Result<u64>;

    /// 业务作用：为 maintain 持久化 deletion_previews 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM deletion_previews WHERE id IN (SELECT id FROM deletion_previews WHERE state = 'prepared' AND expires_at <= unixepoch() LIMIT 256)")]
    async fn maintain_delete_deletion_previews(&self) -> Result<u64>;

    /// 业务作用：为 maintain 读取 objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`sha256` 对应的完整字节摘要。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT sha256, size FROM objects WHERE state = 'present' AND sha256 > #{p1} ORDER BY sha256 LIMIT 1")]
    async fn maintain_query_objects(&self, p1: &Value) -> Result<Vec<Row>>;
}

#[Mapper(datasource = "naxus", cache = false)]
trait ManagementMapper {
    /// 业务作用：为 catalog 读取 repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id, head FROM repositories WHERE id = #{p1} OR name = #{p1}")]
    async fn catalog_query_repositories(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 catalog 读取 revisions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`repository_id` 对应的仓库稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT DISTINCT name, version FROM revisions WHERE repository_id = #{p1} AND (#{p2} IS NULL OR name = #{p2}) AND name || '@' || version > #{p3} ORDER BY name || '@' || version LIMIT 201")]
    async fn catalog_query_revisions(&self, p1: &Value, p2: &Value, p3: &Value)
        -> Result<Vec<Row>>;

    /// 业务作用：为 preview_hosted 读取 repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id, head FROM repositories WHERE name = #{p1} OR id = #{p1}")]
    async fn preview_hosted_query_repositories(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 preview_hosted 读取 revisions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`repository_id` 对应的仓库稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT DISTINCT version FROM revisions WHERE repository_id = #{p1} AND name = #{p2} LIMIT 4097")]
    async fn preview_hosted_query_revisions(&self, p1: &Value, p2: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 preview_hosted 读取 revisions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`repository_id` 对应的仓库稳定身份。
    /// - `p3`：`name` 对应的业务名称或包名称。
    /// - `p4`：`version` 对应的包的精确版本。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT entry_json, metadata_sha256, actor FROM revisions WHERE id = #{p1} AND repository_id = #{p2} AND name = #{p3} AND version = #{p4}")]
    async fn preview_hosted_query_revisions_2(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 preview_hosted 读取 revisions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`repository_id` 对应的仓库稳定身份。
    /// - `p3`：`name` 对应的业务名称或包名称。
    /// - `p4`：`version` 对应的包的精确版本。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM revisions WHERE id = #{p1} AND repository_id = #{p2} AND name = #{p3} AND version = #{p4})")]
    async fn preview_hosted_query_revisions_3(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 preview_hosted 持久化 management_previews 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM management_previews WHERE id IN (SELECT id FROM management_previews WHERE state = 'prepared' AND expires_at <= unixepoch() LIMIT 256)")]
    async fn preview_hosted_delete_management_previews(&self) -> Result<u64>;

    /// 业务作用：为 preview_hosted 读取 management_previews 的仓库状态。
    /// 参数说明：
    /// - `p1`：`actor` 对应的经过认证的操作主体。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT count(*) FROM management_previews WHERE state = 'prepared' AND actor = #{p1}")]
    async fn preview_hosted_query_management_previews(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 preview_hosted 读取 value 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT unixepoch() + 300")]
    async fn preview_hosted_query_value(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 preview_hosted 持久化 management_previews 的仓库状态。
    /// 参数说明：
    /// - `values`：持久管理预览的选集、预期 HEAD 和授权主体。
    ///
    /// 返回：确认完成的影响行数；约束或来源失效时返回错误。
    #[Insert("INSERT INTO management_previews(id, actor, repository_id, expected_head, operation, selection, fingerprint, expires_at) VALUES (#{values.id}, #{values.actor}, #{values.repository_id}, #{values.expected_head}, #{values.operation}, #{values.selection}, #{values.fingerprint}, #{values.expires_at})")]
    async fn preview_hosted_insert_management_previews(
        &self,
        values: ManagementPreviewWrite<'_>,
    ) -> Result<u64>;

    /// 业务作用：为 commit_hosted_management 读取 management_previews 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`actor` 对应的经过认证的操作主体。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT repository_id, expected_head, operation, selection, fingerprint, expires_at > unixepoch(), state, commit_key, committed_snapshot FROM management_previews WHERE id = #{p1} AND actor = #{p2}")]
    async fn commit_hosted_management_query_management_previews(
        &self,
        p1: &Value,
        p2: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 commit_hosted_management 读取 repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT head FROM repositories WHERE id = #{p1}")]
    async fn commit_hosted_management_query_repositories(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 commit_hosted_management 持久化 snapshot_removals 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert(
        "INSERT INTO snapshot_removals(snapshot_id, name, version) VALUES (#{p1}, #{p2}, #{p3})"
    )]
    async fn commit_hosted_management_insert_snapshot_removals(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 commit_hosted_management 读取 artifact_access_revocations 的仓库状态。
    /// 参数说明：
    /// - `p1`：`revision_id` 对应的不可变修订身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM artifact_access_revocations WHERE revision_id = #{p1})")]
    async fn commit_hosted_management_query_artifact_access_revocations(
        &self,
        p1: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 commit_hosted_management 读取 revisions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`repository_id` 对应的仓库稳定身份。
    /// - `p3`：`name` 对应的业务名称或包名称。
    /// - `p4`：`version` 对应的包的精确版本。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT sha256 FROM revisions WHERE id = #{p1} AND repository_id = #{p2} AND name = #{p3} AND version = #{p4}")]
    async fn commit_hosted_management_query_revisions(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 commit_hosted_management 持久化 revisions 的仓库状态。
    /// 参数说明：
    /// - `values`：不可变修订的仓库坐标、内容摘要、索引和审计主体。
    ///
    /// 返回：确认完成的影响行数；约束或来源失效时返回错误。
    #[Insert("INSERT INTO revisions(id, repository_id, name, version, sha256, metadata_sha256, entry_json, actor) VALUES (#{values.id}, #{values.repository_id}, #{values.name}, #{values.version}, #{values.sha256}, #{values.metadata_sha256}, #{values.entry_json}, #{values.actor})")]
    async fn commit_hosted_management_insert_revisions(
        &self,
        values: RevisionWrite<'_>,
    ) -> Result<u64>;

    /// 业务作用：为 commit_hosted_management 持久化 snapshot_changes 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    /// - `p4`：`revision_id` 对应的不可变修订身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO snapshot_changes(snapshot_id, name, version, revision_id) VALUES (#{p1}, #{p2}, #{p3}, #{p4})")]
    async fn commit_hosted_management_insert_snapshot_changes(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 commit_hosted_management 持久化 audit 的仓库状态。
    /// 参数说明：
    /// - `p1`：`actor` 对应的经过认证的操作主体。
    /// - `p2`：`action` 对应的业务筛选或持久状态值。
    /// - `p3`：`repository_id` 对应的仓库稳定身份。
    /// - `p4`：`snapshot_id` 对应的快照稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO audit(actor, action, repository_id, snapshot_id) VALUES (#{p1}, #{p2}, #{p3}, #{p4})")]
    async fn commit_hosted_management_insert_audit(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 commit_hosted_management 持久化 security_audit 的仓库状态。
    /// 参数说明：
    /// - `p1`：`actor` 对应的经过认证的操作主体。
    /// - `p2`：`action` 对应的业务筛选或持久状态值。
    /// - `p3`：`target` 对应的业务筛选或持久状态值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO security_audit(actor, action, target) VALUES (#{p1}, #{p2}, #{p3})")]
    async fn commit_hosted_management_insert_security_audit(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 commit_hosted_management 持久化 management_previews 的仓库状态。
    /// 参数说明：
    /// - `p1`：`commit_key` 对应的业务筛选或持久状态值。
    /// - `p2`：`committed_snapshot` 对应的业务筛选或持久状态值。
    /// - `p3`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE management_previews SET state = 'committed', commit_key = #{p1}, committed_snapshot = #{p2} WHERE id = #{p3}")]
    async fn commit_hosted_management_update_management_previews(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 commit_hosted_management 持久化 repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`head` 对应的已观察到的仓库 HEAD。
    /// - `p2`：`id` 对应的记录的稳定身份。
    /// - `p3`：`head` 对应的已观察到的仓库 HEAD。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE repositories SET head = #{p1} WHERE id = #{p2} AND head = #{p3}")]
    async fn commit_hosted_management_update_repositories(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 management_permission 读取 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT repositories FROM admin_users WHERE id = #{p1} AND role IN ('admin', 'operator') AND enabled = 1 AND must_change = 0")]
    async fn management_permission_query_admin_users(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 revision_history_page 读取 revisions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    /// - `p4`：`rowid` 对应的分页的排他位置。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT v.id, v.sha256, v.metadata_sha256, v.actor, v.entry_json, v.rowid\n             FROM revisions v JOIN repositories r ON r.id = v.repository_id\n             WHERE (r.name = #{p1} OR r.id = #{p1}) AND v.name = #{p2} AND v.version = #{p3}\n               AND (#{p4} = 0 OR v.rowid < #{p4})\n             ORDER BY v.rowid DESC LIMIT 201")]
    async fn revision_history_page_query_revisions(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<Vec<Row>>;
}

#[Mapper(datasource = "naxus", cache = false)]
trait ProxyMapper {
    /// 业务作用：为 proxy_catalog 读取 proxy_repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id FROM proxy_repositories WHERE id = #{p1} OR name = #{p1}")]
    async fn proxy_catalog_query_proxy_repositories(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 proxy_catalog 读取 proxy_coordinates 的仓库状态。
    /// 参数说明：
    /// - `p1`：`proxy_id` 对应的公开来源的稳定身份。
    /// - `p2`：`version` 对应的包的精确版本。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT c.name, c.version, c.digest, p.digest IS NOT NULL AND o.state = 'present' FROM proxy_coordinates c LEFT JOIN proxy_objects p ON p.proxy_id = c.proxy_id AND p.digest = c.digest LEFT JOIN objects o ON o.sha256 = c.digest WHERE c.proxy_id = #{p1} AND c.name || '@' || c.version > #{p2} ORDER BY c.name || '@' || c.version LIMIT 201")]
    async fn proxy_catalog_query_proxy_coordinates(
        &self,
        p1: &Value,
        p2: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 create_proxy 读取 proxy_repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id, name, logical_index FROM proxy_repositories WHERE name = #{p1}")]
    async fn create_proxy_query_proxy_repositories(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 create_proxy 持久化 proxy_repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`logical_index` 对应的业务筛选或持久状态值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert(
        "INSERT INTO proxy_repositories(id, name, logical_index) VALUES (#{p1}, #{p2}, #{p3})"
    )]
    async fn create_proxy_insert_proxy_repositories(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 proxies 读取 proxy_repositories 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id, name, logical_index FROM proxy_repositories ORDER BY name")]
    async fn proxies_query_proxy_repositories(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 save_proxy_index 读取 proxy_repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id FROM proxy_repositories WHERE name = #{p1}")]
    async fn save_proxy_index_query_proxy_repositories(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 save_proxy_index 读取 proxy_coordinates 的仓库状态。
    /// 参数说明：
    /// - `p1`：`proxy_id` 对应的公开来源的稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT digest FROM proxy_coordinates WHERE proxy_id = #{p1} AND name = #{p2} AND version = #{p3}")]
    async fn save_proxy_index_query_proxy_coordinates(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 save_proxy_index 持久化 proxy_coordinates 的仓库状态。
    /// 参数说明：
    /// - `p1`：`proxy_id` 对应的公开来源的稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    /// - `p4`：`digest` 对应的完整字节或凭据摘要。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT OR IGNORE INTO proxy_coordinates(proxy_id, name, version, digest) VALUES (#{p1}, #{p2}, #{p3}, #{p4})")]
    async fn save_proxy_index_insert_proxy_coordinates(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 save_proxy_index 持久化 proxy_indexes 的仓库状态。
    /// 参数说明：
    /// - `p1`：`proxy_id` 对应的公开来源的稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`body` 对应的完整索引字节。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO proxy_indexes(proxy_id, name, body, checked_at) VALUES (#{p1}, #{p2}, #{p3}, unixepoch()) ON CONFLICT(proxy_id, name) DO UPDATE SET body = excluded.body, checked_at = excluded.checked_at")]
    async fn save_proxy_index_insert_proxy_indexes(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 proxy_digest 读取 proxy_coordinates 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT digest FROM proxy_coordinates WHERE proxy_id = (SELECT id FROM proxy_repositories WHERE name = #{p1}) AND name = #{p2} AND version = #{p3}")]
    async fn proxy_digest_query_proxy_coordinates(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 proxy_has_object 读取 proxy_objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    /// - `p2`：`digest` 对应的完整字节或凭据摘要。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM proxy_objects WHERE proxy_id = (SELECT id FROM proxy_repositories WHERE name = #{p1}) AND digest = #{p2})")]
    async fn proxy_has_object_query_proxy_objects(
        &self,
        p1: &Value,
        p2: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 install_proxy_object 读取 objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`sha256` 对应的完整字节摘要。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT state FROM objects WHERE sha256 = #{p1}")]
    async fn install_proxy_object_query_objects(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 install_proxy_object 持久化 objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`sha256` 对应的完整字节摘要。
    /// - `p2`：`size` 对应的对象字节数。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT OR IGNORE INTO objects(sha256, size) VALUES (#{p1}, #{p2})")]
    async fn install_proxy_object_insert_objects(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 install_proxy_object 持久化 proxy_objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    /// - `p2`：当前接口的边界或条件值，参与本方法声明的筛选与授权范围。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT OR IGNORE INTO proxy_objects(proxy_id, digest) SELECT id, #{p2} FROM proxy_repositories WHERE name = #{p1}")]
    async fn install_proxy_object_insert_proxy_objects(
        &self,
        p1: &Value,
        p2: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 proxy_index 读取 proxy_indexes 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    /// - `p2`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT body, checked_at FROM proxy_indexes\n             WHERE proxy_id = (SELECT id FROM proxy_repositories WHERE name = #{p1}) AND name = #{p2}")]
    async fn proxy_index_query_proxy_indexes(&self, p1: &Value, p2: &Value) -> Result<Vec<Row>>;
}

#[Mapper(datasource = "naxus", cache = false)]
trait ProxyResolutionMapper {
    /// 业务作用：为 materialization_valid 读取 proxy_materializations 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM proxy_materializations WHERE id = #{p1} AND valid = 1)")]
    async fn materialization_valid_query_proxy_materializations(
        &self,
        p1: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 begin_proxy_view 持久化 proxy_sessions 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM proxy_sessions WHERE id IN (SELECT id FROM proxy_sessions WHERE expires_at <= unixepoch() LIMIT 256)")]
    async fn begin_proxy_view_delete_proxy_sessions(&self) -> Result<u64>;

    /// 业务作用：为 begin_proxy_view 读取 proxy_sessions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`actor` 对应的经过认证的操作主体。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT count(*), coalesce(sum(actor = #{p1}), 0) FROM proxy_sessions")]
    async fn begin_proxy_view_query_proxy_sessions(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 begin_proxy_view 读取 proxy_repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id FROM proxy_repositories WHERE name = #{p1}")]
    async fn begin_proxy_view_query_proxy_repositories(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 begin_proxy_view 读取 value 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT unixepoch() + 3600")]
    async fn begin_proxy_view_query_value(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 begin_proxy_view 持久化 proxy_sessions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`proxy_id` 对应的公开来源的稳定身份。
    /// - `p3`：`actor` 对应的经过认证的操作主体。
    /// - `p4`：`mode` 对应的当前业务读取策略。
    /// - `p5`：`expires_at` 对应的保护或授权的截止时刻。
    /// - `p6`：`upstream_start` 对应的允许访问的首个镜像位置。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO proxy_sessions(id, proxy_id, actor, mode, expires_at, upstream_start) VALUES (#{p1}, #{p2}, #{p3}, #{p4}, #{p5}, #{p6})")]
    async fn begin_proxy_view_insert_proxy_sessions(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
        p5: &Value,
        p6: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 proxy_session_entry 读取 proxy_session_entries 的仓库状态。
    /// 参数说明：
    /// - `p1`：`session_id` 对应的固定解析视图身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT body FROM proxy_session_entries WHERE session_id = #{p1} AND name = #{p2}")]
    async fn proxy_session_entry_query_proxy_session_entries(
        &self,
        p1: &Value,
        p2: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 freeze_proxy_entry 读取 proxy_sessions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query(
        "SELECT EXISTS(SELECT 1 FROM proxy_sessions WHERE id = #{p1} AND expires_at > unixepoch())"
    )]
    async fn freeze_proxy_entry_query_proxy_sessions(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 freeze_proxy_entry 读取 proxy_session_entries 的仓库状态。
    /// 参数说明：
    /// - `p1`：`session_id` 对应的固定解析视图身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT body FROM proxy_session_entries WHERE session_id = #{p1} AND name = #{p2}")]
    async fn freeze_proxy_entry_query_proxy_session_entries(
        &self,
        p1: &Value,
        p2: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 freeze_proxy_entry 读取 proxy_session_entries 的仓库状态。
    /// 参数说明：
    /// - `p1`：`session_id` 对应的固定解析视图身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT count(*), coalesce(sum(length(body)), 0) FROM proxy_session_entries WHERE session_id = #{p1}")]
    async fn freeze_proxy_entry_query_proxy_session_entries_2(
        &self,
        p1: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 freeze_proxy_entry 持久化 proxy_session_entries 的仓库状态。
    /// 参数说明：
    /// - `p1`：`session_id` 对应的固定解析视图身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`body` 对应的完整索引字节。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT OR IGNORE INTO proxy_session_entries(session_id, name, body) VALUES (#{p1}, #{p2}, #{p3})")]
    async fn freeze_proxy_entry_insert_proxy_session_entries(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 freeze_proxy_entry 读取 proxy_session_entries 的仓库状态。
    /// 参数说明：
    /// - `p1`：`session_id` 对应的固定解析视图身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT body FROM proxy_session_entries WHERE session_id = #{p1} AND name = #{p2}")]
    async fn freeze_proxy_entry_query_proxy_session_entries_3(
        &self,
        p1: &Value,
        p2: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 freeze_proxy_entry 持久化 proxy_session_objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`session_id` 对应的固定解析视图身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`digest` 对应的完整字节或凭据摘要。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT OR IGNORE INTO proxy_session_objects(session_id, name, digest) VALUES (#{p1}, #{p2}, #{p3})")]
    async fn freeze_proxy_entry_insert_proxy_session_objects(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 finish_proxy_session 持久化 proxy_sessions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM proxy_sessions WHERE id = #{p1}")]
    async fn finish_proxy_session_delete_proxy_sessions(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 save_upstream_validation 持久化 proxy_validators 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    /// - `p2`：当前接口的边界或条件值，参与本方法声明的筛选与授权范围。
    /// - `p3`：当前接口的边界或条件值，参与本方法声明的筛选与授权范围。
    /// - `p4`：当前接口的边界或条件值，参与本方法声明的筛选与授权范围。
    /// - `p5`：当前接口的边界或条件值，参与本方法声明的筛选与授权范围。
    /// - `p6`：当前接口的边界或条件值，参与本方法声明的筛选与授权范围。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO proxy_validators(proxy_id, name, upstream, etag, modified, body) SELECT id, #{p2}, #{p3}, #{p4}, #{p5}, #{p6} FROM proxy_repositories WHERE name = #{p1} ON CONFLICT(proxy_id, name, upstream) DO UPDATE SET etag = excluded.etag, modified = excluded.modified, body = excluded.body")]
    async fn save_upstream_validation_insert_proxy_validators(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
        p5: &Value,
        p6: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 proxy_missing 读取 proxy_missing 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    /// - `p2`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM proxy_missing WHERE proxy_id = (SELECT id FROM proxy_repositories WHERE name = #{p1}) AND name = #{p2} AND expires_at > unixepoch())")]
    async fn proxy_missing_query_proxy_missing(&self, p1: &Value, p2: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 save_proxy_missing 持久化 proxy_missing 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM proxy_missing WHERE rowid IN (SELECT rowid FROM proxy_missing WHERE expires_at <= unixepoch() LIMIT 256)")]
    async fn save_proxy_missing_delete_proxy_missing(&self) -> Result<u64>;

    /// 业务作用：为 save_proxy_missing 读取 proxy_missing 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT count(*) FROM proxy_missing")]
    async fn save_proxy_missing_query_proxy_missing(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 save_proxy_missing 持久化 proxy_missing 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    /// - `p2`：当前接口的边界或条件值，参与本方法声明的筛选与授权范围。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO proxy_missing(proxy_id, name, expires_at) SELECT id, #{p2}, unixepoch() + 30 FROM proxy_repositories WHERE name = #{p1} ON CONFLICT(proxy_id, name) DO UPDATE SET expires_at = excluded.expires_at")]
    async fn save_proxy_missing_insert_proxy_missing(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 commit_materialization 读取 proxy_sessions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`actor` 对应的经过认证的操作主体。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM proxy_sessions WHERE id = #{p1} AND actor = #{p2} AND expires_at > unixepoch())")]
    async fn commit_materialization_query_proxy_sessions(
        &self,
        p1: &Value,
        p2: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 commit_materialization 读取 proxy_session_entries 的仓库状态。
    /// 参数说明：
    /// - `p1`：`session_id` 对应的固定解析视图身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT body FROM proxy_session_entries WHERE session_id = #{p1} AND name = #{p2}")]
    async fn commit_materialization_query_proxy_session_entries(
        &self,
        p1: &Value,
        p2: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 commit_materialization 读取 proxy_objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`proxy_id` 对应的公开来源的稳定身份。
    /// - `p2`：`digest` 对应的完整字节或凭据摘要。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM proxy_objects WHERE proxy_id = #{p1} AND digest = #{p2})")]
    async fn commit_materialization_query_proxy_objects(
        &self,
        p1: &Value,
        p2: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 commit_materialization 读取 proxy_materializations 的仓库状态。
    /// 参数说明：
    /// - `p1`：`proxy_id` 对应的公开来源的稳定身份。
    /// - `p2`：`actor` 对应的经过认证的操作主体。
    /// - `p3`：`fingerprint` 对应的预览选集及保护状态的摘要。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id FROM proxy_materializations WHERE proxy_id = #{p1} AND actor = #{p2} AND fingerprint = #{p3} AND valid = 1")]
    async fn commit_materialization_query_proxy_materializations(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 commit_materialization 持久化 proxy_materializations 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`proxy_id` 对应的公开来源的稳定身份。
    /// - `p3`：`actor` 对应的经过认证的操作主体。
    /// - `p4`：`fingerprint` 对应的预览选集及保护状态的摘要。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT OR IGNORE INTO proxy_materializations(id, proxy_id, actor, fingerprint) VALUES (#{p1}, #{p2}, #{p3}, #{p4})")]
    async fn commit_materialization_insert_proxy_materializations(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 commit_materialization 持久化 materialization_items 的仓库状态。
    /// 参数说明：
    /// - `p1`：`materialization_id` 对应的公开依赖物化集合身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    /// - `p4`：`digest` 对应的完整字节或凭据摘要。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT OR IGNORE INTO materialization_items(materialization_id, name, version, digest) VALUES (#{p1}, #{p2}, #{p3}, #{p4})")]
    async fn commit_materialization_insert_materialization_items(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 proxy_session 读取 proxy_sessions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`actor` 对应的经过认证的操作主体。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT s.id, p.name, p.id, s.mode, s.expires_at, s.upstream_start\n             FROM proxy_sessions s JOIN proxy_repositories p ON p.id = s.proxy_id\n             WHERE s.id = #{p1} AND p.name = #{p2} AND s.actor = #{p3} AND s.expires_at > unixepoch()")]
    async fn proxy_session_query_proxy_sessions(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 upstream_validation 读取 proxy_validators 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`upstream` 对应的固定镜像的来源身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT etag, modified, body FROM proxy_validators\n             WHERE proxy_id = (SELECT id FROM proxy_repositories WHERE name = #{p1})\n               AND name = #{p2} AND upstream = #{p3}")]
    async fn upstream_validation_query_proxy_validators(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<Vec<Row>>;
}

#[Mapper(datasource = "naxus", cache = false)]
trait RepositoryMapper {
    /// 业务作用：为 open 持久化 metadata 的仓库状态。
    /// 参数说明：
    /// - `p1`：`value` 对应的元数据键对应的持久值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO metadata(key, value) VALUES ('service_id', #{p1})")]
    async fn open_insert_metadata(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 open 读取 metadata 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT value FROM metadata WHERE key = 'service_id'")]
    async fn open_query_metadata(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 create_repository 读取 repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT policy, head FROM repositories WHERE name = #{p1}")]
    async fn create_repository_query_repositories(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 create_repository 持久化 repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`policy` 对应的仓库是否允许同版本更新。
    /// - `p4`：`head` 对应的已观察到的仓库 HEAD。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert(
        "INSERT INTO repositories(id, name, policy, head) VALUES (#{p1}, #{p2}, #{p3}, #{p4})"
    )]
    async fn create_repository_insert_repositories(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 create_repository 持久化 audit 的仓库状态。
    /// 参数说明：
    /// - `p1`：`repository_id` 对应的仓库稳定身份。
    /// - `p2`：`snapshot_id` 对应的快照稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO audit(actor, action, repository_id, snapshot_id) VALUES ('system', 'create_repository', #{p1}, #{p2})")]
    async fn create_repository_insert_audit(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 install_validated 读取 objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`sha256` 对应的完整字节摘要。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT state FROM objects WHERE sha256 = #{p1}")]
    async fn install_validated_query_objects(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 install_validated 持久化 objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`sha256` 对应的完整字节摘要。
    /// - `p2`：`size` 对应的对象字节数。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO objects(sha256, size, installed_at) VALUES (#{p1}, #{p2}, unixepoch()) ON CONFLICT(sha256) DO UPDATE SET size = excluded.size, generation = CASE WHEN objects.state = 'purged' THEN objects.generation + 1 ELSE objects.generation END, state = 'present', installed_at = unixepoch()")]
    async fn install_validated_insert_objects(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 commit_inner 读取 repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id, policy, head FROM repositories WHERE name = #{p1} OR id = #{p1}")]
    async fn commit_inner_query_repositories(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 commit_inner 读取 access_tokens 的仓库状态。
    /// 参数说明：
    /// - `p1`：`digest` 对应的完整字节或凭据摘要。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT allow_overwrite, publish_crates FROM access_tokens WHERE digest = #{p1}")]
    async fn commit_inner_query_access_tokens(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 commit_inner 读取 objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`sha256` 对应的完整字节摘要。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT state FROM objects WHERE sha256 = #{p1}")]
    async fn commit_inner_query_objects(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 commit_inner 读取 commits 的仓库状态。
    /// 参数说明：
    /// - `p1`：`repository_id` 对应的仓库稳定身份。
    /// - `p2`：`idempotency_key` 对应的固定业务意图的幂等身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT request_sha256, snapshot_id FROM commits WHERE repository_id = #{p1} AND idempotency_key = #{p2}")]
    async fn commit_inner_query_commits(&self, p1: &Value, p2: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 commit_inner 读取 crate_names 的仓库状态。
    /// 参数说明：
    /// - `p1`：`repository_id` 对应的仓库稳定身份。
    /// - `p2`：`canonical` 对应的防止名称歧义的规范包名。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT name FROM crate_names WHERE repository_id = #{p1} AND canonical = #{p2}")]
    async fn commit_inner_query_crate_names(&self, p1: &Value, p2: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 commit_inner 读取 revisions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`repository_id` 对应的仓库稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM revisions WHERE repository_id = #{p1} AND name = #{p2} AND version = #{p3})")]
    async fn commit_inner_query_revisions(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 commit_inner 读取 revisions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`repository_id` 对应的仓库稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT entry_json FROM revisions WHERE repository_id = #{p1} AND name = #{p2} AND version = #{p3} LIMIT 1")]
    async fn commit_inner_query_revisions_2(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 commit_inner 持久化 crate_names 的仓库状态。
    /// 参数说明：
    /// - `p1`：`repository_id` 对应的仓库稳定身份。
    /// - `p2`：`canonical` 对应的防止名称歧义的规范包名。
    /// - `p3`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT OR IGNORE INTO crate_names(repository_id, canonical, name) VALUES (#{p1}, #{p2}, #{p3})")]
    async fn commit_inner_insert_crate_names(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 commit_inner 持久化 objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`sha256` 对应的完整字节摘要。
    /// - `p2`：`size` 对应的对象字节数。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT OR IGNORE INTO objects(sha256, size) VALUES (#{p1}, #{p2})")]
    async fn commit_inner_insert_objects(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 commit_inner 持久化 revisions 的仓库状态。
    /// 参数说明：
    /// - `values`：不可变修订的仓库坐标、内容摘要、索引和审计主体。
    ///
    /// 返回：确认完成的影响行数；约束或来源失效时返回错误。
    #[Insert("INSERT INTO revisions(id, repository_id, name, version, sha256, metadata_sha256, entry_json, actor) VALUES (#{values.id}, #{values.repository_id}, #{values.name}, #{values.version}, #{values.sha256}, #{values.metadata_sha256}, #{values.entry_json}, #{values.actor})")]
    async fn commit_inner_insert_revisions(&self, values: RevisionWrite<'_>) -> Result<u64>;

    /// 业务作用：为 commit_inner 持久化 snapshot_changes 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    /// - `p4`：`revision_id` 对应的不可变修订身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO snapshot_changes(snapshot_id, name, version, revision_id) VALUES (#{p1}, #{p2}, #{p3}, #{p4})")]
    async fn commit_inner_insert_snapshot_changes(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 commit_inner 持久化 repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`head` 对应的已观察到的仓库 HEAD。
    /// - `p2`：`id` 对应的记录的稳定身份。
    /// - `p3`：`head` 对应的已观察到的仓库 HEAD。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE repositories SET head = #{p1} WHERE id = #{p2} AND head = #{p3}")]
    async fn commit_inner_update_repositories(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 commit_inner 持久化 commits 的仓库状态。
    /// 参数说明：
    /// - `p1`：`repository_id` 对应的仓库稳定身份。
    /// - `p2`：`idempotency_key` 对应的固定业务意图的幂等身份。
    /// - `p3`：`request_sha256` 对应的完整提交意图摘要。
    /// - `p4`：`snapshot_id` 对应的快照稳定身份。
    /// - `p5`：`actor` 对应的经过认证的操作主体。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO commits(repository_id, idempotency_key, request_sha256, snapshot_id, actor) VALUES (#{p1}, #{p2}, #{p3}, #{p4}, #{p5})")]
    async fn commit_inner_insert_commits(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
        p5: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 commit_inner 持久化 audit 的仓库状态。
    /// 参数说明：
    /// - `p1`：`actor` 对应的经过认证的操作主体。
    /// - `p2`：`repository_id` 对应的仓库稳定身份。
    /// - `p3`：`snapshot_id` 对应的快照稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO audit(actor, action, repository_id, snapshot_id) VALUES (#{p1}, 'commit', #{p2}, #{p3})")]
    async fn commit_inner_insert_audit(&self, p1: &Value, p2: &Value, p3: &Value) -> Result<u64>;

    /// 业务作用：为 commit_inner 持久化 publish_batches 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    /// - `p2`：`id` 对应的记录的稳定身份。
    /// - `p3`：`repository_id` 对应的仓库稳定身份。
    /// - `p4`：`actor` 对应的经过认证的操作主体。
    /// - `p5`：`expected_head` 对应的发布时固定的预期 HEAD。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE publish_batches SET state = 'committed', snapshot_id = #{p1} WHERE id = #{p2} AND repository_id = #{p3} AND actor = #{p4} AND expected_head = #{p5} AND state = 'validating'")]
    async fn commit_inner_update_publish_batches(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
        p5: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 head 读取 repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT head FROM repositories WHERE name = #{p1} OR id = #{p1}")]
    async fn head_query_repositories(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 repositories 读取 repositories 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id, name, policy, head FROM repositories ORDER BY name")]
    async fn repositories_query_repositories(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 snapshot 读取 repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id FROM repositories WHERE name = #{p1} OR id = #{p1}")]
    async fn snapshot_query_repositories(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 index_entries 读取 crate_names 的仓库状态。
    /// 参数说明：
    /// - `p1`：`repository_id` 对应的仓库稳定身份。
    /// - `p2`：`canonical` 对应的防止名称歧义的规范包名。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT name FROM crate_names WHERE repository_id = #{p1} AND canonical = #{p2}")]
    async fn index_entries_query_crate_names(&self, p1: &Value, p2: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 index_entries 读取 revisions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`repository_id` 对应的仓库稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT DISTINCT version FROM revisions WHERE repository_id = #{p1} AND name = #{p2}")]
    async fn index_entries_query_revisions(&self, p1: &Value, p2: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 contains_object 读取 artifact_access_revocations 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    /// - `p2`：`sha256` 对应的完整字节摘要。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM artifact_access_revocations a JOIN revisions v ON v.id = a.revision_id JOIN repositories r ON r.id = v.repository_id WHERE (r.name = #{p1} OR r.id = #{p1}) AND v.sha256 = #{p2})")]
    async fn contains_object_query_artifact_access_revocations(
        &self,
        p1: &Value,
        p2: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 open_snapshot_object 读取 snapshots 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM snapshots s JOIN repositories r ON r.id = s.repository_id WHERE s.id = #{p1} AND (r.id = #{p2} OR r.name = #{p2}))")]
    async fn open_snapshot_object_query_snapshots(
        &self,
        p1: &Value,
        p2: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 open_snapshot_object 读取 artifact_access_revocations 的仓库状态。
    /// 参数说明：
    /// - `p1`：`revision_id` 对应的不可变修订身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM artifact_access_revocations WHERE revision_id = #{p1})")]
    async fn open_snapshot_object_query_artifact_access_revocations(
        &self,
        p1: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 open_reader 读取 objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`sha256` 对应的完整字节摘要。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT state FROM objects WHERE sha256 = #{p1}")]
    async fn open_reader_query_objects(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 check_integrity 读取 repositories 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM repositories r LEFT JOIN snapshots s ON s.id = r.head WHERE s.id IS NULL OR s.repository_id <> r.id) OR EXISTS(SELECT 1 FROM snapshots s JOIN snapshots p ON p.id = s.parent_id WHERE s.repository_id <> p.repository_id) OR EXISTS(SELECT 1 FROM snapshot_changes c JOIN snapshots s ON s.id = c.snapshot_id JOIN revisions r ON r.id = c.revision_id WHERE r.repository_id <> s.repository_id OR r.name <> c.name OR r.version <> c.version) OR EXISTS(SELECT 1 FROM checkpoint_entries c JOIN snapshots s ON s.id = c.snapshot_id JOIN revisions r ON r.id = c.revision_id WHERE r.repository_id <> s.repository_id OR r.name <> c.name OR r.version <> c.version)")]
    async fn check_integrity_query_repositories(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 check_integrity 读取 revisions 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM revisions r JOIN objects o ON o.sha256 = r.sha256 WHERE o.state != 'present' AND NOT EXISTS(SELECT 1 FROM artifact_access_revocations a WHERE a.revision_id = r.id)) OR EXISTS(SELECT 1 FROM proxy_objects p JOIN objects o ON o.sha256 = p.digest WHERE o.state != 'present') OR EXISTS(SELECT 1 FROM proxy_materializations m JOIN materialization_items i ON i.materialization_id = m.id WHERE m.valid = 1 AND NOT EXISTS(SELECT 1 FROM proxy_objects p JOIN objects o ON o.sha256 = p.digest WHERE p.proxy_id = m.proxy_id AND p.digest = i.digest AND o.state = 'present'))")]
    async fn check_integrity_query_revisions(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 check_integrity 读取 objects 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT sha256, size, state, generation FROM objects")]
    async fn check_integrity_query_objects(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 check_integrity 读取 deletion_items 的仓库状态。
    /// 参数说明：
    /// - `p1`：`digest` 对应的完整字节或凭据摘要。
    /// - `p2`：`generation` 对应的对象的持久存储代次。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM deletion_items i JOIN object_deletions d ON d.id = i.deletion_id WHERE i.digest = #{p1} AND i.generation = #{p2} AND i.state = 'purged' AND d.state = 'completed')")]
    async fn check_integrity_query_deletion_items(
        &self,
        p1: &Value,
        p2: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 check_integrity 读取 revisions 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT name, version, sha256, metadata_sha256, entry_json FROM revisions")]
    async fn check_integrity_query_revisions_2(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 insert_snapshot 持久化 snapshots 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`repository_id` 对应的仓库稳定身份。
    /// - `p3`：`parent_id` 对应的父快照身份。
    /// - `p4`：`manifest_sha256` 对应的完整快照内容摘要。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO snapshots(id, repository_id, parent_id, manifest_sha256) VALUES (#{p1}, #{p2}, #{p3}, #{p4})")]
    async fn insert_snapshot_insert_snapshots(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 read_snapshot 读取 snapshots 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT repository_id, parent_id, manifest_sha256 FROM snapshots WHERE id = #{p1}")]
    async fn read_snapshot_query_snapshots(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 read_entry 读取 snapshot_removals 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM snapshot_removals WHERE snapshot_id = #{p1} AND name = #{p2} AND version = #{p3})")]
    async fn read_entry_query_snapshot_removals(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 read_entry 读取 snapshot_changes 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    /// - `p4`：`repository_id` 对应的仓库稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT r.entry_json FROM snapshot_changes c JOIN revisions r ON r.id = c.revision_id WHERE c.snapshot_id = #{p1} AND c.name = #{p2} AND c.version = #{p3} AND r.repository_id = #{p4}")]
    async fn read_entry_query_snapshot_changes(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 read_entry 读取 snapshot_checkpoints 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM snapshot_checkpoints WHERE snapshot_id = #{p1})")]
    async fn read_entry_query_snapshot_checkpoints(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 read_entry 读取 checkpoint_entries 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    /// - `p4`：`repository_id` 对应的仓库稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT r.entry_json FROM checkpoint_entries c JOIN revisions r ON r.id = c.revision_id WHERE c.snapshot_id = #{p1} AND c.name = #{p2} AND c.version = #{p3} AND r.repository_id = #{p4}")]
    async fn read_entry_query_checkpoint_entries(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 materialize_checkpoint 读取 snapshot_removals 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT name, version FROM snapshot_removals WHERE snapshot_id = #{p1}")]
    async fn materialize_checkpoint_query_snapshot_removals(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 materialize_checkpoint 读取 snapshot_changes 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT name, version, revision_id FROM snapshot_changes WHERE snapshot_id = #{p1}")]
    async fn materialize_checkpoint_query_snapshot_changes(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 materialize_checkpoint 读取 snapshot_checkpoints 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM snapshot_checkpoints WHERE snapshot_id = #{p1})")]
    async fn materialize_checkpoint_query_snapshot_checkpoints(
        &self,
        p1: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 materialize_checkpoint 读取 checkpoint_entries 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT name, version, revision_id FROM checkpoint_entries WHERE snapshot_id = #{p1}")]
    async fn materialize_checkpoint_query_checkpoint_entries(&self, p1: &Value)
        -> Result<Vec<Row>>;

    /// 业务作用：为 materialize_checkpoint 持久化 checkpoint_entries 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    /// - `p4`：`revision_id` 对应的不可变修订身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO checkpoint_entries(snapshot_id, name, version, revision_id) VALUES (#{p1}, #{p2}, #{p3}, #{p4})")]
    async fn materialize_checkpoint_insert_checkpoint_entries(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 materialize_checkpoint 持久化 snapshot_checkpoints 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO snapshot_checkpoints(snapshot_id) VALUES (#{p1})")]
    async fn materialize_checkpoint_insert_snapshot_checkpoints(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 contains_object 读取 revisions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    /// - `p2`：`sha256` 对应的完整字节摘要。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM revisions v JOIN repositories r ON r.id = v.repository_id\n             WHERE (r.name = #{p1} OR r.id = #{p1}) AND v.sha256 = #{p2}\n               AND NOT EXISTS(SELECT 1 FROM artifact_access_revocations a WHERE a.revision_id = v.id))")]
    async fn contains_object_query_revisions(&self, p1: &Value, p2: &Value) -> Result<Vec<Row>>;
}

#[Mapper(datasource = "naxus", cache = false)]
trait ResolutionMapper {
    /// 业务作用：为 resolve_at 读取 snapshots 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT s.id FROM snapshots s JOIN repositories r ON r.id = s.repository_id WHERE s.id = #{p1} AND (r.name = #{p2} OR r.id = #{p2})")]
    async fn resolve_at_query_snapshots(&self, p1: &Value, p2: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 resolve_at 读取 repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT head FROM repositories WHERE name = #{p1} OR id = #{p1}")]
    async fn resolve_at_query_repositories(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 resolve_at 读取 artifact_access_revocations 的仓库状态。
    /// 参数说明：
    /// - `p1`：`revision_id` 对应的不可变修订身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM artifact_access_revocations WHERE revision_id = #{p1})")]
    async fn resolve_at_query_artifact_access_revocations(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 resolve_at 持久化 retention_leases 的仓库状态。
    /// 参数说明：
    /// - `p1`：`expires_at` 对应的保护或授权的截止时刻。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM retention_leases WHERE expires_at <= #{p1}")]
    async fn resolve_at_delete_retention_leases(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 resolve_at 读取 retention_leases 的仓库状态。
    /// 参数说明：
    /// - `p1`：`actor` 对应的经过认证的操作主体。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT count(*), coalesce(sum(actor = #{p1}), 0) FROM retention_leases")]
    async fn resolve_at_query_retention_leases(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 resolve_at 持久化 retention_leases 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`snapshot_set_id` 对应的整组解析快照的稳定身份。
    /// - `p3`：`snapshot_id` 对应的快照稳定身份。
    /// - `p4`：`actor` 对应的经过认证的操作主体。
    /// - `p5`：`expires_at` 对应的保护或授权的截止时刻。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO retention_leases(id, snapshot_set_id, snapshot_id, actor, expires_at) VALUES (#{p1}, #{p2}, #{p3}, #{p4}, #{p5})")]
    async fn resolve_at_insert_retention_leases(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
        p5: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 renew_lease_proof 读取 retention_leases 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`actor` 对应的经过认证的操作主体。
    /// - `p3`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query(
        sql = "UPDATE retention_leases SET expires_at = unixepoch() + 600 WHERE id = #{p1} AND actor = #{p2} AND expires_at > unixepoch() AND snapshot_id IN (SELECT s.id FROM snapshots s JOIN repositories r ON r.id = s.repository_id WHERE r.id = #{p3} OR r.name = #{p3}) RETURNING snapshot_set_id, snapshot_id, expires_at",
        cache = false,
        flush_cache = true
    )]
    async fn renew_lease_proof_update_retention_leases(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 release_lease 持久化 retention_leases 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`actor` 对应的经过认证的操作主体。
    /// - `p3`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM retention_leases WHERE id = #{p1} AND actor = #{p2} AND snapshot_id IN (SELECT s.id FROM snapshots s JOIN repositories r ON r.id = s.repository_id WHERE r.id = #{p3} OR r.name = #{p3})")]
    async fn release_lease_delete_retention_leases(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 revision_at 读取 snapshot_removals 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM snapshot_removals WHERE snapshot_id = #{p1} AND name = #{p2} AND version = #{p3})")]
    async fn revision_at_query_snapshot_removals(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 revision_at 读取 snapshot_changes 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT revision_id FROM snapshot_changes WHERE snapshot_id = #{p1} AND name = #{p2} AND version = #{p3}")]
    async fn revision_at_query_snapshot_changes(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 revision_at 读取 snapshot_checkpoints 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM snapshot_checkpoints WHERE snapshot_id = #{p1})")]
    async fn revision_at_query_snapshot_checkpoints(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 revision_at 读取 checkpoint_entries 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    /// - `p3`：`version` 对应的包的精确版本。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT revision_id FROM checkpoint_entries WHERE snapshot_id = #{p1} AND name = #{p2} AND version = #{p3}")]
    async fn revision_at_query_checkpoint_entries(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 revision_at 读取 revisions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`repository_id` 对应的仓库稳定身份。
    /// - `p3`：`name` 对应的业务名称或包名称。
    /// - `p4`：`version` 对应的包的精确版本。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT entry_json, metadata_sha256, actor FROM revisions WHERE id = #{p1} AND repository_id = #{p2} AND name = #{p3} AND version = #{p4}")]
    async fn revision_at_query_revisions(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<Vec<Row>>;
}

#[Mapper(datasource = "naxus", cache = false)]
trait RetentionMapper {
    /// 业务作用：为 retention_pins 读取 retention_pins 的仓库状态。
    /// 参数说明：
    /// - `p1`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT p.id, p.snapshot_id, p.actor, p.reason FROM retention_pins p JOIN snapshots s ON s.id = p.snapshot_id JOIN repositories r ON r.id = s.repository_id WHERE r.name = #{p1} OR r.id = #{p1} ORDER BY p.rowid DESC LIMIT 4096")]
    async fn retention_pins_query_retention_pins(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 pin_snapshot 读取 repositories 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id FROM repositories WHERE id = #{p1} OR name = #{p1}")]
    async fn pin_snapshot_query_repositories(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 pin_snapshot 读取 retention_pins 的仓库状态。
    /// 参数说明：
    /// - `p1`：`actor` 对应的经过认证的操作主体。
    /// - `p2`：`snapshot_id` 对应的快照稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id FROM retention_pins WHERE actor = #{p1} AND snapshot_id = #{p2} LIMIT 1")]
    async fn pin_snapshot_query_retention_pins(&self, p1: &Value, p2: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 pin_snapshot 读取 retention_pins 的仓库状态。
    /// 参数说明：
    /// - `p1`：`actor` 对应的经过认证的操作主体。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT count(*), coalesce(sum(actor = #{p1}), 0) FROM retention_pins")]
    async fn pin_snapshot_query_retention_pins_2(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 pin_snapshot 读取 revisions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM revisions r JOIN objects o ON o.sha256 = r.sha256 WHERE r.id = #{p1} AND o.state = 'present' AND NOT EXISTS(SELECT 1 FROM artifact_access_revocations a WHERE a.revision_id = r.id))")]
    async fn pin_snapshot_query_revisions(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 pin_snapshot 持久化 retention_pins 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`snapshot_id` 对应的快照稳定身份。
    /// - `p3`：`actor` 对应的经过认证的操作主体。
    /// - `p4`：`reason` 对应的保留或管理动作的业务原因。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO retention_pins(id, snapshot_id, actor, reason) VALUES (#{p1}, #{p2}, #{p3}, #{p4})")]
    async fn pin_snapshot_insert_retention_pins(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 pin_snapshot 持久化 security_audit 的仓库状态。
    /// 参数说明：
    /// - `p1`：`actor` 对应的经过认证的操作主体。
    /// - `p2`：`target` 对应的业务筛选或持久状态值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert(
        "INSERT INTO security_audit(actor, action, target) VALUES (#{p1}, 'pin-snapshot', #{p2})"
    )]
    async fn pin_snapshot_insert_security_audit(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 release_pin 读取 retention_pins 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`name` 对应的业务名称或包名称。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT r.id FROM retention_pins p JOIN snapshots s ON s.id = p.snapshot_id JOIN repositories r ON r.id = s.repository_id WHERE p.id = #{p1} AND (r.name = #{p2} OR r.id = #{p2})")]
    async fn release_pin_query_retention_pins(&self, p1: &Value, p2: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 release_pin 持久化 retention_pins 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM retention_pins WHERE id = #{p1}")]
    async fn release_pin_delete_retention_pins(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 release_pin 持久化 security_audit 的仓库状态。
    /// 参数说明：
    /// - `p1`：`actor` 对应的经过认证的操作主体。
    /// - `p2`：`target` 对应的业务筛选或持久状态值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert(
        "INSERT INTO security_audit(actor, action, target) VALUES (#{p1}, 'release-pin', #{p2})"
    )]
    async fn release_pin_insert_security_audit(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 snapshot_revisions 读取 snapshot_removals 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT name, version FROM snapshot_removals WHERE snapshot_id = #{p1} LIMIT 4097")]
    async fn snapshot_revisions_query_snapshot_removals(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 snapshot_revisions 读取 snapshot_checkpoints 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM snapshot_checkpoints WHERE snapshot_id = #{p1})")]
    async fn snapshot_revisions_query_snapshot_checkpoints(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 snapshot_revisions 读取 snapshot_changes 的仓库状态。
    /// 参数说明：
    /// - `p1`：`snapshot_id` 对应的快照稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT name, version, revision_id, 0 FROM snapshot_changes WHERE snapshot_id = #{p1} UNION ALL SELECT name, version, revision_id, 1 FROM checkpoint_entries WHERE snapshot_id = #{p1} ORDER BY 4, 1, 2 LIMIT 8193")]
    async fn snapshot_revisions_query_snapshot_changes(&self, p1: &Value) -> Result<Vec<Row>>;
}

#[Mapper(datasource = "naxus", cache = false)]
trait SchemaMapper {
    /// 业务作用：为 revision 读取 sqlite_schema 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'metadata')"
    )]
    async fn revision_query_sqlite_schema(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 revision 读取 metadata 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT value FROM metadata WHERE key = 'schema_revision'")]
    async fn revision_query_metadata(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 repository 持久化 metadata 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE metadata (\n    key TEXT PRIMARY KEY,\n    value TEXT NOT NULL\n)")]
    async fn repository_create_metadata(&self) -> Result<u64>;

    /// 业务作用：为 repository 持久化 repositories 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE repositories (\n    id TEXT PRIMARY KEY,\n    name TEXT NOT NULL UNIQUE,\n    policy TEXT NOT NULL CHECK (policy IN ('mutable', 'immutable')),\n    head TEXT NOT NULL\n)")]
    async fn repository_create_repositories(&self) -> Result<u64>;

    /// 业务作用：为 repository 持久化 snapshots 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE snapshots (\n    id TEXT PRIMARY KEY,\n    repository_id TEXT NOT NULL REFERENCES repositories(id),\n    parent_id TEXT REFERENCES snapshots(id),\n    manifest_sha256 TEXT NOT NULL\n)")]
    async fn repository_create_snapshots(&self) -> Result<u64>;

    /// 业务作用：为 repository 持久化 objects 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE objects (\n    sha256 TEXT PRIMARY KEY,\n    size INTEGER NOT NULL CHECK (size >= 0)\n)")]
    async fn repository_create_objects(&self) -> Result<u64>;

    /// 业务作用：为 repository 持久化 crate_names 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE crate_names (\n    repository_id TEXT NOT NULL REFERENCES repositories(id),\n    canonical TEXT NOT NULL,\n    name TEXT NOT NULL,\n    PRIMARY KEY (repository_id, canonical)\n)")]
    async fn repository_create_crate_names(&self) -> Result<u64>;

    /// 业务作用：为 repository 持久化 revisions 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE revisions (\n    id TEXT PRIMARY KEY,\n    repository_id TEXT NOT NULL REFERENCES repositories(id),\n    name TEXT NOT NULL,\n    version TEXT NOT NULL,\n    sha256 TEXT NOT NULL REFERENCES objects(sha256),\n    metadata_sha256 TEXT NOT NULL,\n    entry_json TEXT NOT NULL,\n    actor TEXT NOT NULL\n)")]
    async fn repository_create_revisions(&self) -> Result<u64>;

    /// 业务作用：为 repository 持久化 snapshot_changes 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE snapshot_changes (\n    snapshot_id TEXT NOT NULL REFERENCES snapshots(id),\n    name TEXT NOT NULL,\n    version TEXT NOT NULL,\n    revision_id TEXT NOT NULL REFERENCES revisions(id),\n    PRIMARY KEY (snapshot_id, name, version)\n)")]
    async fn repository_create_snapshot_changes(&self) -> Result<u64>;

    /// 业务作用：为 repository 持久化 commits 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE commits (\n    repository_id TEXT NOT NULL REFERENCES repositories(id),\n    idempotency_key TEXT NOT NULL,\n    request_sha256 TEXT NOT NULL,\n    snapshot_id TEXT NOT NULL REFERENCES snapshots(id),\n    actor TEXT NOT NULL,\n    PRIMARY KEY (repository_id, idempotency_key)\n)")]
    async fn repository_create_commits(&self) -> Result<u64>;

    /// 业务作用：为 repository 持久化 audit 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE audit (\n    sequence INTEGER PRIMARY KEY AUTOINCREMENT,\n    actor TEXT NOT NULL,\n    action TEXT NOT NULL,\n    repository_id TEXT NOT NULL,\n    snapshot_id TEXT NOT NULL,\n    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))\n)")]
    async fn repository_create_audit(&self) -> Result<u64>;

    /// 业务作用：为 repository 持久化 value 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE INDEX revision_coordinates ON revisions(repository_id, name, version)")]
    async fn repository_create_value(&self) -> Result<u64>;

    /// 业务作用：为 repository 持久化 metadata 的仓库状态。
    /// 参数说明：
    /// - `p1`：`value` 对应的元数据键对应的持久值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO metadata(key, value) VALUES ('schema_revision', #{p1}) ON CONFLICT(key) DO UPDATE SET value = excluded.value")]
    async fn repository_insert_metadata(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 snapshot_checkpoints 持久化 snapshot_checkpoints 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE snapshot_checkpoints (\n    snapshot_id TEXT PRIMARY KEY REFERENCES snapshots(id)\n)")]
    async fn snapshot_checkpoints_create_snapshot_checkpoints(&self) -> Result<u64>;

    /// 业务作用：为 snapshot_checkpoints 持久化 checkpoint_entries 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE checkpoint_entries (\n    snapshot_id TEXT NOT NULL REFERENCES snapshots(id),\n    name TEXT NOT NULL,\n    version TEXT NOT NULL,\n    revision_id TEXT NOT NULL REFERENCES revisions(id),\n    PRIMARY KEY (snapshot_id, name, version)\n)")]
    async fn snapshot_checkpoints_create_checkpoint_entries(&self) -> Result<u64>;

    /// 业务作用：为 snapshot_checkpoints 持久化 metadata 的仓库状态。
    /// 参数说明：
    /// - `p1`：`value` 对应的元数据键对应的持久值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO metadata(key, value) VALUES ('schema_revision', #{p1}) ON CONFLICT(key) DO UPDATE SET value = excluded.value")]
    async fn snapshot_checkpoints_insert_metadata(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 publish_batches 持久化 publish_batches 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE publish_batches (\n    id TEXT PRIMARY KEY,\n    repository_id TEXT NOT NULL REFERENCES repositories(id),\n    actor TEXT NOT NULL,\n    idempotency_key TEXT NOT NULL,\n    expected_head TEXT NOT NULL REFERENCES snapshots(id),\n    state TEXT NOT NULL CHECK (state IN ('open', 'validating', 'committed', 'aborted', 'expired')),\n    snapshot_id TEXT REFERENCES snapshots(id),\n    expires_at INTEGER NOT NULL,\n    UNIQUE (repository_id, actor, idempotency_key)\n)")]
    async fn publish_batches_create_publish_batches(&self) -> Result<u64>;

    /// 业务作用：为 publish_batches 持久化 batch_items 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE batch_items (\n    batch_id TEXT NOT NULL REFERENCES publish_batches(id),\n    name TEXT NOT NULL,\n    version TEXT NOT NULL,\n    entry_json TEXT NOT NULL,\n    PRIMARY KEY (batch_id, name, version)\n)")]
    async fn publish_batches_create_batch_items(&self) -> Result<u64>;

    /// 业务作用：为 publish_batches 持久化 value 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE INDEX open_batches ON publish_batches(actor, state, expires_at)")]
    async fn publish_batches_create_value(&self) -> Result<u64>;

    /// 业务作用：为 publish_batches 持久化 metadata 的仓库状态。
    /// 参数说明：
    /// - `p1`：`value` 对应的元数据键对应的持久值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO metadata(key, value) VALUES ('schema_revision', #{p1}) ON CONFLICT(key) DO UPDATE SET value = excluded.value")]
    async fn publish_batches_insert_metadata(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 retention_leases 持久化 retention_leases 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE retention_leases (\n    id TEXT PRIMARY KEY,\n    snapshot_set_id TEXT NOT NULL,\n    snapshot_id TEXT NOT NULL REFERENCES snapshots(id),\n    actor TEXT NOT NULL,\n    expires_at INTEGER NOT NULL\n)")]
    async fn retention_leases_create_retention_leases(&self) -> Result<u64>;

    /// 业务作用：为 retention_leases 持久化 value 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE INDEX lease_expiry ON retention_leases(expires_at)")]
    async fn retention_leases_create_value(&self) -> Result<u64>;

    /// 业务作用：为 retention_leases 持久化 metadata 的仓库状态。
    /// 参数说明：
    /// - `p1`：`value` 对应的元数据键对应的持久值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO metadata(key, value) VALUES ('schema_revision', #{p1}) ON CONFLICT(key) DO UPDATE SET value = excluded.value")]
    async fn retention_leases_insert_metadata(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 proxy_cache 持久化 proxy_repositories 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE proxy_repositories (\n    id TEXT PRIMARY KEY,\n    name TEXT NOT NULL UNIQUE,\n    logical_index TEXT NOT NULL\n)")]
    async fn proxy_cache_create_proxy_repositories(&self) -> Result<u64>;

    /// 业务作用：为 proxy_cache 持久化 proxy_indexes 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE proxy_indexes (\n    proxy_id TEXT NOT NULL REFERENCES proxy_repositories(id),\n    name TEXT NOT NULL,\n    body BLOB NOT NULL,\n    checked_at INTEGER NOT NULL,\n    PRIMARY KEY (proxy_id, name)\n)")]
    async fn proxy_cache_create_proxy_indexes(&self) -> Result<u64>;

    /// 业务作用：为 proxy_cache 持久化 proxy_coordinates 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE proxy_coordinates (\n    proxy_id TEXT NOT NULL REFERENCES proxy_repositories(id),\n    name TEXT NOT NULL,\n    version TEXT NOT NULL,\n    digest TEXT NOT NULL,\n    PRIMARY KEY (proxy_id, name, version)\n)")]
    async fn proxy_cache_create_proxy_coordinates(&self) -> Result<u64>;

    /// 业务作用：为 proxy_cache 持久化 proxy_objects 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE proxy_objects (\n    proxy_id TEXT NOT NULL REFERENCES proxy_repositories(id),\n    digest TEXT NOT NULL REFERENCES objects(sha256),\n    PRIMARY KEY (proxy_id, digest)\n)")]
    async fn proxy_cache_create_proxy_objects(&self) -> Result<u64>;

    /// 业务作用：为 proxy_cache 持久化 metadata 的仓库状态。
    /// 参数说明：
    /// - `p1`：`value` 对应的元数据键对应的持久值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO metadata(key, value) VALUES ('schema_revision', #{p1}) ON CONFLICT(key) DO UPDATE SET value = excluded.value")]
    async fn proxy_cache_insert_metadata(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 proxy_resolution 持久化 proxy_sessions 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE proxy_sessions (\n    id TEXT PRIMARY KEY,\n    proxy_id TEXT NOT NULL REFERENCES proxy_repositories(id),\n    actor TEXT NOT NULL,\n    mode TEXT NOT NULL CHECK (mode IN ('cache-first', 'revalidate')),\n    expires_at INTEGER NOT NULL\n)")]
    async fn proxy_resolution_create_proxy_sessions(&self) -> Result<u64>;

    /// 业务作用：为 proxy_resolution 持久化 proxy_session_entries 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE proxy_session_entries (\n    session_id TEXT NOT NULL REFERENCES proxy_sessions(id) ON DELETE CASCADE,\n    name TEXT NOT NULL,\n    body BLOB NOT NULL,\n    PRIMARY KEY (session_id, name)\n)")]
    async fn proxy_resolution_create_proxy_session_entries(&self) -> Result<u64>;

    /// 业务作用：为 proxy_resolution 持久化 proxy_validators 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE proxy_validators (\n    proxy_id TEXT NOT NULL REFERENCES proxy_repositories(id),\n    name TEXT NOT NULL,\n    upstream TEXT NOT NULL,\n    etag TEXT,\n    modified TEXT,\n    body BLOB NOT NULL,\n    PRIMARY KEY (proxy_id, name, upstream)\n)")]
    async fn proxy_resolution_create_proxy_validators(&self) -> Result<u64>;

    /// 业务作用：为 proxy_resolution 持久化 proxy_missing 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE proxy_missing (\n    proxy_id TEXT NOT NULL REFERENCES proxy_repositories(id),\n    name TEXT NOT NULL,\n    expires_at INTEGER NOT NULL,\n    PRIMARY KEY (proxy_id, name)\n)")]
    async fn proxy_resolution_create_proxy_missing(&self) -> Result<u64>;

    /// 业务作用：为 proxy_resolution 持久化 proxy_materializations 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE proxy_materializations (\n    id TEXT PRIMARY KEY,\n    proxy_id TEXT NOT NULL REFERENCES proxy_repositories(id),\n    actor TEXT NOT NULL,\n    fingerprint TEXT NOT NULL,\n    valid INTEGER NOT NULL DEFAULT 1\n)")]
    async fn proxy_resolution_create_proxy_materializations(&self) -> Result<u64>;

    /// 业务作用：为 proxy_resolution 持久化 value 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE UNIQUE INDEX valid_materialization_graph ON proxy_materializations(proxy_id, actor, fingerprint) WHERE valid = 1")]
    async fn proxy_resolution_create_value(&self) -> Result<u64>;

    /// 业务作用：为 proxy_resolution 持久化 materialization_items 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE materialization_items (\n    materialization_id TEXT NOT NULL REFERENCES proxy_materializations(id),\n    name TEXT NOT NULL,\n    version TEXT NOT NULL,\n    digest TEXT NOT NULL,\n    PRIMARY KEY (materialization_id, name, version)\n)")]
    async fn proxy_resolution_create_materialization_items(&self) -> Result<u64>;

    /// 业务作用：为 proxy_resolution 持久化 metadata 的仓库状态。
    /// 参数说明：
    /// - `p1`：`value` 对应的元数据键对应的持久值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO metadata(key, value) VALUES ('schema_revision', #{p1}) ON CONFLICT(key) DO UPDATE SET value = excluded.value")]
    async fn proxy_resolution_insert_metadata(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 access_control 持久化 admin_users 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE admin_users (\n    id TEXT PRIMARY KEY,\n    username TEXT NOT NULL UNIQUE,\n    password_hash TEXT NOT NULL,\n    role TEXT NOT NULL CHECK (role IN ('admin', 'operator', 'developer', 'reader')),\n    repositories TEXT NOT NULL,\n    enabled INTEGER NOT NULL DEFAULT 1,\n    must_change INTEGER NOT NULL DEFAULT 0,\n    auth_version INTEGER NOT NULL DEFAULT 1,\n    failures INTEGER NOT NULL DEFAULT 0,\n    not_before INTEGER NOT NULL DEFAULT 0\n)")]
    async fn access_control_create_admin_users(&self) -> Result<u64>;

    /// 业务作用：为 access_control 持久化 admin_sessions 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE admin_sessions (\n    digest TEXT PRIMARY KEY,\n    user_id TEXT NOT NULL REFERENCES admin_users(id),\n    csrf_digest TEXT NOT NULL,\n    auth_version INTEGER NOT NULL,\n    expires_at INTEGER NOT NULL\n)")]
    async fn access_control_create_admin_sessions(&self) -> Result<u64>;

    /// 业务作用：为 access_control 持久化 access_tokens 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE access_tokens (\n    id TEXT PRIMARY KEY,\n    digest TEXT NOT NULL UNIQUE,\n    user_id TEXT NOT NULL REFERENCES admin_users(id),\n    label TEXT NOT NULL,\n    permission TEXT NOT NULL CHECK (permission IN ('read', 'publish')),\n    repositories TEXT NOT NULL,\n    auth_version INTEGER NOT NULL,\n    expires_at INTEGER NOT NULL,\n    revoked INTEGER NOT NULL DEFAULT 0\n)")]
    async fn access_control_create_access_tokens(&self) -> Result<u64>;

    /// 业务作用：为 access_control 持久化 security_audit 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE security_audit (\n    sequence INTEGER PRIMARY KEY AUTOINCREMENT,\n    actor TEXT NOT NULL,\n    action TEXT NOT NULL,\n    target TEXT NOT NULL,\n    created_at INTEGER NOT NULL DEFAULT (unixepoch())\n)")]
    async fn access_control_create_security_audit(&self) -> Result<u64>;

    /// 业务作用：为 access_control 持久化 metadata 的仓库状态。
    /// 参数说明：
    /// - `p1`：`value` 对应的元数据键对应的持久值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO metadata(key, value) VALUES ('schema_revision', #{p1}) ON CONFLICT(key) DO UPDATE SET value = excluded.value")]
    async fn access_control_insert_metadata(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 hosted_management 持久化 snapshot_removals 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE snapshot_removals (\n    snapshot_id TEXT NOT NULL REFERENCES snapshots(id),\n    name TEXT NOT NULL,\n    version TEXT NOT NULL,\n    PRIMARY KEY (snapshot_id, name, version)\n)")]
    async fn hosted_management_create_snapshot_removals(&self) -> Result<u64>;

    /// 业务作用：为 hosted_management 持久化 management_previews 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE management_previews (\n    id TEXT PRIMARY KEY,\n    actor TEXT NOT NULL REFERENCES admin_users(id),\n    repository_id TEXT NOT NULL REFERENCES repositories(id),\n    expected_head TEXT NOT NULL REFERENCES snapshots(id),\n    operation TEXT NOT NULL,\n    selection TEXT NOT NULL,\n    fingerprint TEXT NOT NULL,\n    expires_at INTEGER NOT NULL,\n    state TEXT NOT NULL DEFAULT 'prepared',\n    commit_key TEXT,\n    committed_snapshot TEXT REFERENCES snapshots(id)\n)")]
    async fn hosted_management_create_management_previews(&self) -> Result<u64>;

    /// 业务作用：为 hosted_management 持久化 metadata 的仓库状态。
    /// 参数说明：
    /// - `p1`：`value` 对应的元数据键对应的持久值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO metadata(key, value) VALUES ('schema_revision', #{p1}) ON CONFLICT(key) DO UPDATE SET value = excluded.value")]
    async fn hosted_management_insert_metadata(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 object_lifecycle 持久化 objects 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("ALTER TABLE objects ADD COLUMN state TEXT NOT NULL DEFAULT 'present' CHECK (state IN ('present', 'deleting', 'purged'))")]
    async fn object_lifecycle_alter_objects(&self) -> Result<u64>;

    /// 业务作用：为 object_lifecycle 持久化 objects 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("ALTER TABLE objects ADD COLUMN generation INTEGER NOT NULL DEFAULT 1")]
    async fn object_lifecycle_alter_objects_2(&self) -> Result<u64>;

    /// 业务作用：为 object_lifecycle 持久化 artifact_access_revocations 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE artifact_access_revocations (\n    revision_id TEXT PRIMARY KEY REFERENCES revisions(id),\n    deletion_id TEXT NOT NULL,\n    actor TEXT NOT NULL,\n    created_at INTEGER NOT NULL DEFAULT (unixepoch())\n)")]
    async fn object_lifecycle_create_artifact_access_revocations(&self) -> Result<u64>;

    /// 业务作用：为 object_lifecycle 持久化 retention_pins 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE retention_pins (\n    id TEXT PRIMARY KEY,\n    snapshot_id TEXT NOT NULL REFERENCES snapshots(id),\n    actor TEXT NOT NULL,\n    reason TEXT NOT NULL\n)")]
    async fn object_lifecycle_create_retention_pins(&self) -> Result<u64>;

    /// 业务作用：为 object_lifecycle 持久化 deletion_previews 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE deletion_previews (\n    id TEXT PRIMARY KEY,\n    actor TEXT NOT NULL REFERENCES admin_users(id),\n    scope TEXT NOT NULL,\n    fingerprint TEXT NOT NULL,\n    expires_at INTEGER NOT NULL,\n    state TEXT NOT NULL DEFAULT 'prepared',\n    task_id TEXT\n)")]
    async fn object_lifecycle_create_deletion_previews(&self) -> Result<u64>;

    /// 业务作用：为 object_lifecycle 持久化 object_deletions 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE object_deletions (\n    id TEXT PRIMARY KEY,\n    actor TEXT NOT NULL,\n    scope TEXT NOT NULL,\n    state TEXT NOT NULL CHECK (state IN ('accepted', 'running', 'completed', 'failed')),\n    released_bytes INTEGER NOT NULL DEFAULT 0,\n    created_at INTEGER NOT NULL DEFAULT (unixepoch())\n)")]
    async fn object_lifecycle_create_object_deletions(&self) -> Result<u64>;

    /// 业务作用：为 object_lifecycle 持久化 deletion_items 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE deletion_items (\n    deletion_id TEXT NOT NULL REFERENCES object_deletions(id),\n    digest TEXT NOT NULL REFERENCES objects(sha256),\n    generation INTEGER NOT NULL,\n    size INTEGER NOT NULL,\n    state TEXT NOT NULL CHECK (state IN ('deleting', 'purged', 'shared')),\n    PRIMARY KEY (deletion_id, digest)\n)")]
    async fn object_lifecycle_create_deletion_items(&self) -> Result<u64>;

    /// 业务作用：为 object_lifecycle 持久化 proxy_session_objects 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE TABLE proxy_session_objects (\n    session_id TEXT NOT NULL REFERENCES proxy_sessions(id) ON DELETE CASCADE,\n    name TEXT NOT NULL,\n    digest TEXT NOT NULL,\n    PRIMARY KEY (session_id, name, digest)\n)")]
    async fn object_lifecycle_create_proxy_session_objects(&self) -> Result<u64>;

    /// 业务作用：为 object_lifecycle 持久化 metadata 的仓库状态。
    /// 参数说明：
    /// - `p1`：`value` 对应的元数据键对应的持久值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO metadata(key, value) VALUES ('schema_revision', #{p1}) ON CONFLICT(key) DO UPDATE SET value = excluded.value")]
    async fn object_lifecycle_insert_metadata(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 proxy_views 持久化 proxy_sessions 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("ALTER TABLE proxy_sessions ADD COLUMN upstream_start INTEGER NOT NULL DEFAULT 0 CHECK (upstream_start BETWEEN 0 AND 15)")]
    async fn proxy_views_alter_proxy_sessions(&self) -> Result<u64>;

    /// 业务作用：为 proxy_views 持久化 metadata 的仓库状态。
    /// 参数说明：
    /// - `p1`：`value` 对应的元数据键对应的持久值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO metadata(key, value) VALUES ('schema_revision', #{p1}) ON CONFLICT(key) DO UPDATE SET value = excluded.value")]
    async fn proxy_views_insert_metadata(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 deletion_retries 持久化 object_deletions 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("ALTER TABLE object_deletions ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0")]
    async fn deletion_retries_alter_object_deletions(&self) -> Result<u64>;

    /// 业务作用：为 deletion_retries 持久化 object_deletions 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("ALTER TABLE object_deletions ADD COLUMN next_attempt_at INTEGER NOT NULL DEFAULT 0")]
    async fn deletion_retries_alter_object_deletions_2(&self) -> Result<u64>;

    /// 业务作用：为 deletion_retries 持久化 object_deletions 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("ALTER TABLE object_deletions ADD COLUMN last_error_code TEXT")]
    async fn deletion_retries_alter_object_deletions_3(&self) -> Result<u64>;

    /// 业务作用：为 deletion_retries 持久化 value 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE INDEX deletion_retry_due ON object_deletions(state, next_attempt_at)")]
    async fn deletion_retries_create_value(&self) -> Result<u64>;

    /// 业务作用：为 deletion_retries 持久化 metadata 的仓库状态。
    /// 参数说明：
    /// - `p1`：`value` 对应的元数据键对应的持久值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO metadata(key, value) VALUES ('schema_revision', #{p1}) ON CONFLICT(key) DO UPDATE SET value = excluded.value")]
    async fn deletion_retries_insert_metadata(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 token_publish_scope 持久化 access_tokens 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("ALTER TABLE access_tokens ADD COLUMN allow_overwrite INTEGER NOT NULL DEFAULT 0")]
    async fn token_publish_scope_alter_access_tokens(&self) -> Result<u64>;

    /// 业务作用：为 token_publish_scope 持久化 access_tokens 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("ALTER TABLE access_tokens ADD COLUMN publish_crates TEXT NOT NULL DEFAULT '[]'")]
    async fn token_publish_scope_alter_access_tokens_2(&self) -> Result<u64>;

    /// 业务作用：为 token_publish_scope 持久化 metadata 的仓库状态。
    /// 参数说明：
    /// - `p1`：`value` 对应的元数据键对应的持久值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO metadata(key, value) VALUES ('schema_revision', #{p1}) ON CONFLICT(key) DO UPDATE SET value = excluded.value")]
    async fn token_publish_scope_insert_metadata(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 unpublished_objects 持久化 objects 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("ALTER TABLE objects ADD COLUMN installed_at INTEGER NOT NULL DEFAULT 0")]
    async fn unpublished_objects_alter_objects(&self) -> Result<u64>;

    /// 业务作用：为 unpublished_objects 持久化 objects 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE objects SET installed_at = unixepoch()")]
    async fn unpublished_objects_update_objects(&self) -> Result<u64>;

    /// 业务作用：为 unpublished_objects 持久化 value 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE INDEX object_installation_age ON objects(state, installed_at)")]
    async fn unpublished_objects_create_value(&self) -> Result<u64>;

    /// 业务作用：为 unpublished_objects 持久化 value 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE INDEX revision_object_references ON revisions(sha256)")]
    async fn unpublished_objects_create_value_2(&self) -> Result<u64>;

    /// 业务作用：为 unpublished_objects 持久化 value 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE INDEX proxy_object_references ON proxy_objects(digest)")]
    async fn unpublished_objects_create_value_3(&self) -> Result<u64>;

    /// 业务作用：为 unpublished_objects 持久化 value 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE INDEX session_object_references ON proxy_session_objects(digest)")]
    async fn unpublished_objects_create_value_4(&self) -> Result<u64>;

    /// 业务作用：为 unpublished_objects 持久化 value 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute("CREATE INDEX materialization_object_references ON materialization_items(digest)")]
    async fn unpublished_objects_create_value_5(&self) -> Result<u64>;

    /// 业务作用：为 unpublished_objects 持久化 value 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Execute(
        "CREATE INDEX batch_object_references ON batch_items(json_extract(entry_json, '$.cksum'))"
    )]
    async fn unpublished_objects_create_value_6(&self) -> Result<u64>;

    /// 业务作用：为 unpublished_objects 持久化 metadata 的仓库状态。
    /// 参数说明：
    /// - `p1`：`value` 对应的元数据键对应的持久值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO metadata(key, value) VALUES ('schema_revision', #{p1}) ON CONFLICT(key) DO UPDATE SET value = excluded.value")]
    async fn unpublished_objects_insert_metadata(&self, p1: &Value) -> Result<u64>;
}

#[Mapper(datasource = "naxus", cache = false)]
trait SecurityMapper {
    /// 业务作用：为 tokens 读取 access_tokens 的仓库状态。
    /// 参数说明：
    /// - `p1`：`user_id` 对应的账户稳定身份。
    /// - `p2`：当前接口的边界或条件值，参与本方法声明的筛选与授权范围。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id, user_id, label, permission, repositories, expires_at, revoked, allow_overwrite, publish_crates FROM access_tokens WHERE user_id = #{p1} OR #{p2} = 'admin' ORDER BY expires_at DESC LIMIT 4096")]
    async fn tokens_query_access_tokens(&self, p1: &Value, p2: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 security_audit 读取 security_audit 的仓库状态。
    /// 参数说明：
    /// - `p1`：`sequence` 对应的审计分页位置。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT sequence, actor, action, target, created_at FROM security_audit WHERE sequence > #{p1} ORDER BY sequence LIMIT 200")]
    async fn security_audit_query_security_audit(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 security_audit_page 读取 security_audit 的仓库状态。
    /// 参数说明：
    /// - `p1`：`sequence` 对应的审计分页位置。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT sequence, actor, action, target, created_at FROM security_audit WHERE #{p1} = 0 OR sequence < #{p1} ORDER BY sequence DESC LIMIT 200")]
    async fn security_audit_page_query_security_audit(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 set_user_grants 读取 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM admin_users WHERE id = #{p1} AND role = 'admin' AND enabled = 1 AND must_change = 0)")]
    async fn set_user_grants_query_admin_users(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 set_user_grants 读取 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT role, enabled FROM admin_users WHERE id = #{p1}")]
    async fn set_user_grants_query_admin_users_2(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 set_user_grants 读取 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：当前接口的边界或条件值，参与本方法声明的筛选与授权范围。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query(
        "SELECT count(*) FROM admin_users WHERE role = 'admin' AND enabled = 1 AND id != #{p1}"
    )]
    async fn set_user_grants_query_admin_users_3(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 set_user_grants 持久化 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：`role` 对应的业务筛选或持久状态值。
    /// - `p2`：`repositories` 对应的受允许的仓库范围。
    /// - `p3`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE admin_users SET role = #{p1}, repositories = #{p2}, auth_version = auth_version + 1 WHERE id = #{p3}")]
    async fn set_user_grants_update_admin_users(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 set_user_grants 持久化 security_audit 的仓库状态。
    /// 参数说明：
    /// - `p1`：`actor` 对应的经过认证的操作主体。
    /// - `p2`：`target` 对应的业务筛选或持久状态值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO security_audit(actor, action, target) VALUES (#{p1}, 'change-user-grants', #{p2})")]
    async fn set_user_grants_insert_security_audit(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 set_user_enabled 读取 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM admin_users WHERE id = #{p1} AND role = 'admin' AND enabled = 1 AND must_change = 0)")]
    async fn set_user_enabled_query_admin_users(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 set_user_enabled 读取 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：当前接口的边界或条件值，参与本方法声明的筛选与授权范围。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query(
        "SELECT count(*) FROM admin_users WHERE role = 'admin' AND enabled = 1 AND id != #{p1}"
    )]
    async fn set_user_enabled_query_admin_users_2(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 set_user_enabled 读取 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT role = 'admin' FROM admin_users WHERE id = #{p1}")]
    async fn set_user_enabled_query_admin_users_3(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 set_user_enabled 持久化 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：`enabled` 对应的账户是否允许使用。
    /// - `p2`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update(
        "UPDATE admin_users SET enabled = #{p1}, auth_version = auth_version + 1 WHERE id = #{p2}"
    )]
    async fn set_user_enabled_update_admin_users(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 set_user_enabled 持久化 security_audit 的仓库状态。
    /// 参数说明：
    /// - `p1`：`actor` 对应的经过认证的操作主体。
    /// - `p2`：`action` 对应的业务筛选或持久状态值。
    /// - `p3`：`target` 对应的业务筛选或持久状态值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO security_audit(actor, action, target) VALUES (#{p1}, #{p2}, #{p3})")]
    async fn set_user_enabled_insert_security_audit(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 bootstrap_status 读取 admin_users 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT count(*), coalesce(sum(must_change), 0) FROM admin_users")]
    async fn bootstrap_status_query_admin_users(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 bootstrap_admin 读取 admin_users 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT count(*) FROM admin_users")]
    async fn bootstrap_admin_query_admin_users(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 bootstrap_admin 持久化 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`password_hash` 对应的口令校验散列。
    /// - `p3`：`repositories` 对应的受允许的仓库范围。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO admin_users(id, username, password_hash, role, repositories, must_change) VALUES (#{p1}, 'admin', #{p2}, 'admin', #{p3}, 1)")]
    async fn bootstrap_admin_insert_admin_users(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 bootstrap_admin 持久化 security_audit 的仓库状态。
    /// 参数说明：
    /// - `p1`：`target` 对应的业务筛选或持久状态值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO security_audit(actor, action, target) VALUES ('bootstrap', 'create_administrator', #{p1})")]
    async fn bootstrap_admin_insert_security_audit(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 finish_login 持久化 admin_sessions 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM admin_sessions WHERE digest IN (SELECT digest FROM admin_sessions WHERE expires_at <= unixepoch() LIMIT 256)")]
    async fn finish_login_delete_admin_sessions(&self) -> Result<u64>;

    /// 业务作用：为 finish_login 读取 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`auth_version` 对应的当前账户授权代次。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM admin_users WHERE id = #{p1} AND auth_version = #{p2} AND enabled = 1 AND not_before <= unixepoch())")]
    async fn finish_login_query_admin_users(&self, p1: &Value, p2: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 finish_login 读取 admin_sessions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`user_id` 对应的账户稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT count(*), coalesce(sum(user_id = #{p1}), 0) FROM admin_sessions")]
    async fn finish_login_query_admin_sessions(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 finish_login 持久化 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE admin_users SET failures = 0, not_before = 0 WHERE id = #{p1}")]
    async fn finish_login_update_admin_users(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 finish_login 持久化 admin_sessions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`digest` 对应的完整字节或凭据摘要。
    /// - `p2`：`user_id` 对应的账户稳定身份。
    /// - `p3`：`csrf_digest` 对应的同源写请求的 CSRF 校验摘要。
    /// - `p4`：`auth_version` 对应的当前账户授权代次。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO admin_sessions(digest, user_id, csrf_digest, auth_version, expires_at) VALUES (#{p1}, #{p2}, #{p3}, #{p4}, unixepoch() + 28800)")]
    async fn finish_login_insert_admin_sessions(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 finish_login 持久化 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`auth_version` 对应的当前账户授权代次。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE admin_users SET failures = min(failures + 1, 15), not_before = CASE WHEN failures >= 4 THEN unixepoch() + min(900, (1 << min(failures - 3, 10))) ELSE not_before END WHERE id = #{p1} AND auth_version = #{p2}")]
    async fn finish_login_update_admin_users_2(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 finish_login 持久化 security_audit 的仓库状态。
    /// 参数说明：
    /// - `p1`：`target` 对应的业务筛选或持久状态值。
    /// - `p2`：`action` 对应的业务筛选或持久状态值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO security_audit(actor, action, target) VALUES (#{p1}, #{p2}, #{p1})")]
    async fn finish_login_insert_security_audit(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 browser_identity 读取 admin_sessions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`digest` 对应的完整字节或凭据摘要。
    /// - `p2`：`csrf_digest` 对应的同源写请求的 CSRF 校验摘要。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT u.id, u.username, u.role, u.repositories, u.must_change, u.auth_version FROM admin_sessions s JOIN admin_users u ON u.id = s.user_id WHERE s.digest = #{p1} AND s.expires_at > unixepoch() AND s.auth_version = u.auth_version AND u.enabled = 1 AND (#{p2} IS NULL OR s.csrf_digest = #{p2})")]
    async fn browser_identity_query_admin_sessions(
        &self,
        p1: &Value,
        p2: &Value,
    ) -> Result<Vec<Row>>;

    /// 业务作用：为 logout 持久化 admin_sessions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`digest` 对应的完整字节或凭据摘要。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM admin_sessions WHERE digest = #{p1}")]
    async fn logout_delete_admin_sessions(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 users 读取 admin_users 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id, username, role, repositories, enabled, must_change FROM admin_users ORDER BY username LIMIT 4096")]
    async fn users_query_admin_users(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 create_user 读取 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM admin_users WHERE id = #{p1} AND role = 'admin' AND enabled = 1 AND must_change = 0)")]
    async fn create_user_query_admin_users(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 create_user 读取 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：`username` 对应的账户登录名。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM admin_users WHERE username = #{p1})")]
    async fn create_user_query_admin_users_2(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 create_user 读取 admin_users 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT count(*) FROM admin_users")]
    async fn create_user_query_admin_users_3(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 create_user 持久化 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`username` 对应的账户登录名。
    /// - `p3`：`password_hash` 对应的口令校验散列。
    /// - `p4`：`role` 对应的业务筛选或持久状态值。
    /// - `p5`：`repositories` 对应的受允许的仓库范围。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO admin_users(id, username, password_hash, role, repositories) VALUES (#{p1}, #{p2}, #{p3}, #{p4}, #{p5})")]
    async fn create_user_insert_admin_users(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
        p5: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 create_user 持久化 security_audit 的仓库状态。
    /// 参数说明：
    /// - `p1`：`actor` 对应的经过认证的操作主体。
    /// - `p2`：`target` 对应的业务筛选或持久状态值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert(
        "INSERT INTO security_audit(actor, action, target) VALUES (#{p1}, 'create_user', #{p2})"
    )]
    async fn create_user_insert_security_audit(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 change_password 持久化 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：`password_hash` 对应的口令校验散列。
    /// - `p2`：`id` 对应的记录的稳定身份。
    /// - `p3`：`auth_version` 对应的当前账户授权代次。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE admin_users SET password_hash = #{p1}, must_change = 0, auth_version = auth_version + 1, failures = 0, not_before = 0 WHERE id = #{p2} AND auth_version = #{p3} AND enabled = 1")]
    async fn change_password_update_admin_users(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 change_password 持久化 admin_sessions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`user_id` 对应的账户稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Delete("DELETE FROM admin_sessions WHERE user_id = #{p1}")]
    async fn change_password_delete_admin_sessions(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 change_password 持久化 security_audit 的仓库状态。
    /// 参数说明：
    /// - `p1`：`target` 对应的业务筛选或持久状态值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO security_audit(actor, action, target) VALUES (#{p1}, 'change_password', #{p1})")]
    async fn change_password_insert_security_audit(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 create_scoped_token 读取 access_tokens 的仓库状态。
    /// 参数说明：
    /// - `p1`：`user_id` 对应的账户稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT count(*) FROM access_tokens WHERE user_id = #{p1} AND revoked = 0 AND expires_at > unixepoch()")]
    async fn create_scoped_token_query_access_tokens(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 create_scoped_token 持久化 access_tokens 的仓库状态。
    /// 参数说明：
    /// - `values`：受限发布令牌的账户身份、授权范围和到期策略。
    ///
    /// 返回：确认完成的影响行数；约束或来源失效时返回错误。
    #[Insert("INSERT INTO access_tokens(id, digest, user_id, label, permission, repositories, auth_version, expires_at, allow_overwrite, publish_crates) VALUES (#{values.id}, #{values.digest}, #{values.user_id}, #{values.label}, #{values.permission}, #{values.repositories}, #{values.auth_version}, unixepoch() + #{values.expires_at}, #{values.allow_overwrite}, #{values.publish_crates})")]
    async fn create_scoped_token_insert_access_tokens(&self, values: TokenWrite<'_>)
        -> Result<u64>;

    /// 业务作用：为 create_scoped_token 持久化 security_audit 的仓库状态。
    /// 参数说明：
    /// - `p1`：`actor` 对应的经过认证的操作主体。
    /// - `p2`：`target` 对应的业务筛选或持久状态值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert(
        "INSERT INTO security_audit(actor, action, target) VALUES (#{p1}, 'create_token', #{p2})"
    )]
    async fn create_scoped_token_insert_security_audit(
        &self,
        p1: &Value,
        p2: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 revoke_token 读取 access_tokens 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT user_id FROM access_tokens WHERE id = #{p1}")]
    async fn revoke_token_query_access_tokens(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 revoke_token 持久化 access_tokens 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE access_tokens SET revoked = 1 WHERE id = #{p1}")]
    async fn revoke_token_update_access_tokens(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 revoke_token 持久化 security_audit 的仓库状态。
    /// 参数说明：
    /// - `p1`：`actor` 对应的经过认证的操作主体。
    /// - `p2`：`target` 对应的业务筛选或持久状态值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert(
        "INSERT INTO security_audit(actor, action, target) VALUES (#{p1}, 'revoke_token', #{p2})"
    )]
    async fn revoke_token_insert_security_audit(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 browser_actor 读取 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`auth_version` 对应的当前账户授权代次。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT role, repositories FROM admin_users WHERE id = #{p1} AND auth_version = #{p2} AND enabled = 1 AND must_change = 0")]
    async fn browser_actor_query_admin_users(&self, p1: &Value, p2: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 token_identity_on 读取 access_tokens 的仓库状态。
    /// 参数说明：
    /// - `p1`：`digest` 对应的完整字节或凭据摘要。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT u.id, u.username, u.role, u.repositories, t.repositories, t.permission = 'publish', u.auth_version FROM access_tokens t JOIN admin_users u ON u.id = t.user_id WHERE t.digest = #{p1} AND t.revoked = 0 AND t.expires_at > unixepoch() AND t.auth_version = u.auth_version AND u.enabled = 1 AND u.must_change = 0")]
    async fn token_identity_on_query_access_tokens(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 login_user 读取 admin_users 的仓库状态。
    /// 参数说明：
    /// - `p1`：`username` 对应的账户登录名。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT id, password_hash, auth_version, enabled = 1 AND not_before <= unixepoch()\n             FROM admin_users WHERE username = #{p1}")]
    async fn login_user_query_admin_users(&self, p1: &Value) -> Result<Vec<Row>>;
}

#[Mapper(datasource = "naxus", cache = false)]
trait UnpublishedMapper {
    /// 业务作用：为 collect_unpublished 读取 object_deletions 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT count(*) FROM object_deletions WHERE state IN ('accepted', 'running')")]
    async fn collect_unpublished_query_object_deletions(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 collect_unpublished 读取 proxy_session_entries 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT EXISTS(SELECT 1 FROM proxy_session_entries e JOIN proxy_sessions s ON s.id = e.session_id WHERE s.expires_at > unixepoch() AND NOT EXISTS(SELECT 1 FROM proxy_session_objects o WHERE o.session_id = e.session_id AND o.name = e.name))")]
    async fn collect_unpublished_query_proxy_session_entries(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 collect_unpublished 读取 objects 的仓库状态。
    /// 参数说明: 无。
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT o.sha256, o.generation, o.size FROM objects o WHERE o.state = 'present' AND o.installed_at <= unixepoch() - 3600 AND NOT EXISTS(SELECT 1 FROM revisions r WHERE r.sha256 = o.sha256) AND NOT EXISTS(SELECT 1 FROM proxy_objects p WHERE p.digest = o.sha256) AND NOT EXISTS(SELECT 1 FROM materialization_items i JOIN proxy_materializations m ON m.id = i.materialization_id WHERE i.digest = o.sha256 AND m.valid = 1) AND NOT EXISTS(SELECT 1 FROM batch_items i JOIN publish_batches b ON b.id = i.batch_id WHERE json_extract(i.entry_json, '$.cksum') = o.sha256 AND (b.state = 'validating' OR (b.state = 'open' AND b.expires_at > unixepoch()))) AND NOT EXISTS(SELECT 1 FROM proxy_session_objects p JOIN proxy_sessions s ON s.id = p.session_id WHERE p.digest = o.sha256 AND s.expires_at > unixepoch()) ORDER BY o.installed_at, o.sha256 LIMIT 64")]
    async fn collect_unpublished_query_objects(&self) -> Result<Vec<Row>>;

    /// 业务作用：为 collect_unpublished 持久化 object_deletions 的仓库状态。
    /// 参数说明：
    /// - `p1`：`id` 对应的记录的稳定身份。
    /// - `p2`：`scope` 对应的已经验证的管理范围。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO object_deletions(id, actor, scope, state) VALUES (#{p1}, 'maintenance', #{p2}, 'accepted')")]
    async fn collect_unpublished_insert_object_deletions(
        &self,
        p1: &Value,
        p2: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 collect_unpublished 持久化 objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`sha256` 对应的完整字节摘要。
    /// - `p2`：`generation` 对应的对象的持久存储代次。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE objects SET state = 'deleting' WHERE sha256 = #{p1} AND generation = #{p2} AND state = 'present'")]
    async fn collect_unpublished_update_objects(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 collect_unpublished 持久化 deletion_items 的仓库状态。
    /// 参数说明：
    /// - `p1`：`deletion_id` 对应的持久清除任务身份。
    /// - `p2`：`digest` 对应的完整字节或凭据摘要。
    /// - `p3`：`generation` 对应的对象的持久存储代次。
    /// - `p4`：`size` 对应的对象字节数。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO deletion_items(deletion_id, digest, generation, size, state) VALUES (#{p1}, #{p2}, #{p3}, #{p4}, 'deleting')")]
    async fn collect_unpublished_insert_deletion_items(
        &self,
        p1: &Value,
        p2: &Value,
        p3: &Value,
        p4: &Value,
    ) -> Result<u64>;

    /// 业务作用：为 collect_unpublished 持久化 security_audit 的仓库状态。
    /// 参数说明：
    /// - `p1`：`target` 对应的业务筛选或持久状态值。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO security_audit(actor, action, target) VALUES ('maintenance', 'collect-unpublished', #{p1})")]
    async fn collect_unpublished_insert_security_audit(&self, p1: &Value) -> Result<u64>;

    /// 业务作用：为 recover_unpublished 读取 objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`sha256` 对应的完整字节摘要。
    ///
    /// 返回：受来源和事务门禁保护的完整结果；读取失败返回错误。
    #[Query("SELECT size, state FROM objects WHERE sha256 = #{p1}")]
    async fn recover_unpublished_query_objects(&self, p1: &Value) -> Result<Vec<Row>>;

    /// 业务作用：为 recover_unpublished 持久化 objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`sha256` 对应的完整字节摘要。
    /// - `p2`：`size` 对应的对象字节数。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Insert("INSERT INTO objects(sha256, size, installed_at) VALUES (#{p1}, #{p2}, unixepoch())")]
    async fn recover_unpublished_insert_objects(&self, p1: &Value, p2: &Value) -> Result<u64>;

    /// 业务作用：为 recover_unpublished 持久化 objects 的仓库状态。
    /// 参数说明：
    /// - `p1`：`sha256` 对应的完整字节摘要。
    ///
    /// 返回：受来源和事务门禁保护的影响行数；约束或持久化失败返回错误。
    #[Update("UPDATE objects SET generation = generation + 1, state = 'present', installed_at = unixepoch() WHERE sha256 = #{p1} AND state = 'purged'")]
    async fn recover_unpublished_update_objects(&self, p1: &Value) -> Result<u64>;
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Statement {
    BackupBackupQueryObjects,
    BatchesBeginBatchQueryRepositories,
    BatchesBeginBatchQueryPublishBatches,
    BatchesBeginBatchUpdatePublishBatches,
    BatchesBeginBatchQueryPublishBatches2,
    BatchesBeginBatchInsertPublishBatches,
    BatchesBatchQueryRepositories,
    BatchesAddBatchItemQueryBatchItems,
    BatchesAddBatchItemInsertBatchItems,
    BatchesCommitBatchAuthorizedUpdatePublishBatches,
    BatchesCommitBatchAuthorizedUpdatePublishBatches2,
    BatchesAbortBatchUpdatePublishBatches,
    BatchesRecoverBatchesUpdatePublishBatches,
    BatchesRecoverBatchesUpdatePublishBatches2,
    BatchesReadBatchQueryPublishBatches,
    BatchesReadBatchQueryBatchItems,
    DeletionDeletionTasksQueryObjectDeletions,
    DeletionPendingDeletionQueryObjectDeletions,
    DeletionDeferDeletionUpdateObjectDeletions,
    DeletionRetryDeletionQueryAdminUsers,
    DeletionRetryDeletionUpdateObjectDeletions,
    DeletionRetryDeletionInsertSecurityAudit,
    DeletionPreviewDeletionDeleteDeletionPreviews,
    DeletionPreviewDeletionQueryDeletionPreviews,
    DeletionPreviewDeletionInsertDeletionPreviews,
    DeletionAcceptDeletionQueryDeletionPreviews,
    DeletionAcceptDeletionQueryObjectDeletions,
    DeletionAcceptDeletionInsertObjectDeletions,
    DeletionAcceptDeletionInsertArtifactAccessRevocations,
    DeletionAcceptDeletionUpdateProxyMaterializations,
    DeletionAcceptDeletionDeleteProxyObjects,
    DeletionAcceptDeletionInsertDeletionItems,
    DeletionAcceptDeletionUpdateObjects,
    DeletionAcceptDeletionDeleteProxyIndexes,
    DeletionAcceptDeletionDeleteProxyValidators,
    DeletionAcceptDeletionDeleteProxyMissing,
    DeletionAcceptDeletionUpdateDeletionPreviews,
    DeletionAcceptDeletionInsertSecurityAudit,
    DeletionRunDeletionQueryDeletionItems,
    DeletionRunDeletionUpdateObjectDeletions,
    DeletionRunDeletionQueryDeletionItems2,
    DeletionRunDeletionQueryObjects,
    DeletionRunDeletionUpdateObjects,
    DeletionRunDeletionUpdateDeletionItems,
    DeletionRunDeletionUpdateObjectDeletions2,
    DeletionRunDeletionQueryDeletionItems3,
    DeletionRunDeletionUpdateObjectDeletions3,
    DeletionRecoverDeletionsQueryObjectDeletions,
    DeletionDeletionViewQueryRevisions,
    DeletionDeletionViewQueryProxyCoordinates,
    DeletionDeletionViewQueryBatchItems,
    DeletionDeletionViewQueryProxySessionObjects,
    DeletionDeletionViewQueryRevisions2,
    DeletionDeletionViewQueryProxyObjects,
    DeletionDeletionViewQueryValue,
    DeletionDeletionPermissionQueryAdminUsers,
    DeletionDeletionPermissionQueryRepositories,
    DeletionDeletionPermissionQueryProxyRepositories,
    DeletionDeletionPermissionQueryRevisions,
    DeletionProtectRevisionQueryRevisions,
    DeletionProtectRevisionQueryRepositories,
    DeletionReadTaskQueryObjectDeletions,
    DeletionReadTaskQueryObjectDeletions2,
    DeletionReadTaskQueryObjectDeletions3,
    DeletionReadTaskQueryObjectDeletions4,
    MaintenanceMaintainQueryMetadata,
    MaintenanceMaintainUpdatePublishBatches,
    MaintenanceMaintainDeleteProxySessions,
    MaintenanceMaintainDeleteRetentionLeases,
    MaintenanceMaintainDeleteAdminSessions,
    MaintenanceMaintainDeleteProxyMissing,
    MaintenanceMaintainDeleteManagementPreviews,
    MaintenanceMaintainDeleteDeletionPreviews,
    MaintenanceMaintainQueryObjects,
    ManagementCatalogQueryRepositories,
    ManagementCatalogQueryRevisions,
    ManagementPreviewHostedQueryRepositories,
    ManagementPreviewHostedQueryRevisions,
    ManagementPreviewHostedQueryRevisions2,
    ManagementPreviewHostedQueryRevisions3,
    ManagementPreviewHostedDeleteManagementPreviews,
    ManagementPreviewHostedQueryManagementPreviews,
    ManagementPreviewHostedQueryValue,
    ManagementPreviewHostedInsertManagementPreviews,
    ManagementCommitHostedManagementQueryManagementPreviews,
    ManagementCommitHostedManagementQueryRepositories,
    ManagementCommitHostedManagementInsertSnapshotRemovals,
    ManagementCommitHostedManagementQueryArtifactAccessRevocations,
    ManagementCommitHostedManagementQueryRevisions,
    ManagementCommitHostedManagementInsertRevisions,
    ManagementCommitHostedManagementInsertSnapshotChanges,
    ManagementCommitHostedManagementInsertAudit,
    ManagementCommitHostedManagementInsertSecurityAudit,
    ManagementCommitHostedManagementUpdateManagementPreviews,
    ManagementCommitHostedManagementUpdateRepositories,
    ManagementManagementPermissionQueryAdminUsers,
    ProxyProxyCatalogQueryProxyRepositories,
    ProxyProxyCatalogQueryProxyCoordinates,
    ProxyCreateProxyQueryProxyRepositories,
    ProxyCreateProxyInsertProxyRepositories,
    ProxyProxiesQueryProxyRepositories,
    ProxySaveProxyIndexQueryProxyRepositories,
    ProxySaveProxyIndexQueryProxyCoordinates,
    ProxySaveProxyIndexInsertProxyCoordinates,
    ProxySaveProxyIndexInsertProxyIndexes,
    ProxyProxyDigestQueryProxyCoordinates,
    ProxyProxyHasObjectQueryProxyObjects,
    ProxyInstallProxyObjectQueryObjects,
    ProxyInstallProxyObjectInsertObjects,
    ProxyInstallProxyObjectInsertProxyObjects,
    ProxyResolutionMaterializationValidQueryProxyMaterializations,
    ProxyResolutionBeginProxyViewDeleteProxySessions,
    ProxyResolutionBeginProxyViewQueryProxySessions,
    ProxyResolutionBeginProxyViewQueryProxyRepositories,
    ProxyResolutionBeginProxyViewQueryValue,
    ProxyResolutionBeginProxyViewInsertProxySessions,
    ProxyResolutionProxySessionEntryQueryProxySessionEntries,
    ProxyResolutionFreezeProxyEntryQueryProxySessions,
    ProxyResolutionFreezeProxyEntryQueryProxySessionEntries,
    ProxyResolutionFreezeProxyEntryQueryProxySessionEntries2,
    ProxyResolutionFreezeProxyEntryInsertProxySessionEntries,
    ProxyResolutionFreezeProxyEntryQueryProxySessionEntries3,
    ProxyResolutionFreezeProxyEntryInsertProxySessionObjects,
    ProxyResolutionFinishProxySessionDeleteProxySessions,
    ProxyResolutionSaveUpstreamValidationInsertProxyValidators,
    ProxyResolutionProxyMissingQueryProxyMissing,
    ProxyResolutionSaveProxyMissingDeleteProxyMissing,
    ProxyResolutionSaveProxyMissingQueryProxyMissing,
    ProxyResolutionSaveProxyMissingInsertProxyMissing,
    ProxyResolutionCommitMaterializationQueryProxySessions,
    ProxyResolutionCommitMaterializationQueryProxySessionEntries,
    ProxyResolutionCommitMaterializationQueryProxyObjects,
    ProxyResolutionCommitMaterializationQueryProxyMaterializations,
    ProxyResolutionCommitMaterializationInsertProxyMaterializations,
    ProxyResolutionCommitMaterializationInsertMaterializationItems,
    RepositoryOpenInsertMetadata,
    RepositoryOpenQueryMetadata,
    RepositoryCreateRepositoryQueryRepositories,
    RepositoryCreateRepositoryInsertRepositories,
    RepositoryCreateRepositoryInsertAudit,
    RepositoryInstallValidatedQueryObjects,
    RepositoryInstallValidatedInsertObjects,
    RepositoryCommitInnerQueryRepositories,
    RepositoryCommitInnerQueryAccessTokens,
    RepositoryCommitInnerQueryObjects,
    RepositoryCommitInnerQueryCommits,
    RepositoryCommitInnerQueryCrateNames,
    RepositoryCommitInnerQueryRevisions,
    RepositoryCommitInnerQueryRevisions2,
    RepositoryCommitInnerInsertCrateNames,
    RepositoryCommitInnerInsertObjects,
    RepositoryCommitInnerInsertRevisions,
    RepositoryCommitInnerInsertSnapshotChanges,
    RepositoryCommitInnerUpdateRepositories,
    RepositoryCommitInnerInsertCommits,
    RepositoryCommitInnerInsertAudit,
    RepositoryCommitInnerUpdatePublishBatches,
    RepositoryHeadQueryRepositories,
    RepositoryRepositoriesQueryRepositories,
    RepositorySnapshotQueryRepositories,
    RepositoryIndexEntriesQueryCrateNames,
    RepositoryIndexEntriesQueryRevisions,
    RepositoryContainsObjectQueryArtifactAccessRevocations,
    RepositoryOpenSnapshotObjectQuerySnapshots,
    RepositoryOpenSnapshotObjectQueryArtifactAccessRevocations,
    RepositoryOpenReaderQueryObjects,
    RepositoryCheckIntegrityQueryRepositories,
    RepositoryCheckIntegrityQueryRevisions,
    RepositoryCheckIntegrityQueryObjects,
    RepositoryCheckIntegrityQueryDeletionItems,
    RepositoryCheckIntegrityQueryRevisions2,
    RepositoryInsertSnapshotInsertSnapshots,
    RepositoryReadSnapshotQuerySnapshots,
    RepositoryReadEntryQuerySnapshotRemovals,
    RepositoryReadEntryQuerySnapshotChanges,
    RepositoryReadEntryQuerySnapshotCheckpoints,
    RepositoryReadEntryQueryCheckpointEntries,
    RepositoryMaterializeCheckpointQuerySnapshotRemovals,
    RepositoryMaterializeCheckpointQuerySnapshotChanges,
    RepositoryMaterializeCheckpointQuerySnapshotCheckpoints,
    RepositoryMaterializeCheckpointQueryCheckpointEntries,
    RepositoryMaterializeCheckpointInsertCheckpointEntries,
    RepositoryMaterializeCheckpointInsertSnapshotCheckpoints,
    ResolutionResolveAtQuerySnapshots,
    ResolutionResolveAtQueryRepositories,
    ResolutionResolveAtQueryArtifactAccessRevocations,
    ResolutionResolveAtDeleteRetentionLeases,
    ResolutionResolveAtQueryRetentionLeases,
    ResolutionResolveAtInsertRetentionLeases,
    ResolutionRenewLeaseProofUpdateRetentionLeases,
    ResolutionReleaseLeaseDeleteRetentionLeases,
    ResolutionRevisionAtQuerySnapshotRemovals,
    ResolutionRevisionAtQuerySnapshotChanges,
    ResolutionRevisionAtQuerySnapshotCheckpoints,
    ResolutionRevisionAtQueryCheckpointEntries,
    ResolutionRevisionAtQueryRevisions,
    RetentionRetentionPinsQueryRetentionPins,
    RetentionPinSnapshotQueryRepositories,
    RetentionPinSnapshotQueryRetentionPins,
    RetentionPinSnapshotQueryRetentionPins2,
    RetentionPinSnapshotQueryRevisions,
    RetentionPinSnapshotInsertRetentionPins,
    RetentionPinSnapshotInsertSecurityAudit,
    RetentionReleasePinQueryRetentionPins,
    RetentionReleasePinDeleteRetentionPins,
    RetentionReleasePinInsertSecurityAudit,
    RetentionSnapshotRevisionsQuerySnapshotRemovals,
    RetentionSnapshotRevisionsQuerySnapshotCheckpoints,
    RetentionSnapshotRevisionsQuerySnapshotChanges,
    SchemaRevisionQuerySqliteSchema,
    SchemaRevisionQueryMetadata,
    SchemaRepositoryCreateMetadata,
    SchemaRepositoryCreateRepositories,
    SchemaRepositoryCreateSnapshots,
    SchemaRepositoryCreateObjects,
    SchemaRepositoryCreateCrateNames,
    SchemaRepositoryCreateRevisions,
    SchemaRepositoryCreateSnapshotChanges,
    SchemaRepositoryCreateCommits,
    SchemaRepositoryCreateAudit,
    SchemaRepositoryCreateValue,
    SchemaRepositoryInsertMetadata,
    SchemaSnapshotCheckpointsCreateSnapshotCheckpoints,
    SchemaSnapshotCheckpointsCreateCheckpointEntries,
    SchemaSnapshotCheckpointsInsertMetadata,
    SchemaPublishBatchesCreatePublishBatches,
    SchemaPublishBatchesCreateBatchItems,
    SchemaPublishBatchesCreateValue,
    SchemaPublishBatchesInsertMetadata,
    SchemaRetentionLeasesCreateRetentionLeases,
    SchemaRetentionLeasesCreateValue,
    SchemaRetentionLeasesInsertMetadata,
    SchemaProxyCacheCreateProxyRepositories,
    SchemaProxyCacheCreateProxyIndexes,
    SchemaProxyCacheCreateProxyCoordinates,
    SchemaProxyCacheCreateProxyObjects,
    SchemaProxyCacheInsertMetadata,
    SchemaProxyResolutionCreateProxySessions,
    SchemaProxyResolutionCreateProxySessionEntries,
    SchemaProxyResolutionCreateProxyValidators,
    SchemaProxyResolutionCreateProxyMissing,
    SchemaProxyResolutionCreateProxyMaterializations,
    SchemaProxyResolutionCreateValue,
    SchemaProxyResolutionCreateMaterializationItems,
    SchemaProxyResolutionInsertMetadata,
    SchemaAccessControlCreateAdminUsers,
    SchemaAccessControlCreateAdminSessions,
    SchemaAccessControlCreateAccessTokens,
    SchemaAccessControlCreateSecurityAudit,
    SchemaAccessControlInsertMetadata,
    SchemaHostedManagementCreateSnapshotRemovals,
    SchemaHostedManagementCreateManagementPreviews,
    SchemaHostedManagementInsertMetadata,
    SchemaObjectLifecycleAlterObjects,
    SchemaObjectLifecycleAlterObjects2,
    SchemaObjectLifecycleCreateArtifactAccessRevocations,
    SchemaObjectLifecycleCreateRetentionPins,
    SchemaObjectLifecycleCreateDeletionPreviews,
    SchemaObjectLifecycleCreateObjectDeletions,
    SchemaObjectLifecycleCreateDeletionItems,
    SchemaObjectLifecycleCreateProxySessionObjects,
    SchemaObjectLifecycleInsertMetadata,
    SchemaProxyViewsAlterProxySessions,
    SchemaProxyViewsInsertMetadata,
    SchemaDeletionRetriesAlterObjectDeletions,
    SchemaDeletionRetriesAlterObjectDeletions2,
    SchemaDeletionRetriesAlterObjectDeletions3,
    SchemaDeletionRetriesCreateValue,
    SchemaDeletionRetriesInsertMetadata,
    SchemaTokenPublishScopeAlterAccessTokens,
    SchemaTokenPublishScopeAlterAccessTokens2,
    SchemaTokenPublishScopeInsertMetadata,
    SchemaUnpublishedObjectsAlterObjects,
    SchemaUnpublishedObjectsUpdateObjects,
    SchemaUnpublishedObjectsCreateValue,
    SchemaUnpublishedObjectsCreateValue2,
    SchemaUnpublishedObjectsCreateValue3,
    SchemaUnpublishedObjectsCreateValue4,
    SchemaUnpublishedObjectsCreateValue5,
    SchemaUnpublishedObjectsCreateValue6,
    SchemaUnpublishedObjectsInsertMetadata,
    SecurityTokensQueryAccessTokens,
    SecuritySecurityAuditQuerySecurityAudit,
    SecuritySecurityAuditPageQuerySecurityAudit,
    SecuritySetUserGrantsQueryAdminUsers,
    SecuritySetUserGrantsQueryAdminUsers2,
    SecuritySetUserGrantsQueryAdminUsers3,
    SecuritySetUserGrantsUpdateAdminUsers,
    SecuritySetUserGrantsInsertSecurityAudit,
    SecuritySetUserEnabledQueryAdminUsers,
    SecuritySetUserEnabledQueryAdminUsers2,
    SecuritySetUserEnabledQueryAdminUsers3,
    SecuritySetUserEnabledUpdateAdminUsers,
    SecuritySetUserEnabledInsertSecurityAudit,
    SecurityBootstrapStatusQueryAdminUsers,
    SecurityBootstrapAdminQueryAdminUsers,
    SecurityBootstrapAdminInsertAdminUsers,
    SecurityBootstrapAdminInsertSecurityAudit,
    SecurityFinishLoginDeleteAdminSessions,
    SecurityFinishLoginQueryAdminUsers,
    SecurityFinishLoginQueryAdminSessions,
    SecurityFinishLoginUpdateAdminUsers,
    SecurityFinishLoginInsertAdminSessions,
    SecurityFinishLoginUpdateAdminUsers2,
    SecurityFinishLoginInsertSecurityAudit,
    SecurityBrowserIdentityQueryAdminSessions,
    SecurityLogoutDeleteAdminSessions,
    SecurityUsersQueryAdminUsers,
    SecurityCreateUserQueryAdminUsers,
    SecurityCreateUserQueryAdminUsers2,
    SecurityCreateUserQueryAdminUsers3,
    SecurityCreateUserInsertAdminUsers,
    SecurityCreateUserInsertSecurityAudit,
    SecurityChangePasswordUpdateAdminUsers,
    SecurityChangePasswordDeleteAdminSessions,
    SecurityChangePasswordInsertSecurityAudit,
    SecurityCreateScopedTokenQueryAccessTokens,
    SecurityCreateScopedTokenInsertAccessTokens,
    SecurityCreateScopedTokenInsertSecurityAudit,
    SecurityRevokeTokenQueryAccessTokens,
    SecurityRevokeTokenUpdateAccessTokens,
    SecurityRevokeTokenInsertSecurityAudit,
    SecurityBrowserActorQueryAdminUsers,
    SecurityTokenIdentityOnQueryAccessTokens,
    UnpublishedCollectUnpublishedQueryObjectDeletions,
    UnpublishedCollectUnpublishedQueryProxySessionEntries,
    UnpublishedCollectUnpublishedQueryObjects,
    UnpublishedCollectUnpublishedInsertObjectDeletions,
    UnpublishedCollectUnpublishedUpdateObjects,
    UnpublishedCollectUnpublishedInsertDeletionItems,
    UnpublishedCollectUnpublishedInsertSecurityAudit,
    UnpublishedRecoverUnpublishedQueryObjects,
    UnpublishedRecoverUnpublishedInsertObjects,
    UnpublishedRecoverUnpublishedUpdateObjects,
    ManagementRevisionHistoryPageQueryRevisions,
    ProxyProxyIndexQueryProxyIndexes,
    ProxyResolutionProxySessionQueryProxySessions,
    ProxyResolutionUpstreamValidationQueryProxyValidators,
    RepositoryContainsObjectQueryRevisions,
    SecurityLoginUserQueryAdminUsers,
}

pub(crate) enum Output {
    Rows(Vec<Row>),
    Affected(u64),
}

pub(crate) struct Mappers {
    backup: BackupMapperClient,
    batches: BatchesMapperClient,
    deletion: DeletionMapperClient,
    maintenance: MaintenanceMapperClient,
    management: ManagementMapperClient,
    proxy: ProxyMapperClient,
    proxy_resolution: ProxyResolutionMapperClient,
    repository: RepositoryMapperClient,
    resolution: ResolutionMapperClient,
    retention: RetentionMapperClient,
    schema: SchemaMapperClient,
    security: SecurityMapperClient,
    unpublished: UnpublishedMapperClient,
}

impl Mappers {
    /// 业务作用：让所有仓库 Mapper 固定到同一个文件来源，避免同名来源重启后的隐式重连。
    /// 参数说明：
    /// - `source`：由宿主持有关闭责任的 SQLite 来源。
    ///
    /// 返回：每个业务接口绑定同一 owner，名称不匹配时拒绝。
    pub(crate) fn new(source: &nasa::tx::sqlite::SqliteDataSource) -> Result<Self> {
        Ok(Self {
            backup: BackupMapperClient::new().with_datasource(source.clone())?,
            batches: BatchesMapperClient::new().with_datasource(source.clone())?,
            deletion: DeletionMapperClient::new().with_datasource(source.clone())?,
            maintenance: MaintenanceMapperClient::new().with_datasource(source.clone())?,
            management: ManagementMapperClient::new().with_datasource(source.clone())?,
            proxy: ProxyMapperClient::new().with_datasource(source.clone())?,
            proxy_resolution: ProxyResolutionMapperClient::new().with_datasource(source.clone())?,
            repository: RepositoryMapperClient::new().with_datasource(source.clone())?,
            resolution: ResolutionMapperClient::new().with_datasource(source.clone())?,
            retention: RetentionMapperClient::new().with_datasource(source.clone())?,
            schema: SchemaMapperClient::new().with_datasource(source.clone())?,
            security: SecurityMapperClient::new().with_datasource(source.clone())?,
            unpublished: UnpublishedMapperClient::new().with_datasource(source.clone())?,
        })
    }
}

impl Statement {
    /// 业务作用：把同步存储命令路由到固定 Mapper 接口，执行域不接受任意 SQL 文本。
    /// 参数说明：
    /// - `mappers`：固定同一来源的接口集合。
    /// - `parameters`：调用点提供的拥有型业务值。
    ///
    /// 返回：完成的读取或写入结果；参数数量不一致时在 SQL 执行前拒绝。
    pub(crate) async fn run(self, mappers: &Mappers, parameters: Vec<Value>) -> Result<Output> {
        match self {
            Self::BackupBackupQueryObjects => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(mappers.backup.backup_query_objects().await?))
            }
            Self::BatchesBeginBatchQueryRepositories => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .batches
                        .begin_batch_query_repositories(&parameters[0])
                        .await?,
                ))
            }
            Self::BatchesBeginBatchQueryPublishBatches => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .batches
                        .begin_batch_query_publish_batches(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::BatchesBeginBatchUpdatePublishBatches => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .batches
                        .begin_batch_update_publish_batches(&parameters[0])
                        .await?,
                ))
            }
            Self::BatchesBeginBatchQueryPublishBatches2 => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .batches
                        .begin_batch_query_publish_batches_2(&parameters[0])
                        .await?,
                ))
            }
            Self::BatchesBeginBatchInsertPublishBatches => {
                ensure!(parameters.len() == 6, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .batches
                        .begin_batch_insert_publish_batches(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                            &parameters[4],
                            &parameters[5],
                        )
                        .await?,
                ))
            }
            Self::BatchesBatchQueryRepositories => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .batches
                        .batch_query_repositories(&parameters[0])
                        .await?,
                ))
            }
            Self::BatchesAddBatchItemQueryBatchItems => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .batches
                        .add_batch_item_query_batch_items(&parameters[0])
                        .await?,
                ))
            }
            Self::BatchesAddBatchItemInsertBatchItems => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .batches
                        .add_batch_item_insert_batch_items(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::BatchesCommitBatchAuthorizedUpdatePublishBatches => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .batches
                        .commit_batch_authorized_update_publish_batches(
                            &parameters[0],
                            &parameters[1],
                        )
                        .await?,
                ))
            }
            Self::BatchesCommitBatchAuthorizedUpdatePublishBatches2 => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .batches
                        .commit_batch_authorized_update_publish_batches_2(&parameters[0])
                        .await?,
                ))
            }
            Self::BatchesAbortBatchUpdatePublishBatches => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .batches
                        .abort_batch_update_publish_batches(&parameters[0])
                        .await?,
                ))
            }
            Self::BatchesRecoverBatchesUpdatePublishBatches => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .batches
                        .recover_batches_update_publish_batches()
                        .await?,
                ))
            }
            Self::BatchesRecoverBatchesUpdatePublishBatches2 => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .batches
                        .recover_batches_update_publish_batches_2(&parameters[0])
                        .await?,
                ))
            }
            Self::BatchesReadBatchQueryPublishBatches => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .batches
                        .read_batch_query_publish_batches(&parameters[0])
                        .await?,
                ))
            }
            Self::BatchesReadBatchQueryBatchItems => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .batches
                        .read_batch_query_batch_items(&parameters[0])
                        .await?,
                ))
            }
            Self::DeletionDeletionTasksQueryObjectDeletions => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .deletion_tasks_query_object_deletions()
                        .await?,
                ))
            }
            Self::DeletionPendingDeletionQueryObjectDeletions => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .pending_deletion_query_object_deletions()
                        .await?,
                ))
            }
            Self::DeletionDeferDeletionUpdateObjectDeletions => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .defer_deletion_update_object_deletions(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::DeletionRetryDeletionQueryAdminUsers => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .retry_deletion_query_admin_users(&parameters[0])
                        .await?,
                ))
            }
            Self::DeletionRetryDeletionUpdateObjectDeletions => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .retry_deletion_update_object_deletions(&parameters[0])
                        .await?,
                ))
            }
            Self::DeletionRetryDeletionInsertSecurityAudit => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .retry_deletion_insert_security_audit(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::DeletionPreviewDeletionDeleteDeletionPreviews => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .preview_deletion_delete_deletion_previews()
                        .await?,
                ))
            }
            Self::DeletionPreviewDeletionQueryDeletionPreviews => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .preview_deletion_query_deletion_previews(&parameters[0])
                        .await?,
                ))
            }
            Self::DeletionPreviewDeletionInsertDeletionPreviews => {
                ensure!(parameters.len() == 5, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .preview_deletion_insert_deletion_previews(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                            &parameters[4],
                        )
                        .await?,
                ))
            }
            Self::DeletionAcceptDeletionQueryDeletionPreviews => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .accept_deletion_query_deletion_previews(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::DeletionAcceptDeletionQueryObjectDeletions => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .accept_deletion_query_object_deletions()
                        .await?,
                ))
            }
            Self::DeletionAcceptDeletionInsertObjectDeletions => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .accept_deletion_insert_object_deletions(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::DeletionAcceptDeletionInsertArtifactAccessRevocations => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .accept_deletion_insert_artifact_access_revocations(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::DeletionAcceptDeletionUpdateProxyMaterializations => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .accept_deletion_update_proxy_materializations(
                            &parameters[0],
                            &parameters[1],
                        )
                        .await?,
                ))
            }
            Self::DeletionAcceptDeletionDeleteProxyObjects => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .accept_deletion_delete_proxy_objects(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::DeletionAcceptDeletionInsertDeletionItems => {
                ensure!(parameters.len() == 5, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .accept_deletion_insert_deletion_items(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                            &parameters[4],
                        )
                        .await?,
                ))
            }
            Self::DeletionAcceptDeletionUpdateObjects => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .accept_deletion_update_objects(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::DeletionAcceptDeletionDeleteProxyIndexes => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .accept_deletion_delete_proxy_indexes(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::DeletionAcceptDeletionDeleteProxyValidators => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .accept_deletion_delete_proxy_validators(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::DeletionAcceptDeletionDeleteProxyMissing => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .accept_deletion_delete_proxy_missing(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::DeletionAcceptDeletionUpdateDeletionPreviews => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .accept_deletion_update_deletion_previews(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::DeletionAcceptDeletionInsertSecurityAudit => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .accept_deletion_insert_security_audit(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::DeletionRunDeletionQueryDeletionItems => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .run_deletion_query_deletion_items(&parameters[0])
                        .await?,
                ))
            }
            Self::DeletionRunDeletionUpdateObjectDeletions => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .run_deletion_update_object_deletions(&parameters[0])
                        .await?,
                ))
            }
            Self::DeletionRunDeletionQueryDeletionItems2 => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .run_deletion_query_deletion_items_2(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::DeletionRunDeletionQueryObjects => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .run_deletion_query_objects(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::DeletionRunDeletionUpdateObjects => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .run_deletion_update_objects(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::DeletionRunDeletionUpdateDeletionItems => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .run_deletion_update_deletion_items(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::DeletionRunDeletionUpdateObjectDeletions2 => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .run_deletion_update_object_deletions_2(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::DeletionRunDeletionQueryDeletionItems3 => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .run_deletion_query_deletion_items_3(&parameters[0])
                        .await?,
                ))
            }
            Self::DeletionRunDeletionUpdateObjectDeletions3 => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .deletion
                        .run_deletion_update_object_deletions_3(&parameters[0])
                        .await?,
                ))
            }
            Self::DeletionRecoverDeletionsQueryObjectDeletions => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .recover_deletions_query_object_deletions()
                        .await?,
                ))
            }
            Self::DeletionDeletionViewQueryRevisions => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .deletion_view_query_revisions(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::DeletionDeletionViewQueryProxyCoordinates => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .deletion_view_query_proxy_coordinates(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::DeletionDeletionViewQueryBatchItems => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .deletion_view_query_batch_items(&parameters[0])
                        .await?,
                ))
            }
            Self::DeletionDeletionViewQueryProxySessionObjects => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .deletion_view_query_proxy_session_objects(&parameters[0])
                        .await?,
                ))
            }
            Self::DeletionDeletionViewQueryRevisions2 => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .deletion_view_query_revisions_2(&parameters[0])
                        .await?,
                ))
            }
            Self::DeletionDeletionViewQueryProxyObjects => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .deletion_view_query_proxy_objects(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::DeletionDeletionViewQueryValue => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers.deletion.deletion_view_query_value().await?,
                ))
            }
            Self::DeletionDeletionPermissionQueryAdminUsers => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .deletion_permission_query_admin_users(&parameters[0])
                        .await?,
                ))
            }
            Self::DeletionDeletionPermissionQueryRepositories => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .deletion_permission_query_repositories(&parameters[0])
                        .await?,
                ))
            }
            Self::DeletionDeletionPermissionQueryProxyRepositories => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .deletion_permission_query_proxy_repositories(&parameters[0])
                        .await?,
                ))
            }
            Self::DeletionDeletionPermissionQueryRevisions => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .deletion_permission_query_revisions(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::DeletionProtectRevisionQueryRevisions => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .protect_revision_query_revisions(&parameters[0])
                        .await?,
                ))
            }
            Self::DeletionProtectRevisionQueryRepositories => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .protect_revision_query_repositories(&parameters[0])
                        .await?,
                ))
            }
            Self::DeletionReadTaskQueryObjectDeletions => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .read_task_query_object_deletions(&parameters[0])
                        .await?,
                ))
            }
            Self::DeletionReadTaskQueryObjectDeletions2 => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .read_task_query_object_deletions_2(&parameters[0])
                        .await?,
                ))
            }
            Self::DeletionReadTaskQueryObjectDeletions3 => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .read_task_query_object_deletions_3(&parameters[0])
                        .await?,
                ))
            }
            Self::DeletionReadTaskQueryObjectDeletions4 => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .deletion
                        .read_task_query_object_deletions_4(&parameters[0])
                        .await?,
                ))
            }
            Self::MaintenanceMaintainQueryMetadata => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers.maintenance.maintain_query_metadata().await?,
                ))
            }
            Self::MaintenanceMaintainUpdatePublishBatches => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .maintenance
                        .maintain_update_publish_batches()
                        .await?,
                ))
            }
            Self::MaintenanceMaintainDeleteProxySessions => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.maintenance.maintain_delete_proxy_sessions().await?,
                ))
            }
            Self::MaintenanceMaintainDeleteRetentionLeases => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .maintenance
                        .maintain_delete_retention_leases()
                        .await?,
                ))
            }
            Self::MaintenanceMaintainDeleteAdminSessions => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.maintenance.maintain_delete_admin_sessions().await?,
                ))
            }
            Self::MaintenanceMaintainDeleteProxyMissing => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.maintenance.maintain_delete_proxy_missing().await?,
                ))
            }
            Self::MaintenanceMaintainDeleteManagementPreviews => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .maintenance
                        .maintain_delete_management_previews()
                        .await?,
                ))
            }
            Self::MaintenanceMaintainDeleteDeletionPreviews => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .maintenance
                        .maintain_delete_deletion_previews()
                        .await?,
                ))
            }
            Self::MaintenanceMaintainQueryObjects => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .maintenance
                        .maintain_query_objects(&parameters[0])
                        .await?,
                ))
            }
            Self::ManagementCatalogQueryRepositories => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .management
                        .catalog_query_repositories(&parameters[0])
                        .await?,
                ))
            }
            Self::ManagementCatalogQueryRevisions => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .management
                        .catalog_query_revisions(&parameters[0], &parameters[1], &parameters[2])
                        .await?,
                ))
            }
            Self::ManagementPreviewHostedQueryRepositories => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .management
                        .preview_hosted_query_repositories(&parameters[0])
                        .await?,
                ))
            }
            Self::ManagementPreviewHostedQueryRevisions => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .management
                        .preview_hosted_query_revisions(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::ManagementPreviewHostedQueryRevisions2 => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .management
                        .preview_hosted_query_revisions_2(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::ManagementPreviewHostedQueryRevisions3 => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .management
                        .preview_hosted_query_revisions_3(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::ManagementPreviewHostedDeleteManagementPreviews => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .management
                        .preview_hosted_delete_management_previews()
                        .await?,
                ))
            }
            Self::ManagementPreviewHostedQueryManagementPreviews => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .management
                        .preview_hosted_query_management_previews(&parameters[0])
                        .await?,
                ))
            }
            Self::ManagementPreviewHostedQueryValue => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers.management.preview_hosted_query_value().await?,
                ))
            }
            Self::ManagementPreviewHostedInsertManagementPreviews => {
                ensure!(parameters.len() == 8, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .management
                        .preview_hosted_insert_management_previews(ManagementPreviewWrite {
                            id: &parameters[0],
                            actor: &parameters[1],
                            repository_id: &parameters[2],
                            expected_head: &parameters[3],
                            operation: &parameters[4],
                            selection: &parameters[5],
                            fingerprint: &parameters[6],
                            expires_at: &parameters[7],
                        })
                        .await?,
                ))
            }
            Self::ManagementCommitHostedManagementQueryManagementPreviews => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .management
                        .commit_hosted_management_query_management_previews(
                            &parameters[0],
                            &parameters[1],
                        )
                        .await?,
                ))
            }
            Self::ManagementCommitHostedManagementQueryRepositories => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .management
                        .commit_hosted_management_query_repositories(&parameters[0])
                        .await?,
                ))
            }
            Self::ManagementCommitHostedManagementInsertSnapshotRemovals => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .management
                        .commit_hosted_management_insert_snapshot_removals(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::ManagementCommitHostedManagementQueryArtifactAccessRevocations => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .management
                        .commit_hosted_management_query_artifact_access_revocations(&parameters[0])
                        .await?,
                ))
            }
            Self::ManagementCommitHostedManagementQueryRevisions => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .management
                        .commit_hosted_management_query_revisions(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::ManagementCommitHostedManagementInsertRevisions => {
                ensure!(parameters.len() == 8, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .management
                        .commit_hosted_management_insert_revisions(RevisionWrite {
                            id: &parameters[0],
                            repository_id: &parameters[1],
                            name: &parameters[2],
                            version: &parameters[3],
                            sha256: &parameters[4],
                            metadata_sha256: &parameters[5],
                            entry_json: &parameters[6],
                            actor: &parameters[7],
                        })
                        .await?,
                ))
            }
            Self::ManagementCommitHostedManagementInsertSnapshotChanges => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .management
                        .commit_hosted_management_insert_snapshot_changes(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::ManagementCommitHostedManagementInsertAudit => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .management
                        .commit_hosted_management_insert_audit(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::ManagementCommitHostedManagementInsertSecurityAudit => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .management
                        .commit_hosted_management_insert_security_audit(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::ManagementCommitHostedManagementUpdateManagementPreviews => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .management
                        .commit_hosted_management_update_management_previews(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::ManagementCommitHostedManagementUpdateRepositories => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .management
                        .commit_hosted_management_update_repositories(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::ManagementManagementPermissionQueryAdminUsers => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .management
                        .management_permission_query_admin_users(&parameters[0])
                        .await?,
                ))
            }
            Self::ProxyProxyCatalogQueryProxyRepositories => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy
                        .proxy_catalog_query_proxy_repositories(&parameters[0])
                        .await?,
                ))
            }
            Self::ProxyProxyCatalogQueryProxyCoordinates => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy
                        .proxy_catalog_query_proxy_coordinates(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::ProxyCreateProxyQueryProxyRepositories => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy
                        .create_proxy_query_proxy_repositories(&parameters[0])
                        .await?,
                ))
            }
            Self::ProxyCreateProxyInsertProxyRepositories => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .proxy
                        .create_proxy_insert_proxy_repositories(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::ProxyProxiesQueryProxyRepositories => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers.proxy.proxies_query_proxy_repositories().await?,
                ))
            }
            Self::ProxySaveProxyIndexQueryProxyRepositories => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy
                        .save_proxy_index_query_proxy_repositories(&parameters[0])
                        .await?,
                ))
            }
            Self::ProxySaveProxyIndexQueryProxyCoordinates => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy
                        .save_proxy_index_query_proxy_coordinates(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::ProxySaveProxyIndexInsertProxyCoordinates => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .proxy
                        .save_proxy_index_insert_proxy_coordinates(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::ProxySaveProxyIndexInsertProxyIndexes => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .proxy
                        .save_proxy_index_insert_proxy_indexes(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::ProxyProxyDigestQueryProxyCoordinates => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy
                        .proxy_digest_query_proxy_coordinates(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::ProxyProxyHasObjectQueryProxyObjects => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy
                        .proxy_has_object_query_proxy_objects(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::ProxyInstallProxyObjectQueryObjects => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy
                        .install_proxy_object_query_objects(&parameters[0])
                        .await?,
                ))
            }
            Self::ProxyInstallProxyObjectInsertObjects => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .proxy
                        .install_proxy_object_insert_objects(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::ProxyInstallProxyObjectInsertProxyObjects => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .proxy
                        .install_proxy_object_insert_proxy_objects(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::ProxyResolutionMaterializationValidQueryProxyMaterializations => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy_resolution
                        .materialization_valid_query_proxy_materializations(&parameters[0])
                        .await?,
                ))
            }
            Self::ProxyResolutionBeginProxyViewDeleteProxySessions => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .proxy_resolution
                        .begin_proxy_view_delete_proxy_sessions()
                        .await?,
                ))
            }
            Self::ProxyResolutionBeginProxyViewQueryProxySessions => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy_resolution
                        .begin_proxy_view_query_proxy_sessions(&parameters[0])
                        .await?,
                ))
            }
            Self::ProxyResolutionBeginProxyViewQueryProxyRepositories => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy_resolution
                        .begin_proxy_view_query_proxy_repositories(&parameters[0])
                        .await?,
                ))
            }
            Self::ProxyResolutionBeginProxyViewQueryValue => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy_resolution
                        .begin_proxy_view_query_value()
                        .await?,
                ))
            }
            Self::ProxyResolutionBeginProxyViewInsertProxySessions => {
                ensure!(parameters.len() == 6, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .proxy_resolution
                        .begin_proxy_view_insert_proxy_sessions(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                            &parameters[4],
                            &parameters[5],
                        )
                        .await?,
                ))
            }
            Self::ProxyResolutionProxySessionEntryQueryProxySessionEntries => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy_resolution
                        .proxy_session_entry_query_proxy_session_entries(
                            &parameters[0],
                            &parameters[1],
                        )
                        .await?,
                ))
            }
            Self::ProxyResolutionFreezeProxyEntryQueryProxySessions => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy_resolution
                        .freeze_proxy_entry_query_proxy_sessions(&parameters[0])
                        .await?,
                ))
            }
            Self::ProxyResolutionFreezeProxyEntryQueryProxySessionEntries => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy_resolution
                        .freeze_proxy_entry_query_proxy_session_entries(
                            &parameters[0],
                            &parameters[1],
                        )
                        .await?,
                ))
            }
            Self::ProxyResolutionFreezeProxyEntryQueryProxySessionEntries2 => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy_resolution
                        .freeze_proxy_entry_query_proxy_session_entries_2(&parameters[0])
                        .await?,
                ))
            }
            Self::ProxyResolutionFreezeProxyEntryInsertProxySessionEntries => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .proxy_resolution
                        .freeze_proxy_entry_insert_proxy_session_entries(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::ProxyResolutionFreezeProxyEntryQueryProxySessionEntries3 => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy_resolution
                        .freeze_proxy_entry_query_proxy_session_entries_3(
                            &parameters[0],
                            &parameters[1],
                        )
                        .await?,
                ))
            }
            Self::ProxyResolutionFreezeProxyEntryInsertProxySessionObjects => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .proxy_resolution
                        .freeze_proxy_entry_insert_proxy_session_objects(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::ProxyResolutionFinishProxySessionDeleteProxySessions => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .proxy_resolution
                        .finish_proxy_session_delete_proxy_sessions(&parameters[0])
                        .await?,
                ))
            }
            Self::ProxyResolutionSaveUpstreamValidationInsertProxyValidators => {
                ensure!(parameters.len() == 6, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .proxy_resolution
                        .save_upstream_validation_insert_proxy_validators(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                            &parameters[4],
                            &parameters[5],
                        )
                        .await?,
                ))
            }
            Self::ProxyResolutionProxyMissingQueryProxyMissing => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy_resolution
                        .proxy_missing_query_proxy_missing(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::ProxyResolutionSaveProxyMissingDeleteProxyMissing => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .proxy_resolution
                        .save_proxy_missing_delete_proxy_missing()
                        .await?,
                ))
            }
            Self::ProxyResolutionSaveProxyMissingQueryProxyMissing => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy_resolution
                        .save_proxy_missing_query_proxy_missing()
                        .await?,
                ))
            }
            Self::ProxyResolutionSaveProxyMissingInsertProxyMissing => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .proxy_resolution
                        .save_proxy_missing_insert_proxy_missing(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::ProxyResolutionCommitMaterializationQueryProxySessions => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy_resolution
                        .commit_materialization_query_proxy_sessions(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::ProxyResolutionCommitMaterializationQueryProxySessionEntries => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy_resolution
                        .commit_materialization_query_proxy_session_entries(
                            &parameters[0],
                            &parameters[1],
                        )
                        .await?,
                ))
            }
            Self::ProxyResolutionCommitMaterializationQueryProxyObjects => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy_resolution
                        .commit_materialization_query_proxy_objects(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::ProxyResolutionCommitMaterializationQueryProxyMaterializations => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy_resolution
                        .commit_materialization_query_proxy_materializations(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::ProxyResolutionCommitMaterializationInsertProxyMaterializations => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .proxy_resolution
                        .commit_materialization_insert_proxy_materializations(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::ProxyResolutionCommitMaterializationInsertMaterializationItems => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .proxy_resolution
                        .commit_materialization_insert_materialization_items(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::RepositoryOpenInsertMetadata => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .repository
                        .open_insert_metadata(&parameters[0])
                        .await?,
                ))
            }
            Self::RepositoryOpenQueryMetadata => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers.repository.open_query_metadata().await?,
                ))
            }
            Self::RepositoryCreateRepositoryQueryRepositories => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .create_repository_query_repositories(&parameters[0])
                        .await?,
                ))
            }
            Self::RepositoryCreateRepositoryInsertRepositories => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .repository
                        .create_repository_insert_repositories(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::RepositoryCreateRepositoryInsertAudit => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .repository
                        .create_repository_insert_audit(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::RepositoryInstallValidatedQueryObjects => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .install_validated_query_objects(&parameters[0])
                        .await?,
                ))
            }
            Self::RepositoryInstallValidatedInsertObjects => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .repository
                        .install_validated_insert_objects(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::RepositoryCommitInnerQueryRepositories => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .commit_inner_query_repositories(&parameters[0])
                        .await?,
                ))
            }
            Self::RepositoryCommitInnerQueryAccessTokens => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .commit_inner_query_access_tokens(&parameters[0])
                        .await?,
                ))
            }
            Self::RepositoryCommitInnerQueryObjects => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .commit_inner_query_objects(&parameters[0])
                        .await?,
                ))
            }
            Self::RepositoryCommitInnerQueryCommits => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .commit_inner_query_commits(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::RepositoryCommitInnerQueryCrateNames => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .commit_inner_query_crate_names(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::RepositoryCommitInnerQueryRevisions => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .commit_inner_query_revisions(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::RepositoryCommitInnerQueryRevisions2 => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .commit_inner_query_revisions_2(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::RepositoryCommitInnerInsertCrateNames => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .repository
                        .commit_inner_insert_crate_names(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::RepositoryCommitInnerInsertObjects => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .repository
                        .commit_inner_insert_objects(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::RepositoryCommitInnerInsertRevisions => {
                ensure!(parameters.len() == 8, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .repository
                        .commit_inner_insert_revisions(RevisionWrite {
                            id: &parameters[0],
                            repository_id: &parameters[1],
                            name: &parameters[2],
                            version: &parameters[3],
                            sha256: &parameters[4],
                            metadata_sha256: &parameters[5],
                            entry_json: &parameters[6],
                            actor: &parameters[7],
                        })
                        .await?,
                ))
            }
            Self::RepositoryCommitInnerInsertSnapshotChanges => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .repository
                        .commit_inner_insert_snapshot_changes(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::RepositoryCommitInnerUpdateRepositories => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .repository
                        .commit_inner_update_repositories(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::RepositoryCommitInnerInsertCommits => {
                ensure!(parameters.len() == 5, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .repository
                        .commit_inner_insert_commits(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                            &parameters[4],
                        )
                        .await?,
                ))
            }
            Self::RepositoryCommitInnerInsertAudit => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .repository
                        .commit_inner_insert_audit(&parameters[0], &parameters[1], &parameters[2])
                        .await?,
                ))
            }
            Self::RepositoryCommitInnerUpdatePublishBatches => {
                ensure!(parameters.len() == 5, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .repository
                        .commit_inner_update_publish_batches(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                            &parameters[4],
                        )
                        .await?,
                ))
            }
            Self::RepositoryHeadQueryRepositories => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .head_query_repositories(&parameters[0])
                        .await?,
                ))
            }
            Self::RepositoryRepositoriesQueryRepositories => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers.repository.repositories_query_repositories().await?,
                ))
            }
            Self::RepositorySnapshotQueryRepositories => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .snapshot_query_repositories(&parameters[0])
                        .await?,
                ))
            }
            Self::RepositoryIndexEntriesQueryCrateNames => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .index_entries_query_crate_names(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::RepositoryIndexEntriesQueryRevisions => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .index_entries_query_revisions(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::RepositoryContainsObjectQueryArtifactAccessRevocations => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .contains_object_query_artifact_access_revocations(
                            &parameters[0],
                            &parameters[1],
                        )
                        .await?,
                ))
            }
            Self::RepositoryOpenSnapshotObjectQuerySnapshots => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .open_snapshot_object_query_snapshots(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::RepositoryOpenSnapshotObjectQueryArtifactAccessRevocations => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .open_snapshot_object_query_artifact_access_revocations(&parameters[0])
                        .await?,
                ))
            }
            Self::RepositoryOpenReaderQueryObjects => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .open_reader_query_objects(&parameters[0])
                        .await?,
                ))
            }
            Self::RepositoryCheckIntegrityQueryRepositories => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .check_integrity_query_repositories()
                        .await?,
                ))
            }
            Self::RepositoryCheckIntegrityQueryRevisions => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers.repository.check_integrity_query_revisions().await?,
                ))
            }
            Self::RepositoryCheckIntegrityQueryObjects => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers.repository.check_integrity_query_objects().await?,
                ))
            }
            Self::RepositoryCheckIntegrityQueryDeletionItems => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .check_integrity_query_deletion_items(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::RepositoryCheckIntegrityQueryRevisions2 => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .check_integrity_query_revisions_2()
                        .await?,
                ))
            }
            Self::RepositoryInsertSnapshotInsertSnapshots => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .repository
                        .insert_snapshot_insert_snapshots(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::RepositoryReadSnapshotQuerySnapshots => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .read_snapshot_query_snapshots(&parameters[0])
                        .await?,
                ))
            }
            Self::RepositoryReadEntryQuerySnapshotRemovals => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .read_entry_query_snapshot_removals(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::RepositoryReadEntryQuerySnapshotChanges => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .read_entry_query_snapshot_changes(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::RepositoryReadEntryQuerySnapshotCheckpoints => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .read_entry_query_snapshot_checkpoints(&parameters[0])
                        .await?,
                ))
            }
            Self::RepositoryReadEntryQueryCheckpointEntries => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .read_entry_query_checkpoint_entries(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::RepositoryMaterializeCheckpointQuerySnapshotRemovals => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .materialize_checkpoint_query_snapshot_removals(&parameters[0])
                        .await?,
                ))
            }
            Self::RepositoryMaterializeCheckpointQuerySnapshotChanges => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .materialize_checkpoint_query_snapshot_changes(&parameters[0])
                        .await?,
                ))
            }
            Self::RepositoryMaterializeCheckpointQuerySnapshotCheckpoints => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .materialize_checkpoint_query_snapshot_checkpoints(&parameters[0])
                        .await?,
                ))
            }
            Self::RepositoryMaterializeCheckpointQueryCheckpointEntries => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .materialize_checkpoint_query_checkpoint_entries(&parameters[0])
                        .await?,
                ))
            }
            Self::RepositoryMaterializeCheckpointInsertCheckpointEntries => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .repository
                        .materialize_checkpoint_insert_checkpoint_entries(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::RepositoryMaterializeCheckpointInsertSnapshotCheckpoints => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .repository
                        .materialize_checkpoint_insert_snapshot_checkpoints(&parameters[0])
                        .await?,
                ))
            }
            Self::ResolutionResolveAtQuerySnapshots => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .resolution
                        .resolve_at_query_snapshots(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::ResolutionResolveAtQueryRepositories => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .resolution
                        .resolve_at_query_repositories(&parameters[0])
                        .await?,
                ))
            }
            Self::ResolutionResolveAtQueryArtifactAccessRevocations => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .resolution
                        .resolve_at_query_artifact_access_revocations(&parameters[0])
                        .await?,
                ))
            }
            Self::ResolutionResolveAtDeleteRetentionLeases => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .resolution
                        .resolve_at_delete_retention_leases(&parameters[0])
                        .await?,
                ))
            }
            Self::ResolutionResolveAtQueryRetentionLeases => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .resolution
                        .resolve_at_query_retention_leases(&parameters[0])
                        .await?,
                ))
            }
            Self::ResolutionResolveAtInsertRetentionLeases => {
                ensure!(parameters.len() == 5, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .resolution
                        .resolve_at_insert_retention_leases(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                            &parameters[4],
                        )
                        .await?,
                ))
            }
            Self::ResolutionRenewLeaseProofUpdateRetentionLeases => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .resolution
                        .renew_lease_proof_update_retention_leases(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::ResolutionReleaseLeaseDeleteRetentionLeases => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .resolution
                        .release_lease_delete_retention_leases(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::ResolutionRevisionAtQuerySnapshotRemovals => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .resolution
                        .revision_at_query_snapshot_removals(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::ResolutionRevisionAtQuerySnapshotChanges => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .resolution
                        .revision_at_query_snapshot_changes(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::ResolutionRevisionAtQuerySnapshotCheckpoints => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .resolution
                        .revision_at_query_snapshot_checkpoints(&parameters[0])
                        .await?,
                ))
            }
            Self::ResolutionRevisionAtQueryCheckpointEntries => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .resolution
                        .revision_at_query_checkpoint_entries(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::ResolutionRevisionAtQueryRevisions => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .resolution
                        .revision_at_query_revisions(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::RetentionRetentionPinsQueryRetentionPins => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .retention
                        .retention_pins_query_retention_pins(&parameters[0])
                        .await?,
                ))
            }
            Self::RetentionPinSnapshotQueryRepositories => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .retention
                        .pin_snapshot_query_repositories(&parameters[0])
                        .await?,
                ))
            }
            Self::RetentionPinSnapshotQueryRetentionPins => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .retention
                        .pin_snapshot_query_retention_pins(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::RetentionPinSnapshotQueryRetentionPins2 => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .retention
                        .pin_snapshot_query_retention_pins_2(&parameters[0])
                        .await?,
                ))
            }
            Self::RetentionPinSnapshotQueryRevisions => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .retention
                        .pin_snapshot_query_revisions(&parameters[0])
                        .await?,
                ))
            }
            Self::RetentionPinSnapshotInsertRetentionPins => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .retention
                        .pin_snapshot_insert_retention_pins(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::RetentionPinSnapshotInsertSecurityAudit => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .retention
                        .pin_snapshot_insert_security_audit(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::RetentionReleasePinQueryRetentionPins => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .retention
                        .release_pin_query_retention_pins(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::RetentionReleasePinDeleteRetentionPins => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .retention
                        .release_pin_delete_retention_pins(&parameters[0])
                        .await?,
                ))
            }
            Self::RetentionReleasePinInsertSecurityAudit => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .retention
                        .release_pin_insert_security_audit(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::RetentionSnapshotRevisionsQuerySnapshotRemovals => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .retention
                        .snapshot_revisions_query_snapshot_removals(&parameters[0])
                        .await?,
                ))
            }
            Self::RetentionSnapshotRevisionsQuerySnapshotCheckpoints => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .retention
                        .snapshot_revisions_query_snapshot_checkpoints(&parameters[0])
                        .await?,
                ))
            }
            Self::RetentionSnapshotRevisionsQuerySnapshotChanges => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .retention
                        .snapshot_revisions_query_snapshot_changes(&parameters[0])
                        .await?,
                ))
            }
            Self::SchemaRevisionQuerySqliteSchema => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers.schema.revision_query_sqlite_schema().await?,
                ))
            }
            Self::SchemaRevisionQueryMetadata => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers.schema.revision_query_metadata().await?,
                ))
            }
            Self::SchemaRepositoryCreateMetadata => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.repository_create_metadata().await?,
                ))
            }
            Self::SchemaRepositoryCreateRepositories => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.repository_create_repositories().await?,
                ))
            }
            Self::SchemaRepositoryCreateSnapshots => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.repository_create_snapshots().await?,
                ))
            }
            Self::SchemaRepositoryCreateObjects => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.repository_create_objects().await?,
                ))
            }
            Self::SchemaRepositoryCreateCrateNames => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.repository_create_crate_names().await?,
                ))
            }
            Self::SchemaRepositoryCreateRevisions => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.repository_create_revisions().await?,
                ))
            }
            Self::SchemaRepositoryCreateSnapshotChanges => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.repository_create_snapshot_changes().await?,
                ))
            }
            Self::SchemaRepositoryCreateCommits => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.repository_create_commits().await?,
                ))
            }
            Self::SchemaRepositoryCreateAudit => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.repository_create_audit().await?,
                ))
            }
            Self::SchemaRepositoryCreateValue => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.repository_create_value().await?,
                ))
            }
            Self::SchemaRepositoryInsertMetadata => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .repository_insert_metadata(&parameters[0])
                        .await?,
                ))
            }
            Self::SchemaSnapshotCheckpointsCreateSnapshotCheckpoints => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .snapshot_checkpoints_create_snapshot_checkpoints()
                        .await?,
                ))
            }
            Self::SchemaSnapshotCheckpointsCreateCheckpointEntries => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .snapshot_checkpoints_create_checkpoint_entries()
                        .await?,
                ))
            }
            Self::SchemaSnapshotCheckpointsInsertMetadata => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .snapshot_checkpoints_insert_metadata(&parameters[0])
                        .await?,
                ))
            }
            Self::SchemaPublishBatchesCreatePublishBatches => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .publish_batches_create_publish_batches()
                        .await?,
                ))
            }
            Self::SchemaPublishBatchesCreateBatchItems => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.publish_batches_create_batch_items().await?,
                ))
            }
            Self::SchemaPublishBatchesCreateValue => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.publish_batches_create_value().await?,
                ))
            }
            Self::SchemaPublishBatchesInsertMetadata => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .publish_batches_insert_metadata(&parameters[0])
                        .await?,
                ))
            }
            Self::SchemaRetentionLeasesCreateRetentionLeases => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .retention_leases_create_retention_leases()
                        .await?,
                ))
            }
            Self::SchemaRetentionLeasesCreateValue => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.retention_leases_create_value().await?,
                ))
            }
            Self::SchemaRetentionLeasesInsertMetadata => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .retention_leases_insert_metadata(&parameters[0])
                        .await?,
                ))
            }
            Self::SchemaProxyCacheCreateProxyRepositories => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .proxy_cache_create_proxy_repositories()
                        .await?,
                ))
            }
            Self::SchemaProxyCacheCreateProxyIndexes => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.proxy_cache_create_proxy_indexes().await?,
                ))
            }
            Self::SchemaProxyCacheCreateProxyCoordinates => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .proxy_cache_create_proxy_coordinates()
                        .await?,
                ))
            }
            Self::SchemaProxyCacheCreateProxyObjects => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.proxy_cache_create_proxy_objects().await?,
                ))
            }
            Self::SchemaProxyCacheInsertMetadata => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .proxy_cache_insert_metadata(&parameters[0])
                        .await?,
                ))
            }
            Self::SchemaProxyResolutionCreateProxySessions => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .proxy_resolution_create_proxy_sessions()
                        .await?,
                ))
            }
            Self::SchemaProxyResolutionCreateProxySessionEntries => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .proxy_resolution_create_proxy_session_entries()
                        .await?,
                ))
            }
            Self::SchemaProxyResolutionCreateProxyValidators => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .proxy_resolution_create_proxy_validators()
                        .await?,
                ))
            }
            Self::SchemaProxyResolutionCreateProxyMissing => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .proxy_resolution_create_proxy_missing()
                        .await?,
                ))
            }
            Self::SchemaProxyResolutionCreateProxyMaterializations => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .proxy_resolution_create_proxy_materializations()
                        .await?,
                ))
            }
            Self::SchemaProxyResolutionCreateValue => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.proxy_resolution_create_value().await?,
                ))
            }
            Self::SchemaProxyResolutionCreateMaterializationItems => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .proxy_resolution_create_materialization_items()
                        .await?,
                ))
            }
            Self::SchemaProxyResolutionInsertMetadata => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .proxy_resolution_insert_metadata(&parameters[0])
                        .await?,
                ))
            }
            Self::SchemaAccessControlCreateAdminUsers => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.access_control_create_admin_users().await?,
                ))
            }
            Self::SchemaAccessControlCreateAdminSessions => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .access_control_create_admin_sessions()
                        .await?,
                ))
            }
            Self::SchemaAccessControlCreateAccessTokens => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.access_control_create_access_tokens().await?,
                ))
            }
            Self::SchemaAccessControlCreateSecurityAudit => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .access_control_create_security_audit()
                        .await?,
                ))
            }
            Self::SchemaAccessControlInsertMetadata => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .access_control_insert_metadata(&parameters[0])
                        .await?,
                ))
            }
            Self::SchemaHostedManagementCreateSnapshotRemovals => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .hosted_management_create_snapshot_removals()
                        .await?,
                ))
            }
            Self::SchemaHostedManagementCreateManagementPreviews => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .hosted_management_create_management_previews()
                        .await?,
                ))
            }
            Self::SchemaHostedManagementInsertMetadata => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .hosted_management_insert_metadata(&parameters[0])
                        .await?,
                ))
            }
            Self::SchemaObjectLifecycleAlterObjects => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.object_lifecycle_alter_objects().await?,
                ))
            }
            Self::SchemaObjectLifecycleAlterObjects2 => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.object_lifecycle_alter_objects_2().await?,
                ))
            }
            Self::SchemaObjectLifecycleCreateArtifactAccessRevocations => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .object_lifecycle_create_artifact_access_revocations()
                        .await?,
                ))
            }
            Self::SchemaObjectLifecycleCreateRetentionPins => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .object_lifecycle_create_retention_pins()
                        .await?,
                ))
            }
            Self::SchemaObjectLifecycleCreateDeletionPreviews => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .object_lifecycle_create_deletion_previews()
                        .await?,
                ))
            }
            Self::SchemaObjectLifecycleCreateObjectDeletions => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .object_lifecycle_create_object_deletions()
                        .await?,
                ))
            }
            Self::SchemaObjectLifecycleCreateDeletionItems => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .object_lifecycle_create_deletion_items()
                        .await?,
                ))
            }
            Self::SchemaObjectLifecycleCreateProxySessionObjects => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .object_lifecycle_create_proxy_session_objects()
                        .await?,
                ))
            }
            Self::SchemaObjectLifecycleInsertMetadata => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .object_lifecycle_insert_metadata(&parameters[0])
                        .await?,
                ))
            }
            Self::SchemaProxyViewsAlterProxySessions => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.proxy_views_alter_proxy_sessions().await?,
                ))
            }
            Self::SchemaProxyViewsInsertMetadata => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .proxy_views_insert_metadata(&parameters[0])
                        .await?,
                ))
            }
            Self::SchemaDeletionRetriesAlterObjectDeletions => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .deletion_retries_alter_object_deletions()
                        .await?,
                ))
            }
            Self::SchemaDeletionRetriesAlterObjectDeletions2 => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .deletion_retries_alter_object_deletions_2()
                        .await?,
                ))
            }
            Self::SchemaDeletionRetriesAlterObjectDeletions3 => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .deletion_retries_alter_object_deletions_3()
                        .await?,
                ))
            }
            Self::SchemaDeletionRetriesCreateValue => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.deletion_retries_create_value().await?,
                ))
            }
            Self::SchemaDeletionRetriesInsertMetadata => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .deletion_retries_insert_metadata(&parameters[0])
                        .await?,
                ))
            }
            Self::SchemaTokenPublishScopeAlterAccessTokens => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .token_publish_scope_alter_access_tokens()
                        .await?,
                ))
            }
            Self::SchemaTokenPublishScopeAlterAccessTokens2 => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .token_publish_scope_alter_access_tokens_2()
                        .await?,
                ))
            }
            Self::SchemaTokenPublishScopeInsertMetadata => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .token_publish_scope_insert_metadata(&parameters[0])
                        .await?,
                ))
            }
            Self::SchemaUnpublishedObjectsAlterObjects => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.unpublished_objects_alter_objects().await?,
                ))
            }
            Self::SchemaUnpublishedObjectsUpdateObjects => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.unpublished_objects_update_objects().await?,
                ))
            }
            Self::SchemaUnpublishedObjectsCreateValue => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.unpublished_objects_create_value().await?,
                ))
            }
            Self::SchemaUnpublishedObjectsCreateValue2 => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.unpublished_objects_create_value_2().await?,
                ))
            }
            Self::SchemaUnpublishedObjectsCreateValue3 => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.unpublished_objects_create_value_3().await?,
                ))
            }
            Self::SchemaUnpublishedObjectsCreateValue4 => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.unpublished_objects_create_value_4().await?,
                ))
            }
            Self::SchemaUnpublishedObjectsCreateValue5 => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.unpublished_objects_create_value_5().await?,
                ))
            }
            Self::SchemaUnpublishedObjectsCreateValue6 => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers.schema.unpublished_objects_create_value_6().await?,
                ))
            }
            Self::SchemaUnpublishedObjectsInsertMetadata => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .schema
                        .unpublished_objects_insert_metadata(&parameters[0])
                        .await?,
                ))
            }
            Self::SecurityTokensQueryAccessTokens => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .tokens_query_access_tokens(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::SecuritySecurityAuditQuerySecurityAudit => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .security_audit_query_security_audit(&parameters[0])
                        .await?,
                ))
            }
            Self::SecuritySecurityAuditPageQuerySecurityAudit => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .security_audit_page_query_security_audit(&parameters[0])
                        .await?,
                ))
            }
            Self::SecuritySetUserGrantsQueryAdminUsers => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .set_user_grants_query_admin_users(&parameters[0])
                        .await?,
                ))
            }
            Self::SecuritySetUserGrantsQueryAdminUsers2 => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .set_user_grants_query_admin_users_2(&parameters[0])
                        .await?,
                ))
            }
            Self::SecuritySetUserGrantsQueryAdminUsers3 => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .set_user_grants_query_admin_users_3(&parameters[0])
                        .await?,
                ))
            }
            Self::SecuritySetUserGrantsUpdateAdminUsers => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .set_user_grants_update_admin_users(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::SecuritySetUserGrantsInsertSecurityAudit => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .set_user_grants_insert_security_audit(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::SecuritySetUserEnabledQueryAdminUsers => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .set_user_enabled_query_admin_users(&parameters[0])
                        .await?,
                ))
            }
            Self::SecuritySetUserEnabledQueryAdminUsers2 => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .set_user_enabled_query_admin_users_2(&parameters[0])
                        .await?,
                ))
            }
            Self::SecuritySetUserEnabledQueryAdminUsers3 => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .set_user_enabled_query_admin_users_3(&parameters[0])
                        .await?,
                ))
            }
            Self::SecuritySetUserEnabledUpdateAdminUsers => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .set_user_enabled_update_admin_users(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::SecuritySetUserEnabledInsertSecurityAudit => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .set_user_enabled_insert_security_audit(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::SecurityBootstrapStatusQueryAdminUsers => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .bootstrap_status_query_admin_users()
                        .await?,
                ))
            }
            Self::SecurityBootstrapAdminQueryAdminUsers => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers.security.bootstrap_admin_query_admin_users().await?,
                ))
            }
            Self::SecurityBootstrapAdminInsertAdminUsers => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .bootstrap_admin_insert_admin_users(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::SecurityBootstrapAdminInsertSecurityAudit => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .bootstrap_admin_insert_security_audit(&parameters[0])
                        .await?,
                ))
            }
            Self::SecurityFinishLoginDeleteAdminSessions => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .finish_login_delete_admin_sessions()
                        .await?,
                ))
            }
            Self::SecurityFinishLoginQueryAdminUsers => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .finish_login_query_admin_users(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::SecurityFinishLoginQueryAdminSessions => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .finish_login_query_admin_sessions(&parameters[0])
                        .await?,
                ))
            }
            Self::SecurityFinishLoginUpdateAdminUsers => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .finish_login_update_admin_users(&parameters[0])
                        .await?,
                ))
            }
            Self::SecurityFinishLoginInsertAdminSessions => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .finish_login_insert_admin_sessions(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::SecurityFinishLoginUpdateAdminUsers2 => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .finish_login_update_admin_users_2(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::SecurityFinishLoginInsertSecurityAudit => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .finish_login_insert_security_audit(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::SecurityBrowserIdentityQueryAdminSessions => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .browser_identity_query_admin_sessions(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::SecurityLogoutDeleteAdminSessions => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .logout_delete_admin_sessions(&parameters[0])
                        .await?,
                ))
            }
            Self::SecurityUsersQueryAdminUsers => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers.security.users_query_admin_users().await?,
                ))
            }
            Self::SecurityCreateUserQueryAdminUsers => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .create_user_query_admin_users(&parameters[0])
                        .await?,
                ))
            }
            Self::SecurityCreateUserQueryAdminUsers2 => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .create_user_query_admin_users_2(&parameters[0])
                        .await?,
                ))
            }
            Self::SecurityCreateUserQueryAdminUsers3 => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers.security.create_user_query_admin_users_3().await?,
                ))
            }
            Self::SecurityCreateUserInsertAdminUsers => {
                ensure!(parameters.len() == 5, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .create_user_insert_admin_users(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                            &parameters[4],
                        )
                        .await?,
                ))
            }
            Self::SecurityCreateUserInsertSecurityAudit => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .create_user_insert_security_audit(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::SecurityChangePasswordUpdateAdminUsers => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .change_password_update_admin_users(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::SecurityChangePasswordDeleteAdminSessions => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .change_password_delete_admin_sessions(&parameters[0])
                        .await?,
                ))
            }
            Self::SecurityChangePasswordInsertSecurityAudit => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .change_password_insert_security_audit(&parameters[0])
                        .await?,
                ))
            }
            Self::SecurityCreateScopedTokenQueryAccessTokens => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .create_scoped_token_query_access_tokens(&parameters[0])
                        .await?,
                ))
            }
            Self::SecurityCreateScopedTokenInsertAccessTokens => {
                ensure!(parameters.len() == 10, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .create_scoped_token_insert_access_tokens(TokenWrite {
                            id: &parameters[0],
                            digest: &parameters[1],
                            user_id: &parameters[2],
                            label: &parameters[3],
                            permission: &parameters[4],
                            repositories: &parameters[5],
                            auth_version: &parameters[6],
                            expires_at: &parameters[7],
                            allow_overwrite: &parameters[8],
                            publish_crates: &parameters[9],
                        })
                        .await?,
                ))
            }
            Self::SecurityCreateScopedTokenInsertSecurityAudit => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .create_scoped_token_insert_security_audit(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::SecurityRevokeTokenQueryAccessTokens => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .revoke_token_query_access_tokens(&parameters[0])
                        .await?,
                ))
            }
            Self::SecurityRevokeTokenUpdateAccessTokens => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .revoke_token_update_access_tokens(&parameters[0])
                        .await?,
                ))
            }
            Self::SecurityRevokeTokenInsertSecurityAudit => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .security
                        .revoke_token_insert_security_audit(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::SecurityBrowserActorQueryAdminUsers => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .browser_actor_query_admin_users(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::SecurityTokenIdentityOnQueryAccessTokens => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .token_identity_on_query_access_tokens(&parameters[0])
                        .await?,
                ))
            }
            Self::UnpublishedCollectUnpublishedQueryObjectDeletions => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .unpublished
                        .collect_unpublished_query_object_deletions()
                        .await?,
                ))
            }
            Self::UnpublishedCollectUnpublishedQueryProxySessionEntries => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .unpublished
                        .collect_unpublished_query_proxy_session_entries()
                        .await?,
                ))
            }
            Self::UnpublishedCollectUnpublishedQueryObjects => {
                ensure!(parameters.is_empty(), "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .unpublished
                        .collect_unpublished_query_objects()
                        .await?,
                ))
            }
            Self::UnpublishedCollectUnpublishedInsertObjectDeletions => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .unpublished
                        .collect_unpublished_insert_object_deletions(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::UnpublishedCollectUnpublishedUpdateObjects => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .unpublished
                        .collect_unpublished_update_objects(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::UnpublishedCollectUnpublishedInsertDeletionItems => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .unpublished
                        .collect_unpublished_insert_deletion_items(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::UnpublishedCollectUnpublishedInsertSecurityAudit => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .unpublished
                        .collect_unpublished_insert_security_audit(&parameters[0])
                        .await?,
                ))
            }
            Self::UnpublishedRecoverUnpublishedQueryObjects => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .unpublished
                        .recover_unpublished_query_objects(&parameters[0])
                        .await?,
                ))
            }
            Self::UnpublishedRecoverUnpublishedInsertObjects => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .unpublished
                        .recover_unpublished_insert_objects(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::UnpublishedRecoverUnpublishedUpdateObjects => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Affected(
                    mappers
                        .unpublished
                        .recover_unpublished_update_objects(&parameters[0])
                        .await?,
                ))
            }
            Self::ManagementRevisionHistoryPageQueryRevisions => {
                ensure!(parameters.len() == 4, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .management
                        .revision_history_page_query_revisions(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                            &parameters[3],
                        )
                        .await?,
                ))
            }
            Self::ProxyProxyIndexQueryProxyIndexes => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy
                        .proxy_index_query_proxy_indexes(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::ProxyResolutionProxySessionQueryProxySessions => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy_resolution
                        .proxy_session_query_proxy_sessions(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::ProxyResolutionUpstreamValidationQueryProxyValidators => {
                ensure!(parameters.len() == 3, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .proxy_resolution
                        .upstream_validation_query_proxy_validators(
                            &parameters[0],
                            &parameters[1],
                            &parameters[2],
                        )
                        .await?,
                ))
            }
            Self::RepositoryContainsObjectQueryRevisions => {
                ensure!(parameters.len() == 2, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .repository
                        .contains_object_query_revisions(&parameters[0], &parameters[1])
                        .await?,
                ))
            }
            Self::SecurityLoginUserQueryAdminUsers => {
                ensure!(parameters.len() == 1, "invalid Mapper binding count");
                Ok(Output::Rows(
                    mappers
                        .security
                        .login_user_query_admin_users(&parameters[0])
                        .await?,
                ))
            }
        }
    }
}
