use std::{
    collections::BTreeSet,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{ensure, Result};
use naxus_core::{Coordinate, LeaseProof, Resolution, ResolvedPackage, Revision};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use uuid::Uuid;

use crate::{repository::read_snapshot, Store};

impl Store {
    /// 业务作用：在单一事务固定 HEAD、查询全部已选坐标，并建立保留租约。
    /// 参数说明：
    /// - `repository`：已授权仓库名称或 UUID。
    /// - `coordinates`：客户端当前已选精确坐标。
    /// - `actor`：经过认证的租约主体。
    ///
    /// 返回：一致快照、逐坐标修订和十分钟租约；缺失坐标显式返回 None。
    pub fn resolve(
        &self,
        repository: &str,
        coordinates: &[Coordinate],
        actor: &str,
    ) -> Result<Resolution> {
        self.resolve_at(repository, coordinates, actor, None)
    }

    /// 业务作用：为首次固定或后续锁定读取查询修订并取得租约。
    /// 参数说明：
    /// - `repository`：已授权仓库。
    /// - `coordinates`：精确坐标集合。
    /// - `actor`：当前认证主体。
    /// - `fixed_snapshot`：已固定快照；None 时才读取 HEAD。
    ///
    /// 返回：一致快照与修订，固定读取不观察后来的 HEAD。
    pub fn resolve_at(
        &self,
        repository: &str,
        coordinates: &[Coordinate],
        actor: &str,
        fixed_snapshot: Option<&str>,
    ) -> Result<Resolution> {
        ensure!(
            coordinates.len() <= 4096 && !actor.is_empty() && actor.len() <= 256,
            crate::RepositoryFailure::InvalidRequest("invalid resolution request")
        );
        for coordinate in coordinates {
            naxus_core::validate_name(&coordinate.name)
                .map_err(|_| crate::RepositoryFailure::InvalidRequest("invalid resolution name"))?;
            semver::Version::parse(&coordinate.version).map_err(|_| {
                crate::RepositoryFailure::InvalidRequest("invalid resolution version")
            })?;
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let head: String = match fixed_snapshot {
            Some(snapshot) => transaction.query_row("SELECT s.id FROM snapshots s JOIN repositories r ON r.id = s.repository_id WHERE s.id = ?1 AND (r.name = ?2 OR r.id = ?2)", params![snapshot, repository], |row| row.get(0))?,
            None => transaction.query_row("SELECT head FROM repositories WHERE name = ?1 OR id = ?1", [repository], |row| row.get(0))?,
        };
        let snapshot = read_snapshot(&transaction, &head)?;
        let mut packages = Vec::new();
        for coordinate in coordinates {
            let revision = revision_at(
                &transaction,
                &head,
                coordinate,
                self.options.max_snapshot_depth,
            )?;
            if let Some(revision) = &revision {
                let revoked: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM artifact_access_revocations WHERE revision_id = ?1)", [&revision.id], |row| row.get(0))?;
                if revoked {
                    return Err(crate::ArtifactGone.into());
                }
            }
            packages.push(ResolvedPackage {
                coordinate: coordinate.clone(),
                revision,
            });
        }
        let resolution = Resolution {
            snapshot_set_id: Uuid::new_v4().to_string(),
            snapshot,
            packages,
            lease_id: Uuid::new_v4().to_string(),
            lease_expires_at: now.saturating_add(600),
        };
        // HEAD 与租约同时提交，回收不能在“已经选中但尚未保留”的窗口移走依赖对象。
        transaction.execute("DELETE FROM retention_leases WHERE expires_at <= ?1", [now])?;
        let (total, active): (u64, u64) = transaction.query_row(
            "SELECT count(*), coalesce(sum(actor = ?1), 0) FROM retention_leases",
            [actor],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        ensure!(
            total < 8192 && active < 256,
            crate::ResourceLimitExceeded("retention lease budget exceeded")
        );
        transaction.execute("INSERT INTO retention_leases(id, snapshot_set_id, snapshot_id, actor, expires_at) VALUES (?1, ?2, ?3, ?4, ?5)", params![resolution.lease_id, resolution.snapshot_set_id, resolution.snapshot.id, actor, resolution.lease_expires_at])?;
        transaction.commit()?;
        Ok(resolution)
    }

    /// 业务作用：延长当前主体仍然持有的保留租约，防止长时间构建期间历史对象进入清除流程。
    /// 参数说明：
    /// - `repository`：已授权的仓库名称或 UUID。
    /// - `lease`：解析响应建立的租约 UUID。
    /// - `actor`：创建该租约的认证主体。
    ///
    /// 返回：新的到期 Unix 秒；租约已到期、已关闭或归属不符时拒绝，不能恢复失去的保护权。
    pub fn renew_lease(&self, repository: &str, lease: &str, actor: &str) -> Result<u64> {
        Ok(self.renew_lease_proof(repository, lease, actor)?.expires_at)
    }

    /// 业务作用：在同一事务续期并读回保护对象，向客户端证明租约未被错配到其他快照集合。
    /// 参数说明：
    /// - `repository`：已授权仓库名称或 UUID。
    /// - `lease`：原解析响应给出的租约 UUID。
    /// - `actor`：建立租约的当前主体。
    ///
    /// 返回：归属、未过期状态及快照一致的持久证据；未知、过期及归属不符统一按不存在拒绝，不复活已到期租约。
    pub fn renew_lease_proof(
        &self,
        repository: &str,
        lease: &str,
        actor: &str,
    ) -> Result<LeaseProof> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // 期限与归属在同一条写语句复验；已到期的引用不能重新阻挡已经获准的清除任务。
        let proof = transaction.query_row("UPDATE retention_leases SET expires_at = unixepoch() + 600 WHERE id = ?1 AND actor = ?2 AND expires_at > unixepoch() AND snapshot_id IN (SELECT s.id FROM snapshots s JOIN repositories r ON r.id = s.repository_id WHERE r.id = ?3 OR r.name = ?3) RETURNING snapshot_set_id, snapshot_id, expires_at", params![lease, actor, repository], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, u64>(2)?))).optional()?.ok_or(crate::RepositoryFailure::NotFound("retention lease is unavailable"))?;
        let snapshot = read_snapshot(&transaction, &proof.1)?;
        let proof = LeaseProof {
            id: lease.to_owned(),
            repository_id: snapshot.repository_id,
            snapshot_id: proof.1,
            snapshot_set_id: proof.0,
            expires_at: proof.2,
        };
        transaction.commit()?;
        Ok(proof)
    }

    /// 业务作用：构建结束后主动释放当前主体的快照保留权，减少无用引用占用清除窗口。
    /// 参数说明：
    /// - `repository`：已授权仓库。
    /// - `lease`：需要释放的精确租约。
    /// - `actor`：创建租约的主体。
    ///
    /// 返回：只删除归属一致的租约，重复释放不改变其他主体或仓库的保护状态。
    pub fn release_lease(&self, repository: &str, lease: &str, actor: &str) -> Result<()> {
        self.connection()?.execute("DELETE FROM retention_leases WHERE id = ?1 AND actor = ?2 AND snapshot_id IN (SELECT s.id FROM snapshots s JOIN repositories r ON r.id = s.repository_id WHERE r.id = ?3 OR r.name = ?3)", params![lease, actor, repository])?;
        Ok(())
    }
}

