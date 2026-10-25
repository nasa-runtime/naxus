use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{ensure, Context, Result};
use naxus_client::ClientConfig;
use naxus_core::{ArtifactDigest, RepositoryPolicy, Snapshot};
use reqwest::{
    blocking::{Client, RequestBuilder, Response},
    Method,
};
use serde::{de::DeserializeOwned, Deserialize};

#[derive(Deserialize)]
pub(crate) struct Capabilities {
    pub service_uuid: String,
    pub protocol_min: u32,
    pub protocol_max: u32,
    pub public_base_url: url::Url,
}

#[derive(Deserialize)]
pub(crate) struct Repository {
    pub id: String,
    pub name: String,
    pub policy: RepositoryPolicy,
    pub head: String,
}

#[derive(Clone)]
pub(crate) struct Remote {
    pub config: ClientConfig,
    path: PathBuf,
    http: Client,
}

impl Remote {
    /// 业务作用：建立只向配置来源发送凭据的客户端，禁止隐式代理与自动重定向。
    /// 参数说明：
    /// - `config`：已固定的项目配置。
    /// - `path`：凭据文件相对位置的配置基准。
    ///
    /// 返回：有界 HTTP 客户端；配置非法时不执行网络请求。
    pub fn new(config: ClientConfig, path: PathBuf) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            path,
            http: Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(130))
                .build()?,
        })
    }

    /// 业务作用：在任何写操作前协商协议并核对已登记服务身份。
    /// 参数说明: 无。
    ///
    /// 返回：兼容的稳定服务描述；错误恢复或服务身份变化拒绝继续。
    pub fn capabilities(&self) -> Result<Capabilities> {
        let capabilities: Capabilities = self.get("/api/naxus/v1/capabilities")?;
        ensure!(
            capabilities.protocol_min <= 1 && capabilities.protocol_max >= 1,
            "Naxus protocol is incompatible"
        );
        uuid::Uuid::parse_str(&capabilities.service_uuid)?;
        ensure!(
            capabilities.public_base_url.as_str().trim_end_matches('/')
                == self.config.service.endpoint.as_str().trim_end_matches('/'),
            "configured endpoint differs from the server's canonical public_base_url"
        );
        ensure!(
            self.config
                .service
                .service_uuid
                .as_ref()
                .is_none_or(|expected| *expected == capabilities.service_uuid),
            "Naxus service identity changed"
        );
        Ok(capabilities)
    }

    /// 业务作用：把项目的仓库名称解析为当前服务的稳定仓库身份。
    /// 参数说明: 无。
    ///
    /// 返回：唯一匹配的仓库；不存在时不回退到其他仓库或公开来源。
    pub fn repository(&self) -> Result<Repository> {
        let repositories: Vec<Repository> = self.get("/api/naxus/v1/repositories")?;
        let repository = repositories
            .into_iter()
            .find(|repository| {
                repository.name == self.config.service.repository
                    || repository.id == self.config.service.repository
            })
            .context("configured hosted repository does not exist")?;
        uuid::Uuid::parse_str(&repository.id)?;
        uuid::Uuid::parse_str(&repository.head)?;
        Ok(repository)
    }

    /// 业务作用：在 Cargo 开始解析前核对服务端来源映射，防止新出现的跨 registry 依赖绕过单入口。
    /// 参数说明: 无。
    ///
    /// 返回：与项目已绑定的代理身份完全一致才成功；来源改变要求重新配置项目。
    pub fn validate_sources(&self) -> Result<()> {
        let sources: Vec<naxus_client::ProxySource> = self.get("/api/naxus/v1/proxies")?;
        let mapped = |sources: &[naxus_client::ProxySource]| {
            sources
                .iter()
                .map(|source| {
                    (
                        source.name.clone(),
                        (source.id.clone(), source.logical_index.clone()),
                    )
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        ensure!(mapped(&sources) == mapped(&self.config.sources), "Naxus source mappings changed; run cargo naxus configure before resolving dependencies");
        Ok(())
    }

    /// 业务作用：固定仓库当前 HEAD，后续索引访问仅使用这个快照。
    /// 参数说明: 无。
    ///
    /// 返回：已校验的不可变快照描述。
    pub fn head(&self) -> Result<Snapshot> {
        let snapshot: Snapshot = self.get(&format!(
            "/api/naxus/v1/repositories/{}/head",
            self.config.service.repository
        ))?;
        uuid::Uuid::parse_str(&snapshot.id)?;
        uuid::Uuid::parse_str(&snapshot.repository_id)?;
        Ok(snapshot)
    }

    /// 业务作用：读取有界控制 JSON，不回显响应中的任意正文。
    /// 参数说明：
    /// - `path`：固定服务下的协议路径。
    ///
    /// 返回：成功 JSON；HTTP 失败或响应超限时拒绝。
    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        Self::json(self.request(Method::GET, path)?.send()?)
    }

    /// 业务作用：以完整发布预算创建或回读批次，提交确认与恢复不能被普通控制响应上限阻断。
    /// 参数说明：
    /// - `path`：当前仓库的批次集合或精确批次路径。
    /// - `creation`：创建时的原固定请求；None 表示读取已有批次。
    ///
    /// 返回：受批次元数据预算约束的完整 JSON；响应超限仍明确拒绝并保留发布日志。
    pub fn batch<T: DeserializeOwned>(
        &self,
        path: &str,
        creation: Option<&serde_json::Value>,
    ) -> Result<T> {
        let request = if let Some(creation) = creation {
            self.request(Method::POST, path)?.json(creation)
        } else {
            self.request(Method::GET, path)?
        };
        Ok(serde_json::from_slice(&Self::body(
            request.send()?,
            naxus_core::MAX_BATCH_RESPONSE_BYTES,
        )?)?)
    }

    /// 业务作用：向固定来源提交元数据操作，凭据不进入 URL 或请求日志。
    /// 参数说明：
    /// - `path`：协议路径。
    /// - `value`：不含令牌的控制请求。
    ///
    /// 返回：成功响应的 JSON；失败保留服务端真实 HTTP 状态。
    pub fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        value: &impl serde::Serialize,
    ) -> Result<T> {
        Self::json(self.request(Method::POST, path)?.json(value).send()?)
    }

    /// 业务作用：命令结束时有界释放短期服务资源，不延迟业务进程无限等待清理。
    /// 参数说明：
    /// - `path`：当前主体拥有的短期资源路径。
    ///
    /// 返回：两秒内收到成功状态才成功；失败资源由服务端到期机制回收。
    pub fn close(&self, path: &str) -> Result<()> {
        let response = self
            .request(Method::DELETE, path)?
            .timeout(Duration::from_secs(2))
            .send()?;
        ensure!(
            response.status().is_success(),
            "cannot close temporary server resource"
        );
        Ok(())
    }

    /// 业务作用：在独立短期限内续期构建租约，避免退出时等待普通下载期限。
    /// 参数说明：
    /// - `path`：原主体已持有的仓库租约路径。
    ///
    /// 返回：服务端确认的期限与租约绑定；调用方还须核对原解析身份，失败不视为持续保护。
    pub fn renew(&self, path: &str) -> Result<naxus_core::LeaseProof> {
        let response = self
            .request(Method::POST, path)?
            .timeout(Duration::from_secs(10))
            .send()?;
        Self::json(response)
    }

    /// 业务作用：流式上传本次固定的归档字节，保持摘要、长度与文件内容一致。
    /// 参数说明：
    /// - `path`：批次中某个坐标的上传路径。
    /// - `file`：本次打包输出的精确文件。
    /// - `digest`：上传前计算的原始 SHA-256。
    ///
    /// 返回：静态检查后的索引 JSON；413 提示明确结束旧意图后缩小输入，失败不自动提交或终止批次。
    pub fn upload(
        &self,
        path: &str,
        file: &Path,
        digest: &ArtifactDigest,
    ) -> Result<serde_json::Value> {
        let response = self
            .request(Method::PUT, path)?
            .header("X-Checksum-Sha256", digest.as_str())
            .header("Content-Length", std::fs::metadata(file)?.len())
            .body(File::open(file)?)
            .send()?;
        // 固定输入被拒绝后仍保留原发布权威；只有用户明确终止旧意图，才能重新打包较小的输入。
        ensure!(
            response.status() != reqwest::StatusCode::PAYLOAD_TOO_LARGE,
            "Naxus returned HTTP 413: publication exceeds upload or batch limits; run cargo naxus publish --abort-pending before preparing smaller inputs"
        );
        Ok(serde_json::from_slice(&Self::body(
            response,
            naxus_core::MAX_BATCH_RESPONSE_BYTES,
        )?)?)
    }

    /// 业务作用：为固定服务地址附加当前受保护凭据。
    /// 参数说明：
    /// - `method`：协议方法。
    /// - `path`：当前服务中的协议路径。
    ///
    /// 返回：禁止跨来源跳转的请求构造器。
    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let mut request = self.http.request(method, self.config.url(path));
        if let Some(token) = self.config.token(&self.path)? {
            request = request.header("Authorization", token.as_str());
        }
        Ok(request)
    }

    /// 业务作用：在解析前限制元数据响应，错误正文不作为可信协议结果。
    /// 参数说明：
    /// - `response`：HTTP 返回结果。
    /// - `limit`：该协议资源的成功响应字节预算。
    ///
    /// 返回：预算内的完整成功正文，其他状态或超限明确失败。
    fn body(response: Response, limit: usize) -> Result<Vec<u8>> {
        ensure!(
            response.status().is_success(),
            "Naxus returned HTTP {}",
            response.status()
        );
        let mut bytes = Vec::new();
        response.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= limit,
            "Naxus metadata response exceeds size limit"
        );
        Ok(bytes)
    }

    /// 业务作用：把已确认成功的有界正文解析为协议模型。
    /// 参数说明：
    /// - `response`：HTTP 返回结果。
    ///
    /// 返回：符合当前协议的 JSON，不接受半响应或无效结构。
    fn json<T: DeserializeOwned>(response: Response) -> Result<T> {
        Ok(serde_json::from_slice(&Self::body(
            response,
            16 * 1024 * 1024,
        )?)?)
    }
}
