use anyhow::{ensure, Context, Result};
use naxus_core::{ArtifactDigest, Coordinate, IndexEntry, Snapshot};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    objects,
    repository::{insert_snapshot, materialize_checkpoint, read_snapshot},
    resolution::revision_at,
    Store,
};

/// 界面显式选择的 hosted 操作，不接受模糊名称批量修改。
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedAction {
    pub repository: String,
    pub name: String,
    pub version: Option<String>,
    #[serde(default)]
    pub all_versions: bool,
    pub operation: String,
    pub revision: Option<String>,
}

/// 已固定精确范围与 HEAD 的管理预览。
#[derive(Serialize, Deserialize)]
pub struct ManagementPreview {
    pub id: String,
    pub repository_id: String,
    pub expected_head: String,
    pub operation: String,
    pub coordinates: Vec<Coordinate>,
    pub revisions: Vec<String>,
    pub retained_history: bool,
    pub reclaimable_bytes: u64,
    pub fingerprint: ArtifactDigest,
    pub expires_at: i64,
}

/// 精确坐标从新到旧的一页修订及下一页的排他游标。
#[derive(Serialize)]
pub struct RevisionPage {
    pub items: Vec<serde_json::Value>,
    pub next: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct SelectedRevision {
    coordinate: Coordinate,
    revision: String,
    entry: IndexEntry,
    replacement: Option<String>,
}

impl Store {
    /// 业务作用：按明确仓库分页查询已知坐标及当前可见状态，历史修订独立保留。
    /// 参数说明：
    /// - `repository`：已授权仓库名称或 UUID。
    /// - `name`：可选精确 crate 名。
    /// - `after`：上一页最后一个名称与版本组成的游标。
    ///
    /// 返回：最多二百个坐标及当前索引；非法包名或超长游标按输入拒绝，查询不会修改 HEAD。
    pub fn catalog(
        &self,
        repository: &str,
        name: Option<&str>,
        after: &str,
    ) -> Result<serde_json::Value> {
        if let Some(name) = name {
            naxus_core::validate_name(name).context(crate::RepositoryFailure::InvalidRequest(
                "invalid catalog name",
            ))?;
        }
        ensure!(
            after.len() <= 512,
            crate::RepositoryFailure::InvalidRequest("catalog cursor exceeds limit")
        );
        let connection = self.connection()?;
        let (id, head): (String, String) = connection.query_row(
            "SELECT id, head FROM repositories WHERE id = ?1 OR name = ?1",
            [repository],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut statement = connection.prepare("SELECT DISTINCT name, version FROM revisions WHERE repository_id = ?1 AND (?2 IS NULL OR name = ?2) AND name || '@' || version > ?3 ORDER BY name || '@' || version LIMIT 201")?;
        let mut coordinates = statement
            .query_map(params![id, name, after], |row| {
                Ok(Coordinate {
                    name: row.get(0)?,
                    version: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let more = coordinates.len() > 200;
        coordinates.truncate(200);
        let next = if more {
            coordinates
                .last()
                .map(|coordinate| format!("{}@{}", coordinate.name, coordinate.version))
        } else {
            None
        };
        let mut items = Vec::new();
        for coordinate in coordinates {
            let current = revision_at(
                &connection,
                &head,
                &coordinate,
                self.options.max_snapshot_depth,
            )?;
            items.push(serde_json::json!({
                "name": coordinate.name,
                "version": coordinate.version,
                "current": current
            }));
        }
        Ok(serde_json::json!({
            "repository_id": id,
            "head": head,
            "items": items,
            "next": next
        }))
    }

    /// 业务作用：列出精确 hosted 坐标的修订历史，供界面选择回退目标。
    /// 参数说明：
    /// - `repository`：已经授权的仓库。
    /// - `name`：精确包名。
    /// - `version`：精确版本。
    ///
    /// 返回：最新二百条修订描述；遍历更早记录使用 `revision_history_page`。
    pub fn revision_history(
        &self,
        repository: &str,
        name: &str,
        version: &str,
    ) -> Result<Vec<serde_json::Value>> {
        Ok(self
            .revision_history_page(repository, name, version, 0)?
            .items)
    }

    /// 业务作用：从新到旧分页枚举精确坐标的完整历史，让长期覆盖产生的旧修订仍可选择管理。
    /// 参数说明：
    /// - `repository`：已经授权的仓库名称或 UUID。
    /// - `name`：精确包名。
    /// - `version`：精确版本。
    /// - `before`：上一页返回的排他游标，零表示从最新记录开始。
    ///
    /// 返回：最多二百条修订及可选下一页游标；非法包名、版本或负游标按输入拒绝，查询不改变 HEAD。
    pub fn revision_history_page(
        &self,
        repository: &str,
        name: &str,
        version: &str,
        before: i64,
    ) -> Result<RevisionPage> {
        naxus_core::validate_name(name).context(crate::RepositoryFailure::InvalidRequest(
            "invalid revision name",
        ))?;
        semver::Version::parse(version).context(crate::RepositoryFailure::InvalidRequest(
            "invalid revision version",
        ))?;
        ensure!(
            before >= 0,
            crate::RepositoryFailure::InvalidRequest("invalid revision history cursor")
        );
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT v.id, v.sha256, v.metadata_sha256, v.actor, v.entry_json, v.rowid
             FROM revisions v JOIN repositories r ON r.id = v.repository_id
             WHERE (r.name = ?1 OR r.id = ?1) AND v.name = ?2 AND v.version = ?3
               AND (?4 = 0 OR v.rowid < ?4)
             ORDER BY v.rowid DESC LIMIT 201",
        )?;
        let mut rows = statement
            .query_map(params![repository, name, version, before], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let more = rows.len() > 200;
        rows.truncate(200);
        // 以已返回的末条记录继续读取，新增修订不会挤占或重复更早页中的记录。
        let next = more.then(|| rows.last().expect("full revision page").5);
        let items = rows
            .into_iter()
            .map(|(id, sha256, metadata_sha256, actor, entry, _)| {
                Ok(serde_json::json!({
                    "id": id,
                    "sha256": sha256,
                    "metadata_sha256": metadata_sha256,
                    "actor": actor,
                    "entry": serde_json::from_str::<IndexEntry>(&entry)?
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(RevisionPage {
            items,
            next,
        })
    }

    /// 业务作用：在管理动作提交前固定影响范围、修订和 HEAD，不将模糊搜索结果作为操作对象。
    /// 参数说明：
    /// - `actor`：请求认证时固定的浏览器身份及账号代次。
    /// - `action`：精确坐标、操作及可选历史修订。
    ///
    /// 返回：五分钟预览；非法选择按输入拒绝，超出预览预算按配额拒绝；逻辑移除保留历史，不声称已释放物理字节。
    pub fn preview_hosted(
        &self,
        actor: &crate::AccessIdentity,
        action: &HostedAction,
    ) -> Result<ManagementPreview> {
        naxus_core::validate_name(&action.name).context(
            crate::RepositoryFailure::InvalidRequest("invalid hosted action name"),
        )?;
        ensure!(
            matches!(
                action.operation.as_str(),
                "remove" | "yank" | "unyank" | "rollback"
            ),
            crate::RepositoryFailure::InvalidRequest("unsupported hosted management action")
        );
        ensure!(
            action.all_versions != action.version.is_some(),
            crate::RepositoryFailure::InvalidRequest(
                "select one version or explicitly all versions"
            )
        );
        ensure!(
            (action.operation == "rollback") == action.revision.is_some()
                && (action.operation != "rollback" || !action.all_versions),
            crate::RepositoryFailure::InvalidRequest("invalid rollback target")
        );
        if let Some(version) = &action.version {
            semver::Version::parse(version).context(crate::RepositoryFailure::InvalidRequest(
                "invalid hosted action version",
            ))?;
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actor = crate::security::browser_actor(&transaction, actor)?;
        let (repository_id, head): (String, String) = transaction.query_row(
            "SELECT id, head FROM repositories WHERE name = ?1 OR id = ?1",
            [&action.repository],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        management_permission(&transaction, actor, &repository_id)?;
        let versions = if let Some(version) = &action.version {
            vec![version.clone()]
        } else {
            let mut statement = transaction.prepare("SELECT DISTINCT version FROM revisions WHERE repository_id = ?1 AND name = ?2 LIMIT 4097")?;
            let versions = statement
                .query_map(params![repository_id, action.name], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ensure!(
                versions.len() <= 4096,
                crate::ResourceLimitExceeded("management selection exceeds limit")
            );
            versions
        };
        let mut selected = Vec::new();
        for version in versions {
            let coordinate = Coordinate {
                name: action.name.clone(),
                version,
            };
            let mut current = revision_at(
                &transaction,
                &head,
                &coordinate,
                self.options.max_snapshot_depth,
            )?;
            if current.is_none() {
                if let Some(revision) = &action.revision {
                    let (json, metadata, publisher): (String, String, String) = transaction.query_row("SELECT entry_json, metadata_sha256, actor FROM revisions WHERE id = ?1 AND repository_id = ?2 AND name = ?3 AND version = ?4", params![revision, repository_id, coordinate.name, coordinate.version], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
                    // 逻辑移除后仍可显式选择保留的历史作为回退目标；提交时继续复验撤销状态和字节。
                    current = Some(naxus_core::Revision {
                        id: revision.clone(),
                        repository_id: repository_id.clone(),
                        entry: serde_json::from_str(&json)?,
                        metadata_sha256: metadata.parse()?,
                        actor: publisher,
                    });
                }
            }
            if let Some(current) = current {
                let replacement = if let Some(revision) = &action.revision {
                    let valid: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM revisions WHERE id = ?1 AND repository_id = ?2 AND name = ?3 AND version = ?4)", params![revision, repository_id, coordinate.name, coordinate.version], |row| row.get(0))?;
                    // 回退只能引用当前坐标的历史，不能把另一包或版本的修订发布为目标内容。
                    ensure!(
                        valid,
                        crate::RepositoryFailure::InvalidRequest(
                            "rollback revision belongs to another coordinate"
                        )
                    );
                    Some(revision.clone())
                } else {
                    None
                };
                selected.push(SelectedRevision {
                    coordinate,
                    revision: current.id,
                    entry: current.entry,
                    replacement,
                });
            }
        }
        ensure!(
            !selected.is_empty(),
            crate::RepositoryFailure::NotFound("unknown visible package")
        );
        selected.sort_by(|left, right| left.coordinate.version.cmp(&right.coordinate.version));
        let fingerprint = ArtifactDigest::of(&serde_json::to_vec(&(
            &repository_id,
            &head,
            &action.operation,
            &selected,
        ))?);
        transaction.execute("DELETE FROM management_previews WHERE id IN (SELECT id FROM management_previews WHERE state = 'prepared' AND expires_at <= unixepoch() LIMIT 256)", [])?;
        let active: u64 = transaction.query_row(
            "SELECT count(*) FROM management_previews WHERE state = 'prepared' AND actor = ?1",
            [actor],
            |row| row.get(0),
        )?;
        ensure!(
            active < 128,
            crate::ResourceLimitExceeded("management preview budget exceeded")
        );
        let preview = ManagementPreview {
            id: Uuid::new_v4().to_string(),
            repository_id,
            expected_head: head,
            operation: action.operation.clone(),
            coordinates: selected
                .iter()
                .map(|selected| selected.coordinate.clone())
                .collect(),
            revisions: selected
                .iter()
                .map(|selected| selected.revision.clone())
                .collect(),
            retained_history: true,
            reclaimable_bytes: 0,
            fingerprint,
            expires_at: transaction.query_row("SELECT unixepoch() + 300", [], |row| row.get(0))?,
        };
        transaction.execute("INSERT INTO management_previews(id, actor, repository_id, expected_head, operation, selection, fingerprint, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)", params![preview.id, actor, preview.repository_id, preview.expected_head, preview.operation, serde_json::to_string(&selected)?, preview.fingerprint.as_str(), preview.expires_at])?;
        transaction.commit()?;
        Ok(preview)
    }

    /// 业务作用：复验预览权限与 HEAD 后原子发布管理快照，历史索引不被原地改写。
    /// 参数说明：
    /// - `actor`：请求认证时固定的浏览器身份及账号代次。
    /// - `id`：先前确认的影响预览。
    /// - `fingerprint`：界面显示并确认的精确范围摘要。
    /// - `key`：客户端固定的幂等键。
    ///
    /// 返回：完整新快照；权限、HEAD 或预览变化时拒绝，重复提交返回同一结果。
    pub fn commit_hosted_management(
        &self,
        actor: &crate::AccessIdentity,
        id: &str,
        fingerprint: &ArtifactDigest,
        key: &str,
    ) -> Result<Snapshot> {
        ensure!(
            !key.is_empty() && key.len() <= 256,
            crate::RepositoryFailure::InvalidRequest("invalid management idempotency key")
        );
        let _objects = self
            .object_gate
            .lock()
            .map_err(|_| anyhow::anyhow!("object lifecycle unavailable"))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actor = crate::security::browser_actor(&transaction, actor)?;
        let (repository, expected, operation, selection, digest, active, state, previous_key, committed): (String, String, String, String, String, bool, String, Option<String>, Option<String>) = transaction.query_row("SELECT repository_id, expected_head, operation, selection, fingerprint, expires_at > unixepoch(), state, commit_key, committed_snapshot FROM management_previews WHERE id = ?1 AND actor = ?2", params![id, actor], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?)))?;
        management_permission(&transaction, actor, &repository)?;
        ensure!(
            digest == fingerprint.as_str(),
            crate::RepositoryFailure::Conflict("management preview fingerprint changed".to_owned())
        );
        if state == "committed" {
            ensure!(
                previous_key.as_deref() == Some(key),
                crate::RepositoryFailure::Conflict("management idempotency key changed".to_owned())
            );
            return read_snapshot(
                &transaction,
                &committed.context("committed management snapshot missing")?,
            );
        }
        ensure!(
            active && state == "prepared",
            crate::RepositoryFailure::Conflict("management preview expired".to_owned())
        );
        let head: String = transaction.query_row(
            "SELECT head FROM repositories WHERE id = ?1",
            [&repository],
            |row| row.get(0),
        )?;
        // 预览之后的任何发布都使当前影响范围失去权威，必须重新预览而不是覆盖新 HEAD。
        ensure!(
            head == expected,
            crate::RepositoryFailure::Conflict(
                "repository HEAD changed since management preview".to_owned()
            )
        );
        let selected: Vec<SelectedRevision> = serde_json::from_str(&selection)?;
        let parent = read_snapshot(&transaction, &head)?;
        let snapshot = Snapshot {
            id: Uuid::new_v4().to_string(),
            repository_id: repository.clone(),
            parent_id: Some(head.clone()),
            manifest_sha256: ArtifactDigest::of(&serde_json::to_vec(&(
                &repository,
                &parent.manifest_sha256,
                &operation,
                fingerprint,
            ))?),
        };
        insert_snapshot(&transaction, &snapshot)?;
        for selected in selected {
            let name = selected.coordinate.name;
            let version = selected.coordinate.version;
            if operation == "remove" {
                transaction.execute(
                    "INSERT INTO snapshot_removals(snapshot_id, name, version) VALUES (?1, ?2, ?3)",
                    params![snapshot.id, name, version],
                )?;
                continue;
            }
            let revision = if let Some(replacement) = selected.replacement {
                let revoked: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM artifact_access_revocations WHERE revision_id = ?1)", [&replacement], |row| row.get(0))?;
                if revoked {
                    return Err(crate::ArtifactGone.into());
                }
                let digest: String = transaction.query_row("SELECT sha256 FROM revisions WHERE id = ?1 AND repository_id = ?2 AND name = ?3 AND version = ?4", params![replacement, repository, name, version], |row| row.get(0))?;
                objects::verify_object(&self.root, &digest.parse()?, None)?;
                replacement
            } else {
                let mut entry = selected.entry;
                entry.yanked = operation == "yank";
                entry.validate()?;
                objects::verify_object(&self.root, &entry.cksum, None)?;
                let revision = Uuid::new_v4().to_string();
                transaction.execute("INSERT INTO revisions(id, repository_id, name, version, sha256, metadata_sha256, entry_json, actor) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)", params![revision, repository, name, version, entry.cksum.as_str(), entry.metadata_digest()?.as_str(), serde_json::to_string(&entry)?, actor])?;
                revision
            };
            transaction.execute("INSERT INTO snapshot_changes(snapshot_id, name, version, revision_id) VALUES (?1, ?2, ?3, ?4)", params![snapshot.id, name, version, revision])?;
        }
        materialize_checkpoint(&transaction, &snapshot.id, self.options.max_snapshot_depth)?;
        // 修改、审计和预览终态先进入事务，最后发布 HEAD，读者只能观察完整结果。
        transaction.execute(
            "INSERT INTO audit(actor, action, repository_id, snapshot_id) VALUES (?1, ?2, ?3, ?4)",
            params![actor, operation, repository, snapshot.id],
        )?;
        transaction.execute(
            "INSERT INTO security_audit(actor, action, target) VALUES (?1, ?2, ?3)",
            params![actor, operation, id],
        )?;
        transaction.execute("UPDATE management_previews SET state = 'committed', commit_key = ?1, committed_snapshot = ?2 WHERE id = ?3", params![key, snapshot.id, id])?;
        transaction.execute(
            "UPDATE repositories SET head = ?1 WHERE id = ?2 AND head = ?3",
            params![snapshot.id, repository, expected],
        )?;
        transaction.commit()?;
        Ok(snapshot)
    }
}

/// 业务作用：在管理事务内核对当前账户与目标仓库，避免预览权限被撤销后仍可提交。
/// 参数说明：
/// - `connection`：本次管理事务。
/// - `actor`：浏览器主体 UUID。
/// - `repository`：目标仓库 UUID。
///
/// 返回：当前账号仍为运维或管理员且拥有目标范围才成功。
pub(crate) fn management_permission(
    connection: &Connection,
    actor: &str,
    repository: &str,
) -> Result<()> {
    let grants: Option<String> = connection.query_row("SELECT repositories FROM admin_users WHERE id = ?1 AND role IN ('admin', 'operator') AND enabled = 1 AND must_change = 0", [actor], |row| row.get(0)).optional()?;
    let grants: Vec<String> = serde_json::from_str(&grants.context(
        crate::RepositoryFailure::Forbidden("management permission denied"),
    )?)?;
    ensure!(
        grants
            .iter()
            .any(|grant| grant == "*" || grant == repository),
        crate::RepositoryFailure::Forbidden("repository management permission denied")
    );
    Ok(())
}
