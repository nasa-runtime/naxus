use std::{
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use url::Url;
use zeroize::Zeroizing;

/// 项目只登记一个服务入口，凭据材料不写入配置正文。
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    pub service: ServiceConfig,
    #[serde(default)]
    pub sources: Vec<ProxySource>,
}

/// 由 Naxus 返回的公开来源身份，业务侧不维护镜像的物理地址。
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProxySource {
    pub id: String,
    pub name: String,
    pub logical_index: String,
}

/// 业务侧稳定服务与 hosted 仓库的映射。
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub endpoint: Url,
    pub repository: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_file: Option<PathBuf>,
}

impl ClientConfig {
    /// 业务作用：原子保存已经绑定服务身份的项目配置，避免中断留下半份来源映射。
    /// 参数说明：
    /// - `path`：已持有工作区锁的配置目标，父目录须存在。
    ///
    /// 返回：新文件与父目录全部同步才成功，失败时不原地截断原配置。
    pub fn save(&self, path: &Path) -> Result<()> {
        use std::{fs, io::Write};
        self.validate()?;
        let parent = path
            .parent()
            .context("client configuration has no parent")?;
        let temporary = parent.join(format!("{}.configuration", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut options = fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(toml::to_string_pretty(self)?.as_bytes())?;
            file.sync_all()?;
            // 完整身份映射先落盘再替换入口，不能在原文件上截断写入。
            fs::rename(&temporary, path)?;
            fs::File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() && temporary.try_exists()? {
            fs::remove_file(&temporary)?;
            fs::File::open(parent)?.sync_all()?;
        }
        result
    }

    /// 业务作用：读取项目服务映射，拒绝不能安全承载凭据的地址。
    /// 参数说明：
    /// - `path`：项目配置绝对路径。
    ///
    /// 返回：校验后的单入口配置；未知键和嵌入凭据拒绝。
    pub fn load(path: &Path) -> Result<Self> {
        ensure!(
            std::fs::metadata(path)?.len() <= 1024 * 1024,
            "client configuration exceeds size limit"
        );
        let config: Self = toml::from_str(&std::fs::read_to_string(path)?)
            .map_err(|_| anyhow::anyhow!("invalid Naxus client configuration"))?;
        config.validate()?;
        Ok(config)
    }

    /// 业务作用：在网络调用和配置写入前校验凭据来源边界。
    /// 参数说明: 无。
    ///
    /// 返回：HTTPS 或回环 HTTP 地址合法时成功，不接受 URL 内嵌账号与令牌。
    pub fn validate(&self) -> Result<()> {
        naxus_core::validate_service_url(&self.service.endpoint)?;
        ensure!(
            naxus_core::validate_name(&self.service.repository).is_ok()
                || uuid::Uuid::parse_str(&self.service.repository).is_ok(),
            "invalid hosted repository identity"
        );
        ensure!(self.sources.len() <= 32, "too many proxy mappings");
        let mut names = std::collections::BTreeSet::new();
        let mut indexes = std::collections::BTreeSet::new();
        let mut identities = std::collections::BTreeSet::new();
        for source in &self.sources {
            uuid::Uuid::parse_str(&source.id)?;
            naxus_core::validate_name(&source.name)?;
            ensure!(
                source.name != "naxus"
                    && !source.name.starts_with("naxus-proxy-")
                    && names.insert(&source.name)
                    && indexes.insert(naxus_core::registry_identity(&source.logical_index)?)
                    && identities.insert(uuid::Uuid::parse_str(&source.id)?),
                "ambiguous proxy source mapping"
            );
            let index = Url::parse(
                source
                    .logical_index
                    .strip_prefix("sparse+")
                    .unwrap_or(&source.logical_index),
            )?;
            ensure!(
                matches!(index.scheme(), "http" | "https")
                    && index.username().is_empty()
                    && index.password().is_none()
                    && index.query().is_none()
                    && index.fragment().is_none(),
                "invalid logical registry source"
            );
        }
        Ok(())
    }

    /// 业务作用：从唯一入口构造协议地址，保留反向代理部署前缀。
    /// 参数说明：
    /// - `path`：以斜线开头的 Naxus 路由。
    ///
    /// 返回：固定服务来源的请求地址。
    pub fn url(&self, path: &str) -> String {
        format!(
            "{}{}",
            self.service.endpoint.as_str().trim_end_matches('/'),
            path
        )
    }

    /// 业务作用：从环境或受保护文件读取凭据，并在释放时零化材料。
    /// 参数说明：
    /// - `path`：项目配置位置，用于固定相对 token_file。
    ///
    /// 返回：暂存令牌；没有配置材料返回 None，过大或无效材料拒绝。
    pub fn token(&self, path: &Path) -> Result<Option<Zeroizing<String>>> {
        if let Some(value) = std::env::var_os("NAXUS_TOKEN") {
            let value = Zeroizing::new(
                value
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("NAXUS_TOKEN must be UTF-8"))?,
            );
            ensure!(
                (32..=512).contains(&value.len())
                    && value.bytes().all(|byte| byte.is_ascii_graphic()),
                "invalid NAXUS_TOKEN material"
            );
            return Ok(Some(value));
        }
        let Some(token_path) = &self.service.token_file else {
            return Ok(None);
        };
        let token_path = if token_path.is_absolute() {
            token_path.clone()
        } else {
            path.parent()
                .context("configuration has no parent")?
                .join(token_path)
        };
        let metadata = std::fs::symlink_metadata(&token_path)?;
        ensure!(
            metadata.file_type().is_file() && metadata.len() <= 1024,
            "invalid credential file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                metadata.permissions().mode() & 0o077 == 0,
                "credential file must be private to its owner"
            );
        }
        let mut material = Zeroizing::new(String::new());
        std::fs::File::open(token_path)?
            .take(1025)
            .read_to_string(&mut material)?;
        let value = Zeroizing::new(material.trim().to_owned());
        ensure!(
            (32..=512).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_graphic()),
            "invalid credential material"
        );
        Ok(Some(value))
    }

    /// 业务作用：限定 credential provider 响应的来源，避免凭据泄露给其他 registry。
    /// 参数说明：
    /// - `index`：Cargo 请求的 registry index URL。
    ///
    /// 返回：本服务、本仓库的逻辑或不可变快照来源匹配时为 true。
    pub fn permits_index(&self, index: &str) -> bool {
        let Ok(index) = Url::parse(index.strip_prefix("sparse+").unwrap_or(index)) else {
            return false;
        };
        let endpoint = &self.service.endpoint;
        if index.origin() != endpoint.origin()
            || !index.username().is_empty()
            || index.password().is_some()
            || index.query().is_some()
            || index.fragment().is_some()
        {
            return false;
        }
        let prefix = format!(
            "{}/repositories/{}/",
            endpoint.path().trim_end_matches('/'),
            self.service.repository
        );
        let Some(path) = index.path().strip_prefix(&prefix) else {
            return false;
        };
        if path == "index/" {
            return true;
        }
        let parts = path.split('/').collect::<Vec<_>>();
        parts.len() == 4
            && parts[0] == "snapshots"
            && uuid::Uuid::parse_str(parts[1]).is_ok()
            && parts[2] == "index"
            && parts[3].is_empty()
    }

    /// 业务作用：允许同一服务公开代理请求凭据，保持 hosted 包识别与凭据范围独立。
    /// 参数说明：
    /// - `index`：Cargo 请求的索引来源。
    ///
    /// 返回：仅受配置 endpoint 约束的 hosted 或代理索引可获得令牌。
    pub fn permits_credential_index(&self, index: &str) -> bool {
        if self.permits_index(index) {
            return true;
        }
        let Ok(index) = Url::parse(index.strip_prefix("sparse+").unwrap_or(index)) else {
            return false;
        };
        let endpoint = &self.service.endpoint;
        if index.origin() != endpoint.origin()
            || !index.username().is_empty()
            || index.password().is_some()
            || index.query().is_some()
            || index.fragment().is_some()
        {
            return false;
        }
        let prefix = format!("{}/proxies/", endpoint.path().trim_end_matches('/'));
        let Some(path) = index.path().strip_prefix(&prefix) else {
            return false;
        };
        let parts = path.split('/').collect::<Vec<_>>();
        let allowed = self.sources.iter().any(|source| source.name == parts[0]);
        allowed
            && ((parts.len() == 3 && parts[1] == "index" && parts[2].is_empty())
                || (parts.len() == 5
                    && parts[1] == "sessions"
                    && uuid::Uuid::parse_str(parts[2]).is_ok()
                    && parts[3] == "index"
                    && parts[4].is_empty()))
    }
}