/// 业务作用：读取快照实际选中的修订 ID，保留回退和重复内容发布的准确身份。
/// 参数说明：
/// - `connection`：固定读取事务。
/// - `snapshot`：目标快照。
/// - `coordinate`：精确坐标。
/// - `limit`：父链深度预算。
///
/// 返回：最近变更或检查点中的真实修订；不存在返回 None，异常历史拒绝。
pub(crate) fn revision_at(
    connection: &Connection,
    snapshot: &str,
    coordinate: &Coordinate,
    limit: usize,
) -> Result<Option<Revision>> {
    let repository = read_snapshot(connection, snapshot)?.repository_id;
    let mut current = Some(snapshot.to_owned());
    let mut visited = BTreeSet::new();
    while let Some(id) = current {
        ensure!(
            visited.len() < limit && visited.insert(id.clone()),
            "invalid snapshot lineage"
        );
        let snapshot = read_snapshot(connection, &id)?;
        ensure!(
            snapshot.repository_id == repository,
            crate::RepositoryFailure::Forbidden("snapshot belongs to a different repository")
        );
        let removed: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM snapshot_removals WHERE snapshot_id = ?1 AND name = ?2 AND version = ?3)", params![id, coordinate.name, coordinate.version], |row| row.get(0))?;
        if removed {
            return Ok(None);
        }
        let mut revision: Option<String> = connection.query_row("SELECT revision_id FROM snapshot_changes WHERE snapshot_id = ?1 AND name = ?2 AND version = ?3", params![id, coordinate.name, coordinate.version], |row| row.get(0)).optional()?;
        let checkpoint: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM snapshot_checkpoints WHERE snapshot_id = ?1)",
            [&id],
            |row| row.get(0),
        )?;
        if revision.is_none() && checkpoint {
            revision = connection.query_row("SELECT revision_id FROM checkpoint_entries WHERE snapshot_id = ?1 AND name = ?2 AND version = ?3", params![id, coordinate.name, coordinate.version], |row| row.get(0)).optional()?;
        }
        if let Some(id) = revision {
            let (json, metadata, actor): (String, String, String) = connection.query_row("SELECT entry_json, metadata_sha256, actor FROM revisions WHERE id = ?1 AND repository_id = ?2 AND name = ?3 AND version = ?4", params![id, repository, coordinate.name, coordinate.version], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            return Ok(Some(Revision {
                id,
                repository_id: repository,
                entry: serde_json::from_str(&json)?,
                metadata_sha256: metadata.parse()?,
                actor,
            }));
        }
        if checkpoint {
            return Ok(None);
        }
        current = snapshot.parent_id;
    }
    Ok(None)
}
