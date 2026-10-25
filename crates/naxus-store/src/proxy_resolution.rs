use anyhow::{ensure, Context, Result};
use naxus_core::{ArtifactDigest, IndexEntry};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Store;

/// 一次公开来源解析期间的固定索引视图。
#[derive(Clone, Serialize, Deserialize)]
pub struct ProxySession {
    pub id: String,
    pub proxy: String,
    pub proxy_id: String,
    pub mode: String,
    pub expires_at: i64,
    pub upstream_start: usize,
}

/// 只属于一个上游地址的条件请求证据。
pub struct UpstreamValidation {
    pub etag: Option<String>,
    pub modified: Option<String>,
    pub body: Vec<u8>,
}

/// Cargo 实际解析图中的公开制品身份。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializationItem {
    pub name: String,
    pub version: String,
    pub digest: ArtifactDigest,
}

/// 全部公开对象已持久化的集合证据。
#[derive(Serialize, Deserialize)]
pub struct Materialization {
    pub id: String,
    pub proxy_id: String,
    pub fingerprint: ArtifactDigest,
}

impl Store {
    /// 业务作用：核对工作区记录的公开物化集合仍可用于锁定构建。
    /// 参数说明：
    /// - `id`：锁文件记录的物化身份。
    ///
    /// 返回：公开集合未被主动撤销时为 true，团队可共享已提交的锁文件证据。
    pub fn materialization_valid(&self, id: &str) -> Result<bool> {
        Ok(self.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM proxy_materializations WHERE id = ?1 AND valid = 1)",
            [id],
            |row| row.get(0),
        )?)
    }
    /// 业务作用：为客户端解析创建有界短期视图，固定条目而非修改 Cargo 来源身份。
    /// 参数说明：
    /// - `proxy`：公开来源名称。
    /// - `actor`：已认证主体。
    /// - `mode`：cache-first 或 revalidate。
    ///
    /// 返回：一小时 session；超过主体或实例活动预算拒绝。
    pub fn begin_proxy_session(
        &self,
        proxy: &str,
        actor: &str,
        mode: &str,
    ) -> Result<ProxySession> {
        self.begin_proxy_view(proxy, actor, mode, 0)
    }

    /// 业务作用：为完整图重试创建新的固定视图，从指定镜像位置继续按顺序读取原始索引。
    /// 参数说明：
    /// - `proxy`：独立公开来源名称。
    /// - `actor`：已认证主体。
    /// - `mode`：cache-first 或 revalidate。
    /// - `upstream_start`：本次视图采用的首个镜像位置。
    ///
    /// 返回：独立的一小时 session，不改变任何既有 session 的固定条目。
    pub fn begin_proxy_view(
        &self,
        proxy: &str,
        actor: &str,
        mode: &str,
        upstream_start: usize,
    ) -> Result<ProxySession> {
        ensure!(
            upstream_start < 16 && !actor.is_empty() && actor.len() <= 256,
            crate::RepositoryFailure::InvalidRequest("invalid proxy view")
        );
        ensure!(
            matches!(mode, "cache-first" | "revalidate"),
            crate::RepositoryFailure::InvalidRequest("invalid proxy session mode")
        );
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute("DELETE FROM proxy_sessions WHERE id IN (SELECT id FROM proxy_sessions WHERE expires_at <= unixepoch() LIMIT 256)", [])?;
        let (total, owned): (u64, u64) = transaction.query_row(
            "SELECT count(*), coalesce(sum(actor = ?1), 0) FROM proxy_sessions",
            [actor],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        ensure!(
            total < 4096 && owned < 256,
            crate::ResourceLimitExceeded("proxy session budget exceeded")
        );
        let proxy_id: String = transaction.query_row(
            "SELECT id FROM proxy_repositories WHERE name = ?1",
            [proxy],
            |row| row.get(0),
        )?;
        let expires_at =
            transaction.query_row("SELECT unixepoch() + 3600", [], |row| row.get(0))?;
        let session = ProxySession {
            id: Uuid::new_v4().to_string(),
            proxy: proxy.to_owned(),
            proxy_id,
            mode: mode.to_owned(),
            expires_at,
            upstream_start,
        };
        transaction.execute("INSERT INTO proxy_sessions(id, proxy_id, actor, mode, expires_at, upstream_start) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![session.id, session.proxy_id, actor, session.mode, session.expires_at, upstream_start])?;
        transaction.commit()?;
        Ok(session)
    }

    /// 业务作用：核对 session 的主体、来源和期限，禁止猜测 UUID 后复用他人解析视图。
    /// 参数说明：
    /// - `proxy`：请求路由的来源。
    /// - `id`：session UUID。
    /// - `actor`：当前主体。
    ///
    /// 返回：仍有效且归属一致的 session；不匹配按不存在拒绝。
    pub fn proxy_session(&self, proxy: &str, id: &str, actor: &str) -> Result<ProxySession> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT s.id, p.name, p.id, s.mode, s.expires_at, s.upstream_start
             FROM proxy_sessions s JOIN proxy_repositories p ON p.id = s.proxy_id
             WHERE s.id = ?1 AND p.name = ?2 AND s.actor = ?3 AND s.expires_at > unixepoch()",
                params![id, proxy, actor],
                |row| {
                    Ok(ProxySession {
                        id: row.get(0)?,
                        proxy: row.get(1)?,
                        proxy_id: row.get(2)?,
                        mode: row.get(3)?,
                        expires_at: row.get(4)?,
                        upstream_start: row.get(5)?,
                    })
                },
            )
            .optional()?
            .context(crate::RepositoryFailure::NotFound(
                "proxy session is unavailable",
            ))
    }

    /// 业务作用：读取本次解析已经固定的原始索引，不受后续刷新影响。
    /// 参数说明：
    /// - `session`：已授权 session。
    /// - `name`：crate 名称。
    ///
    /// 返回：固定的原始响应或尚未请求的 None。
    pub fn proxy_session_entry(
        &self,
        session: &ProxySession,
        name: &str,
    ) -> Result<Option<Vec<u8>>> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT body FROM proxy_session_entries WHERE session_id = ?1 AND name = ?2",
                params![session.id, name],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// 业务作用：以首次返回的索引固定本次视图，竞争读取只接受已先提交的同一内容。
    /// 参数说明：
    /// - `session`：已授权 session。
    /// - `name`：crate 名称。
    /// - `body`：已经被代理验证的原始索引。
    ///
    /// 返回：本次 session 实际固定的内容，不覆盖已固定条目。
    pub fn freeze_proxy_entry(
        &self,
        session: &ProxySession,
        name: &str,
        body: &[u8],
    ) -> Result<Vec<u8>> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM proxy_sessions WHERE id = ?1 AND expires_at > unixepoch())", [&session.id], |row| row.get(0))?;
        ensure!(
            active,
            crate::RepositoryFailure::NotFound("proxy session is unavailable")
        );
        let existing: Option<Vec<u8>> = transaction
            .query_row(
                "SELECT body FROM proxy_session_entries WHERE session_id = ?1 AND name = ?2",
                params![session.id, name],
                |row| row.get(0),
            )
            .optional()?;
        // 已固定条目的重读不申请新容量，也不能改写先前选择；过期检查仍必须先完成。
        if let Some(existing) = existing {
            return Ok(existing);
        }
        ensure!(
            fs2::available_space(&self.root)?
                >= self
                    .options
                    .reserve_free_bytes
                    .saturating_add((body.len() as u64).saturating_mul(8)),
            crate::RepositoryFailure::InsufficientStorage
        );
        let (count, bytes): (u64, u64) = transaction.query_row("SELECT count(*), coalesce(sum(length(body)), 0) FROM proxy_session_entries WHERE session_id = ?1", [&session.id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        ensure!(
            count < 4096 && bytes.saturating_add(body.len() as u64) <= 512 * 1024 * 1024,
            crate::ResourceLimitExceeded("proxy session index budget exceeded")
        );
        transaction.execute("INSERT OR IGNORE INTO proxy_session_entries(session_id, name, body) VALUES (?1, ?2, ?3)", params![session.id, name, body])?;
        let fixed: Vec<u8> = transaction.query_row(
            "SELECT body FROM proxy_session_entries WHERE session_id = ?1 AND name = ?2",
            params![session.id, name],
            |row| row.get(0),
        )?;
        for line in fixed
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let entry: IndexEntry = serde_json::from_slice(line)?;
            transaction.execute("INSERT OR IGNORE INTO proxy_session_objects(session_id, name, digest) VALUES (?1, ?2, ?3)", params![session.id, name, entry.cksum.as_str()])?;
        }
        transaction.commit()?;
        Ok(fixed)
    }

    /// 业务作用：结束当前命令不再需要的解析视图，长期物化证据独立保留。
    /// 参数说明：
    /// - `session`：当前主体已授权的 session。
    ///
    /// 返回：删除短期固定索引，不删除制品对象。
    pub fn finish_proxy_session(&self, session: &ProxySession) -> Result<()> {
        self.connection()?
            .execute("DELETE FROM proxy_sessions WHERE id = ?1", [&session.id])?;
        Ok(())
    }

    /// 业务作用：读取特定镜像的条件请求证据，避免把另一上游的 ETag 或正文用于 304。
    /// 参数说明：
    /// - `proxy`：逻辑来源。
    /// - `name`：crate 名称。
    /// - `upstream`：精确镜像索引地址。
    ///
    /// 返回：该镜像已有的校验标记及原始正文。
    pub fn upstream_validation(
        &self,
        proxy: &str,
        name: &str,
        upstream: &str,
    ) -> Result<Option<UpstreamValidation>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT etag, modified, body FROM proxy_validators
             WHERE proxy_id = (SELECT id FROM proxy_repositories WHERE name = ?1)
               AND name = ?2 AND upstream = ?3",
                params![proxy, name, upstream],
                |row| {
                    Ok(UpstreamValidation {
                        etag: row.get(0)?,
                        modified: row.get(1)?,
                        body: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// 业务作用：仅在索引通过校验后保存同一镜像的条件请求标记。
    /// 参数说明：
    /// - `proxy`：来源名称。
    /// - `name`：crate 名称。
    /// - `upstream`：精确镜像地址。
    /// - `validation`：有界标记与已验证正文。
    ///
    /// 返回：标记被持久化；不会跨镜像复用。
    pub fn save_upstream_validation(
        &self,
        proxy: &str,
        name: &str,
        upstream: &str,
        validation: &UpstreamValidation,
    ) -> Result<()> {
        ensure!(
            validation
                .etag
                .as_ref()
                .is_none_or(|value| value.len() <= 4096)
                && validation
                    .modified
                    .as_ref()
                    .is_none_or(|value| value.len() <= 4096),
            "upstream validator exceeds limit"
        );
        self.connection()?.execute("INSERT INTO proxy_validators(proxy_id, name, upstream, etag, modified, body) SELECT id, ?2, ?3, ?4, ?5, ?6 FROM proxy_repositories WHERE name = ?1 ON CONFLICT(proxy_id, name, upstream) DO UPDATE SET etag = excluded.etag, modified = excluded.modified, body = excluded.body", params![proxy, name, upstream, validation.etag, validation.modified, validation.body])?;
        Ok(())
    }

    /// 业务作用：检查全部上游明确缺失后建立的短期负缓存，不把网络失败作为不存在。
    /// 参数说明：
    /// - `proxy`：来源名称。
    /// - `name`：crate 名称。
    ///
    /// 返回：仍有效的明确缺失证据是否存在。
    pub fn proxy_missing(&self, proxy: &str, name: &str) -> Result<bool> {
        Ok(self.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM proxy_missing WHERE proxy_id = (SELECT id FROM proxy_repositories WHERE name = ?1) AND name = ?2 AND expires_at > unixepoch())", params![proxy, name], |row| row.get(0))?)
    }

    /// 业务作用：短期记住全部镜像的 404，限制重复错误名称造成的回源放大。
    /// 参数说明：
    /// - `proxy`：来源名称。
    /// - `name`：全部镜像明确缺失的名称。
    ///
    /// 返回：三十秒负缓存；不构成永久封禁。
    pub fn save_proxy_missing(&self, proxy: &str, name: &str) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute("DELETE FROM proxy_missing WHERE rowid IN (SELECT rowid FROM proxy_missing WHERE expires_at <= unixepoch() LIMIT 256)", [])?;
        let count: u64 =
            transaction.query_row("SELECT count(*) FROM proxy_missing", [], |row| row.get(0))?;
        if count < 8192 {
            transaction.execute("INSERT INTO proxy_missing(proxy_id, name, expires_at) SELECT id, ?2, unixepoch() + 30 FROM proxy_repositories WHERE name = ?1 ON CONFLICT(proxy_id, name) DO UPDATE SET expires_at = excluded.expires_at", params![proxy, name])?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// 业务作用：证明 Cargo 解析图的每个公开对象已本地保存，并建立可审计的完整物化集合。
    /// 参数说明：
    /// - `session`：已固定索引的解析视图。
    /// - `actor`：当前主体。
    /// - `items`：Cargo.lock 中的精确公开坐标与摘要。
    ///
    /// 返回：索引证据、对象引用及摘要全部一致后返回集合身份；重复图复用原身份。
    pub fn commit_materialization(
        &self,
        session: &ProxySession,
        actor: &str,
        items: &[MaterializationItem],
    ) -> Result<Materialization> {
        ensure!(
            items.len() <= 4096,
            crate::RepositoryFailure::InvalidRequest("materialization exceeds limit")
        );
        for item in items {
            naxus_core::validate_name(&item.name).map_err(|_| {
                crate::RepositoryFailure::InvalidRequest("invalid materialization name")
            })?;
            semver::Version::parse(&item.version).map_err(|_| {
                crate::RepositoryFailure::InvalidRequest("invalid materialization version")
            })?;
        }
        let mut items = items.to_vec();
        items
            .sort_by(|left, right| (&left.name, &left.version).cmp(&(&right.name, &right.version)));
        ensure!(
            items
                .windows(2)
                .all(|pair| (&pair[0].name, &pair[0].version) != (&pair[1].name, &pair[1].version)),
            crate::RepositoryFailure::InvalidRequest("duplicate materialization coordinate")
        );
        let fingerprint = ArtifactDigest::of(&serde_json::to_vec(&items)?);
        let _objects = self
            .object_gate
            .lock()
            .map_err(|_| anyhow::anyhow!("object lifecycle unavailable"))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM proxy_sessions WHERE id = ?1 AND actor = ?2 AND expires_at > unixepoch())", params![session.id, actor], |row| row.get(0))?;
        ensure!(
            active,
            crate::RepositoryFailure::NotFound("unknown proxy session")
        );
        for item in &items {
            let body: Vec<u8> = transaction.query_row(
                "SELECT body FROM proxy_session_entries WHERE session_id = ?1 AND name = ?2",
                params![session.id, item.name],
                |row| row.get(0),
            )?;
            let proven = body
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(serde_json::from_slice::<IndexEntry>)
                .collect::<serde_json::Result<Vec<_>>>()?
                .into_iter()
                .any(|entry| {
                    entry.name == item.name
                        && entry.vers == item.version
                        && entry.cksum == item.digest
                });
            let stored: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM proxy_objects WHERE proxy_id = ?1 AND digest = ?2)",
                params![session.proxy_id, item.digest.as_str()],
                |row| row.get(0),
            )?;
            // 索引证据与完整对象缺一不可，不能仅凭客户端提交的摘要建立保留引用。
            ensure!(
                proven && stored,
                "public object is not completely materialized"
            );
        }
        let existing: Option<String> = transaction.query_row("SELECT id FROM proxy_materializations WHERE proxy_id = ?1 AND actor = ?2 AND fingerprint = ?3 AND valid = 1", params![session.proxy_id, actor, fingerprint.as_str()], |row| row.get(0)).optional()?;
        let id = existing.unwrap_or_else(|| Uuid::new_v4().to_string());
        transaction.execute("INSERT OR IGNORE INTO proxy_materializations(id, proxy_id, actor, fingerprint) VALUES (?1, ?2, ?3, ?4)", params![id, session.proxy_id, actor, fingerprint.as_str()])?;
        for item in items {
            transaction.execute("INSERT OR IGNORE INTO materialization_items(materialization_id, name, version, digest) VALUES (?1, ?2, ?3, ?4)", params![id, item.name, item.version, item.digest.as_str()])?;
        }
        transaction.commit()?;
        Ok(Materialization {
            id,
            proxy_id: session.proxy_id.clone(),
            fingerprint,
        })
    }
}
