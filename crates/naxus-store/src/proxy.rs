use std::{collections::BTreeSet, io::Read};

use anyhow::{ensure, Result};
use naxus_core::{ArtifactDigest, IndexEntry};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Store;

/// 公开来源具有独立持久身份，同源镜像不能改变其逻辑来源。
#[derive(Clone, Serialize, Deserialize)]
pub struct ProxyInfo {
    pub id: String,
    pub name: String,
    pub logical_index: String,
}

/// 上游原始索引与单独的刷新时间，包体缓存不依赖索引 TTL。
pub struct ProxyIndex {
    pub body: Vec<u8>,
    pub checked_at: i64,
}

impl Store {
    /// 业务作用：分页展示某一公开来源已见过的坐标及本地包体状态，不混入 hosted 包。
    /// 参数说明：
    /// - `proxy`：公开代理名称或 UUID。
    /// - `after`：上一页最后一个名称与版本游标。
    ///
    /// 返回：最多二百个公开坐标、摘要与是否已缓存的事实；超长游标按输入拒绝。
    pub fn proxy_catalog(&self, proxy: &str, after: &str) -> Result<serde_json::Value> {
        ensure!(
            after.len() <= 512,
            crate::RepositoryFailure::InvalidRequest("proxy catalog cursor exceeds limit")
        );
        let connection = self.connection()?;
        let id: String = connection.query_row(
            "SELECT id FROM proxy_repositories WHERE id = ?1 OR name = ?1",
            [proxy],
            |row| row.get(0),
        )?;
        let mut statement = connection.prepare("SELECT c.name, c.version, c.digest, p.digest IS NOT NULL AND o.state = 'present' FROM proxy_coordinates c LEFT JOIN proxy_objects p ON p.proxy_id = c.proxy_id AND p.digest = c.digest LEFT JOIN objects o ON o.sha256 = c.digest WHERE c.proxy_id = ?1 AND c.name || '@' || c.version > ?2 ORDER BY c.name || '@' || c.version LIMIT 201")?;
        let mut rows = statement
            .query_map(params![id, after], |row| {
                Ok(serde_json::json!({
                    "name": row.get::<_, String>(0)?,
                    "version": row.get::<_, String>(1)?,
                    "digest": row.get::<_, String>(2)?,
                    "cached": row.get::<_, bool>(3)?
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let more = rows.len() > 200;
        rows.truncate(200);
        let next = if more {
            rows.last().map(|item| {
                format!(
                    "{}@{}",
                    item["name"].as_str().unwrap_or_default(),
                    item["version"].as_str().unwrap_or_default()
                )
            })
        } else {
            None
        };
        Ok(serde_json::json!({
            "proxy_id": id,
            "items": rows,
            "next": next
        }))
    }
    /// 业务作用：建立公开代理的稳定身份，禁止同名代理被悄悄换成另一来源。
    /// 参数说明：
    /// - `name`：配置中的代理名。
    /// - `logical_index`：Cargo 原有 registry 身份。
    ///
    /// 返回：新建或相同来源已有代理；等价声明保留原始 URL 与 UUID，来源身份改变时拒绝。
    pub fn create_proxy(&self, name: &str, logical_index: &str) -> Result<ProxyInfo> {
        naxus_core::validate_name(name)?;
        let identity = naxus_core::registry_identity(logical_index)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT id, name, logical_index FROM proxy_repositories WHERE name = ?1",
                [name],
                |row| {
                    Ok(ProxyInfo {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        logical_index: row.get(2)?,
                    })
                },
            )
            .optional()?;
        if let Some(existing) = existing {
            // 持久身份已被锁文件引用，配置变化不能把旧锁重新解释为其他 registry。
            ensure!(
                naxus_core::registry_identity(&existing.logical_index)? == identity,
                "proxy logical source changed"
            );
            return Ok(existing);
        }
        let proxy = ProxyInfo {
            id: Uuid::new_v4().to_string(),
            name: name.to_owned(),
            logical_index: logical_index.to_owned(),
        };
        transaction.execute(
            "INSERT INTO proxy_repositories(id, name, logical_index) VALUES (?1, ?2, ?3)",
            params![proxy.id, proxy.name, proxy.logical_index],
        )?;
        transaction.commit()?;
        Ok(proxy)
    }

    /// 业务作用：列出公开代理映射供单入口客户端保留来源身份。
    /// 参数说明: 无。
    ///
    /// 返回：按名称排序的稳定身份，不包含上游地址和凭据。
    pub fn proxies(&self) -> Result<Vec<ProxyInfo>> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT id, name, logical_index FROM proxy_repositories ORDER BY name")?;
        let proxies = statement
            .query_map([], |row| {
                Ok(ProxyInfo {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    logical_index: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(proxies)
    }

    /// 业务作用：读取代理的持久索引缓存，不以失效 TTL 丢弃可用于离线读取的内容。
    /// 参数说明：
    /// - `proxy`：代理名称。
    /// - `name`：规范 crate 名。
    ///
    /// 返回：已有原始 JSONL 及刷新时间，未保存返回 None。
    pub fn proxy_index(&self, proxy: &str, name: &str) -> Result<Option<ProxyIndex>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT body, checked_at FROM proxy_indexes
             WHERE proxy_id = (SELECT id FROM proxy_repositories WHERE name = ?1) AND name = ?2",
                params![proxy, name],
                |row| {
                    Ok(ProxyIndex {
                        body: row.get(0)?,
                        checked_at: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    /// 业务作用：原子保存上游原始索引与坐标摘要，拒绝同源镜像提供互相矛盾的制品。
    /// 参数说明：
    /// - `proxy`：已建立的来源名称。
    /// - `name`：sparse 路径对应名称。
    /// - `body`：经过长度门禁的原始 JSONL。
    ///
    /// 返回：所有行合法且历史同版本摘要不变才提交；失败保留原索引。
    pub fn save_proxy_index(&self, proxy: &str, name: &str, body: &[u8]) -> Result<()> {
        ensure!(
            body.len() <= 32 * 1024 * 1024 && !body.is_empty(),
            "invalid proxy index size"
        );
        ensure!(
            fs2::available_space(&self.root)?
                >= self
                    .options
                    .reserve_free_bytes
                    .saturating_add((body.len() as u64).saturating_mul(8)),
            crate::RepositoryFailure::InsufficientStorage
        );
        let mut entries = Vec::new();
        let mut versions = BTreeSet::new();
        for line in body
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            ensure!(
                line.len() <= 2 * 1024 * 1024,
                "proxy index line exceeds limit"
            );
            let entry: IndexEntry = serde_json::from_slice(line)?;
            ensure!(
                entry.name.eq_ignore_ascii_case(name) && versions.insert(entry.vers.clone()),
                "invalid proxy index coordinate"
            );
            entry.validate_upstream()?;
            entries.push(entry);
        }
        ensure!(!entries.is_empty(), "empty proxy index");
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id: String = transaction.query_row(
            "SELECT id FROM proxy_repositories WHERE name = ?1",
            [proxy],
            |row| row.get(0),
        )?;
        for entry in entries {
            let old: Option<String> = transaction.query_row("SELECT digest FROM proxy_coordinates WHERE proxy_id = ?1 AND name = ?2 AND version = ?3", params![id, name, entry.vers], |row| row.get(0)).optional()?;
            // 公开镜像承诺同版本原始字节不变；不能把上游矛盾当作可覆盖的私有修订。
            ensure!(
                old.as_deref()
                    .is_none_or(|digest| digest == entry.cksum.as_str()),
                "upstream checksum conflict"
            );
            transaction.execute("INSERT OR IGNORE INTO proxy_coordinates(proxy_id, name, version, digest) VALUES (?1, ?2, ?3, ?4)", params![id, name, entry.vers, entry.cksum.as_str()])?;
        }
        transaction.execute("INSERT INTO proxy_indexes(proxy_id, name, body, checked_at) VALUES (?1, ?2, ?3, unixepoch()) ON CONFLICT(proxy_id, name) DO UPDATE SET body = excluded.body, checked_at = excluded.checked_at", params![id, name, body])?;
        transaction.commit()?;
        Ok(())
    }

    /// 业务作用：从受验证的来源索引读取精确版本摘要，不由调用者指定任意下载对象。
    /// 参数说明：
    /// - `proxy`：公开代理名称。
    /// - `name`：crate 名称。
    /// - `version`：精确版本。
    ///
    /// 返回：历史索引已证明的摘要；从未见过的版本返回 None。
    pub fn proxy_digest(
        &self,
        proxy: &str,
        name: &str,
        version: &str,
    ) -> Result<Option<ArtifactDigest>> {
        let digest: Option<String> = self.connection()?.query_row("SELECT digest FROM proxy_coordinates WHERE proxy_id = (SELECT id FROM proxy_repositories WHERE name = ?1) AND name = ?2 AND version = ?3", params![proxy, name, version], |row| row.get(0)).optional()?;
        digest.map(|digest| digest.parse()).transpose()
    }

    /// 业务作用：检测某公开来源已完整保存的对象，避免跨来源未授权引用暂存内容。
    /// 参数说明：
    /// - `proxy`：公开代理名称。
    /// - `digest`：索引已证明的精确摘要。
    ///
    /// 返回：已持久登记的对象关联是否存在。
    pub fn proxy_has_object(&self, proxy: &str, digest: &ArtifactDigest) -> Result<bool> {
        Ok(self.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM proxy_objects WHERE proxy_id = (SELECT id FROM proxy_repositories WHERE name = ?1) AND digest = ?2)", params![proxy, digest.as_str()], |row| row.get(0))?)
    }

    /// 业务作用：下载并校验完整包体后登记来源引用，失败不产生可下载的代理对象。
    /// 参数说明：
    /// - `proxy`：公开代理名称。
    /// - `reader`：上游原始包体流。
    /// - `digest`：索引提供的预期摘要。
    ///
    /// 返回：原始对象已持久化并关联来源；网络、容量或摘要错误返回失败。
    pub fn install_proxy_object(
        &self,
        proxy: &str,
        reader: &mut dyn Read,
        digest: &ArtifactDigest,
    ) -> Result<()> {
        let object = self.install_object(reader, digest)?;
        let _objects = self
            .object_gate
            .lock()
            .map_err(|_| anyhow::anyhow!("object lifecycle unavailable"))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state: String = transaction.query_row(
            "SELECT state FROM objects WHERE sha256 = ?1",
            [digest.as_str()],
            |row| row.get(0),
        )?;
        ensure!(
            state == "present",
            "object is being deleted or is unavailable"
        );
        transaction.execute(
            "INSERT OR IGNORE INTO objects(sha256, size) VALUES (?1, ?2)",
            params![digest.as_str(), object.size],
        )?;
        transaction.execute("INSERT OR IGNORE INTO proxy_objects(proxy_id, digest) SELECT id, ?2 FROM proxy_repositories WHERE name = ?1", params![proxy, digest.as_str()])?;
        transaction.commit()?;
        Ok(())
    }
}
