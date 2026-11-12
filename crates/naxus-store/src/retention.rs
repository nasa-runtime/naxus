use std::collections::{BTreeMap, BTreeSet};

use crate::database::{params, Connection, OptionalExtension, TransactionBehavior};
use anyhow::{ensure, Result};
use serde::Serialize;
use uuid::Uuid;

use crate::{management::management_permission, repository::read_snapshot, Store};

/// 运维明确保留的 hosted 快照，独立于短期构建租约。
#[derive(Serialize)]
pub struct RetentionPin {
    pub id: String,
    pub snapshot_id: String,
    pub actor: String,
    pub reason: String,
}

impl Store {
    /// 业务作用：列出仓库中显式保留的历史快照，供运维理解物理清除阻挡。
    /// 参数说明：
    /// - `repository`：已授权仓库名称或 UUID。
    ///
    /// 返回：最多四千零九十六个保留点，不改变引用或 HEAD。
    pub fn retention_pins(&self, repository: &str) -> Result<Vec<RetentionPin>> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(crate::statements::Statement::RetentionRetentionPinsQueryRetentionPins)?;
        let pins = statement
            .query_map([repository], |row| {
                Ok(RetentionPin {
                    id: row.get(0)?,
                    snapshot_id: row.get(1)?,
                    actor: row.get(2)?,
                    reason: row.get(3)?,
                })
            })?
            .collect::<sqlx::Result<Vec<_>>>()?;
        Ok(pins)
    }

    /// 业务作用：为仍完整可读的历史快照建立显式保留点，防止以后物理清除其选中修订。
    /// 参数说明：
    /// - `actor`：请求认证时固定的浏览器身份及账号代次。
    /// - `repository`：目标仓库。
    /// - `snapshot`：已经存在的同仓快照 UUID。
    /// - `reason`：不含凭据的保留原因。
    ///
    /// 返回：保留点 UUID；跨仓选择为输入拒绝，已撤销或不可读历史为状态冲突，超出预算或权限不符分别拒绝，不恢复已清除内容。
    pub fn pin_snapshot(
        &self,
        actor: &crate::AccessIdentity,
        repository: &str,
        snapshot: &str,
        reason: &str,
    ) -> Result<String> {
        ensure!(
            !reason.is_empty() && reason.len() <= 512 && !reason.chars().any(char::is_control),
            crate::RepositoryFailure::InvalidRequest("invalid retention reason")
        );
        let _objects = self
            .object_gate
            .lock()
            .map_err(|_| anyhow::anyhow!("object lifecycle unavailable"))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actor = crate::security::browser_actor(&transaction, actor)?;
        let repository_id: String = transaction.query_row(
            crate::statements::Statement::RetentionPinSnapshotQueryRepositories,
            [repository],
            |row| row.get(0),
        )?;
        // 先确认目标仓库管理权，再检查所选历史，越权请求不能借保留接口查询快照状态。
        management_permission(&transaction, actor, &repository_id)?;
        let selected = read_snapshot(&transaction, snapshot)?;
        ensure!(
            selected.repository_id == repository_id,
            crate::RepositoryFailure::InvalidRequest(
                "retention snapshot belongs to another repository"
            )
        );
        let existing: Option<String> = transaction
            .query_row(
                crate::statements::Statement::RetentionPinSnapshotQueryRetentionPins,
                params![actor, snapshot],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Ok(id);
        }
        let (total, owned): (u64, u64) = transaction.query_row(
            crate::statements::Statement::RetentionPinSnapshotQueryRetentionPins2,
            [actor],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        ensure!(
            total < 4096 && owned < 128,
            crate::ResourceLimitExceeded("retention pin budget exceeded")
        );
        let revisions =
            snapshot_revisions(&transaction, snapshot, self.options.max_snapshot_depth)?;
        // 保留权与对象清除共用生命周期门禁，已经撤销的历史不能借保留点重新开放。
        for revision in revisions {
            let valid: bool = transaction.query_row(
                crate::statements::Statement::RetentionPinSnapshotQueryRevisions,
                [revision],
                |row| row.get(0),
            )?;
            ensure!(
                valid,
                crate::RepositoryFailure::Conflict(
                    "retention snapshot includes revoked or unavailable revisions".to_owned()
                )
            );
        }
        let id = Uuid::new_v4().to_string();
        transaction.execute(
            crate::statements::Statement::RetentionPinSnapshotInsertRetentionPins,
            params![id, snapshot, actor, reason],
        )?;
        transaction.execute(
            crate::statements::Statement::RetentionPinSnapshotInsertSecurityAudit,
            params![actor, id],
        )?;
        transaction.commit()?;
        Ok(id)
    }

    /// 业务作用：运维显式解除目标保留点，解除本身不删除快照或对象。
    /// 参数说明：
    /// - `actor`：请求认证时固定的浏览器身份及账号代次。
    /// - `repository`：保留点所在仓库。
    /// - `id`：精确保留点 UUID。
    ///
    /// 返回：同仓且仍有管理权限才解除；物理清除仍需另行预览并受其他引用保护。
    pub fn release_pin(
        &self,
        actor: &crate::AccessIdentity,
        repository: &str,
        id: &str,
    ) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actor = crate::security::browser_actor(&transaction, actor)?;
        let repository_id: String = transaction.query_row(
            crate::statements::Statement::RetentionReleasePinQueryRetentionPins,
            params![id, repository],
            |row| row.get(0),
        )?;
        management_permission(&transaction, actor, &repository_id)?;
        transaction.execute(
            crate::statements::Statement::RetentionReleasePinDeleteRetentionPins,
            [id],
        )?;
        transaction.execute(
            crate::statements::Statement::RetentionReleasePinInsertSecurityAudit,
            params![actor, id],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

/// 业务作用：按与读取相同的移除及检查点顺序，计算保留快照真正选中的修订。
/// 参数说明：
/// - `connection`：本次保留事务。
/// - `snapshot`：需要保留的历史快照。
/// - `depth`：父链读取预算。
///
/// 返回：最多四千零九十六个可见坐标的实际修订；循环、跨仓历史或超额拒绝。
fn snapshot_revisions(
    connection: &Connection,
    snapshot: &str,
    depth: usize,
) -> Result<Vec<String>> {
    let repository = read_snapshot(connection, snapshot)?.repository_id;
    let mut current = Some(snapshot.to_owned());
    let mut visited = BTreeSet::new();
    let mut visible = BTreeMap::new();
    while let Some(id) = current {
        ensure!(
            visited.len() < depth && visited.insert(id.clone()),
            "invalid retention snapshot lineage"
        );
        let selected = read_snapshot(connection, &id)?;
        ensure!(
            selected.repository_id == repository,
            "retention snapshot crosses repositories"
        );
        let mut removals = connection.prepare(
            crate::statements::Statement::RetentionSnapshotRevisionsQuerySnapshotRemovals,
        )?;
        for coordinate in removals.query_map([&id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            visible.entry(coordinate?).or_insert(None);
            ensure!(
                visible.len() <= 4096,
                crate::ResourceLimitExceeded("retention snapshot coordinate budget exceeded")
            );
        }
        let checkpoint: bool = connection.query_row(
            crate::statements::Statement::RetentionSnapshotRevisionsQuerySnapshotCheckpoints,
            [&id],
            |row| row.get(0),
        )?;
        let mut entries = connection.prepare(
            crate::statements::Statement::RetentionSnapshotRevisionsQuerySnapshotChanges,
        )?;
        let mut count = 0;
        for entry in entries.query_map([&id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })? {
            count += 1;
            ensure!(
                count <= 8192,
                crate::ResourceLimitExceeded("retention checkpoint budget exceeded")
            );
            let (name, version, revision) = entry?;
            visible.entry((name, version)).or_insert(Some(revision));
            ensure!(
                visible.len() <= 4096,
                crate::ResourceLimitExceeded("retention snapshot coordinate budget exceeded")
            );
        }
        if checkpoint {
            break;
        }
        current = selected.parent_id;
    }
    Ok(visible.into_values().flatten().collect())
}
