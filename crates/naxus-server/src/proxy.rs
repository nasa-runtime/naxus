use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    io,
    net::{IpAddr, SocketAddr},
    sync::{atomic::Ordering, Arc},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, ensure, Context, Result};
use futures_util::TryStreamExt;
use naxus_core::ArtifactDigest;
use reqwest::{redirect::Policy, StatusCode};
use tokio_util::io::{StreamReader, SyncIoBridge};
use url::Url;

use crate::upstream::{Failure, UpstreamRuntime};
use crate::{config::UpstreamConfig, http, ServerState};

/// 全部上游均明确表示坐标不存在，与上游超时或拒绝访问区分。
#[derive(Debug)]
pub(crate) struct NotFound;

impl std::fmt::Display for NotFound {
    /// 业务作用：提供不包含上游地址的缺失描述。
    /// 参数说明：
    /// - `formatter`：错误输出目标。
    ///
    /// 返回：固定的不存在描述。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("public package not found")
    }
}

impl std::error::Error for NotFound {}

/// 业务作用：返回解析 session 首次采用的索引，不因另一个客户端刷新而改变本次视图。
/// 参数说明：
/// - `state`：当前受管资源。
/// - `session`：已经授权且未过期的解析视图。
/// - `name`：crate 名称。
///
/// 返回：本次固定的原始索引；revalidate 模式首次读取必须取得上游新鲜证据。
pub(crate) async fn session_index(
    state: &Arc<ServerState>,
    session: &naxus_store::ProxySession,
    name: &str,
) -> Result<Vec<u8>> {
    let name_owned = name.to_owned();
    let selected = session.clone();
    let selected_name = name_owned.clone();
    if let Some(body) = http::storage(state, move |store| {
        store.proxy_session_entry(&selected, &selected_name)
    })
    .await?
    {
        validate_source_routes(state, &body)?;
        state.metrics.index_cache.fetch_add(1, Ordering::Relaxed);
        return Ok(body);
    }
    let body = index_view(
        state,
        &session.proxy,
        name,
        session.mode == "revalidate" || session.upstream_start > 0,
        session.upstream_start,
        tokio::time::Instant::now() + Duration::from_secs(120),
    )
    .await?;
    let session = session.clone();
    http::storage(state, move |store| {
        store.freeze_proxy_entry(&session, &name_owned, &body)
    })
    .await
}

/// 业务作用：读取公开索引，命中有效缓存时不回源，缺失时按配置顺序尝试同源镜像。
/// 参数说明：
/// - `state`：存储、上游白名单和并发预算。
/// - `proxy`：独立来源名称。
/// - `name`：已验证的 crate 名称。
/// - `force`：显式要求重新确认上游，失败时不使用过期索引。
///
/// 返回：上游原始 JSONL；全部明确缺失为 NotFound，其余失败不会保存无效索引。
pub(crate) async fn index(
    state: &Arc<ServerState>,
    proxy: &str,
    name: &str,
    force: bool,
) -> Result<Vec<u8>> {
    index_view(
        state,
        proxy,
        name,
        force,
        0,
        tokio::time::Instant::now() + Duration::from_secs(120),
    )
    .await
}

/// 业务作用：以单个镜像的原始索引建立视图，保持完整图重试与已固定视图隔离。
/// 参数说明：
/// - `state`：受管资源和来源配置。
/// - `proxy`：独立来源名称。
/// - `name`：crate 名称。
/// - `force`：是否必须取得上游新鲜证据。
/// - `upstream_start`：按配置顺序开始尝试的位置。
/// - `deadline`：含排队、索引与后续包体阶段的同一次操作期限。
///
/// 返回：首个有效原始索引，不拼接不同镜像的版本记录。
async fn index_view(
    state: &Arc<ServerState>,
    proxy: &str,
    name: &str,
    force: bool,
    upstream_start: usize,
    deadline: tokio::time::Instant,
) -> Result<Vec<u8>> {
    let configuration = state
        .config
        .proxies
        .iter()
        .find(|item| item.name == proxy)
        .context(NotFound)?;
    let _budget = state
        .proxy_budget
        .clone()
        .try_acquire_owned()
        .context("proxy budget exceeded")?;
    let _gate = tokio::time::timeout_at(
        deadline,
        gate(state, &format!("index:{proxy}:{name}")).lock(),
    )
    .await
    .context("proxy index queue timed out")?;
    let owned_proxy = proxy.to_owned();
    let owned_name = name.to_owned();
    let cached = http::storage(state, move |store| {
        store.proxy_index(&owned_proxy, &owned_name)
    })
    .await?;
    if !force && cached.is_none() {
        let proxy = proxy.to_owned();
        let name = name.to_owned();
        if http::storage(state, move |store| store.proxy_missing(&proxy, &name)).await? {
            return Err(NotFound.into());
        }
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    if !force
        && cached.as_ref().is_some_and(|cached| {
            now.saturating_sub(cached.checked_at as u64) < configuration.index_ttl_seconds
        })
    {
        let body = cached.expect("checked cache").body;
        validate_source_routes(state, &body)?;
        state.metrics.index_cache.fetch_add(1, Ordering::Relaxed);
        return Ok(body);
    }
    let path = naxus_registry::index_path(name)?;
    let mut missing = 0;
    for (position, upstream) in configuration
        .upstreams
        .iter()
        .enumerate()
        .skip(upstream_start)
    {
        let runtime = state
            .upstreams
            .iter()
            .find(|runtime| runtime.proxy == proxy && runtime.position == position)
            .context(Failure::InvalidResponse)?;
        let fetched = async {
            let validation_proxy = proxy.to_owned();
            let validation_name = name.to_owned();
            let validation_upstream = upstream.index.to_string();
            let validation = http::storage(state, move |store| {
                store.upstream_validation(&validation_proxy, &validation_name, &validation_upstream)
            })
            .await?;
            let response = request(
                upstream.index.join(&path)?,
                upstream,
                validation.as_ref(),
                deadline,
                &state.metrics,
                runtime,
                false,
            )
            .await?;
            let received_etag = response
                .headers()
                .get("etag")
                .and_then(|value| value.to_str().ok())
                .filter(|value| value.len() <= 4096)
                .map(str::to_owned);
            let received_modified = response
                .headers()
                .get("last-modified")
                .and_then(|value| value.to_str().ok())
                .filter(|value| value.len() <= 4096)
                .map(str::to_owned);
            let (body, etag, modified) = if response.status() == StatusCode::NOT_MODIFIED {
                let validation = validation.context("upstream 304 has no prior evidence")?;
                (
                    validation.body,
                    received_etag.or(validation.etag),
                    received_modified.or(validation.modified),
                )
            } else {
                ensure!(
                    response.status().is_success(),
                    "upstream index request failed"
                );
                (
                    bounded_body(response, 32 * 1024 * 1024, &state.metrics.index_bytes).await?,
                    received_etag,
                    received_modified,
                )
            };
            let proxy = proxy.to_owned();
            let name = name.to_owned();
            validate_source_routes(state, &body)?;
            let saved = body.clone();
            let upstream = upstream.index.to_string();
            http::storage(state, move |store| {
                store.save_proxy_index(&proxy, &name, &saved)?;
                store.save_upstream_validation(
                    &proxy,
                    &name,
                    &upstream,
                    &naxus_store::UpstreamValidation {
                        etag,
                        modified,
                        body: saved,
                    },
                )
            })
            .await?;
            Ok::<_, anyhow::Error>(body)
        }
        .await;
        match fetched {
            Ok(body) => return Ok(body),
            Err(error) => {
                if matches!(error.downcast_ref::<Failure>(), Some(Failure::Missing)) {
                    missing += 1;
                }
                // 未知错误包括结构、来源和坐标冲突；不能以另一份响应掩盖已观察到的不可信内容。
                if !runtime.failover(&error) {
                    return Err(error);
                }
            }
        }
    }
    if missing == configuration.upstreams.len().saturating_sub(upstream_start) {
        if upstream_start > 0 {
            return Err(NotFound.into());
        }
        let proxy = proxy.to_owned();
        let name = name.to_owned();
        http::storage(state, move |store| store.save_proxy_missing(&proxy, &name)).await?;
        return Err(NotFound.into());
    }
    // 上游故障不撤销已缓存依赖的读取能力；显式刷新必须报告无法取得新鲜证据。
    if !force {
        if let Some(cached) = cached {
            validate_source_routes(state, &cached.body)?;
            state.metrics.index_cache.fetch_add(1, Ordering::Relaxed);
            return Ok(cached.body);
        }
    }
    bail!("public index upstream unavailable")
}

/// 业务作用：在公开索引进入 Cargo 之前拒绝未配置的跨 registry 来源，防止解析过程直接外连。
/// 参数说明：
/// - `state`：当前启用的独立来源映射。
/// - `body`：保持原始字节的上游 JSONL。
///
/// 返回：每个显式 registry 依赖都有本服务映射才成功，不改写索引来源身份。
fn validate_source_routes(state: &ServerState, body: &[u8]) -> Result<()> {
    let sources = state
        .config
        .proxies
        .iter()
        .map(|proxy| naxus_core::registry_identity(&proxy.logical_index))
        .collect::<Result<std::collections::BTreeSet<_>>>()?;
    for line in body
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let entry: naxus_core::IndexEntry = serde_json::from_slice(line)?;
        // 既有缓存也可能来自较早的输入边界；开放索引前复验同一语义，不能只约束本次回源。
        entry.validate_upstream()?;
        for dependency in entry.deps {
            if let Some(registry) = dependency.registry {
                // 来源相等必须与配置判重一致，不能因公开别名的拼写而误拒绝，也不能允许未映射来源外连。
                ensure!(
                    sources.contains(&naxus_core::registry_identity(&registry)?),
                    "upstream dependency has no configured Naxus source mapping"
                );
            }
        }
    }
    Ok(())
}

/// 业务作用：在向业务客户端开放下载前，按索引摘要持久化上游原始包体。
/// 参数说明：
/// - `state`：存储、上游白名单和资源预算。
/// - `proxy`：独立公开来源。
/// - `name`：crate 名称。
/// - `version`：精确 SemVer。
///
/// 返回：已验证且已关联本来源的对象摘要；已有包体不重复回源。
pub(crate) async fn materialize(
    state: &Arc<ServerState>,
    proxy: &str,
    name: &str,
    version: &str,
) -> Result<ArtifactDigest> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    naxus_core::validate_name(name)?;
    semver::Version::parse(version)?;
    let configuration = state
        .config
        .proxies
        .iter()
        .find(|item| item.name == proxy)
        .context(NotFound)?;
    let proxy_name = proxy.to_owned();
    let crate_name = name.to_owned();
    let crate_version = version.to_owned();
    let mut digest = http::storage(state, move |store| {
        store.proxy_digest(&proxy_name, &crate_name, &crate_version)
    })
    .await?;
    if digest.is_none() {
        index_view(state, proxy, name, false, 0, deadline).await?;
        let proxy = proxy.to_owned();
        let name = name.to_owned();
        let version = version.to_owned();
        digest = http::storage(state, move |store| {
            store.proxy_digest(&proxy, &name, &version)
        })
        .await?;
    }
    let digest = digest.context(NotFound)?;
    let _budget = state
        .proxy_budget
        .clone()
        .try_acquire_owned()
        .context("proxy budget exceeded")?;
    let _gate = tokio::time::timeout_at(
        deadline,
        gate(state, &format!("object:{proxy}:{digest}")).lock(),
    )
    .await
    .context("proxy artifact queue timed out")?;
    let store = http::ready_store(state).map_err(|_| anyhow::anyhow!("repository unavailable"))?;
    let existing_proxy = proxy.to_owned();
    let existing_digest = digest.clone();
    if http::storage(state, move |store| {
        store.proxy_has_object(&existing_proxy, &existing_digest)
    })
    .await?
    {
        state.metrics.artifact_cache.fetch_add(1, Ordering::Relaxed);
        return Ok(digest);
    }
    let mut missing = 0;
    for (position, upstream) in configuration.upstreams.iter().enumerate() {
        let runtime = state
            .upstreams
            .iter()
            .find(|runtime| runtime.proxy == proxy && runtime.position == position)
            .context(Failure::InvalidResponse)?;
        let downloaded = async {
            let configuration = request(
                upstream.index.join("config.json")?,
                upstream,
                None,
                deadline,
                &state.metrics,
                runtime,
                false,
            )
            .await?;
            ensure!(
                configuration.status().is_success(),
                "upstream config request failed"
            );
            let configuration: serde_json::Value = serde_json::from_slice(
                &bounded_body(configuration, 64 * 1024, &state.metrics.index_bytes).await?,
            )?;
            let template = configuration["dl"]
                .as_str()
                .context("upstream download template missing")?;
            let url = download_url(template, name, version, &digest)?;
            let response =
                request(url, upstream, None, deadline, &state.metrics, runtime, true).await?;
            ensure!(
                response.status().is_success(),
                "upstream artifact request failed"
            );
            ensure!(
                response.content_length().is_none_or(|length| length
                    <= state
                        .config
                        .data
                        .max_artifact_bytes
                        .unwrap_or(256 * 1024 * 1024)),
                "upstream artifact exceeds limit"
            );
            let metrics = state.metrics.clone();
            let stream = StreamReader::new(
                response
                    .bytes_stream()
                    .inspect_ok(move |bytes| {
                        metrics
                            .artifact_bytes
                            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                    })
                    .map_err(|error| io::Error::other(crate::upstream::transport(error))),
            );
            let mut bridge = SyncIoBridge::new(stream);
            let proxy = proxy.to_owned();
            let digest = digest.clone();
            let store = store.clone();
            let permit = state
                .work
                .clone()
                .try_acquire_owned()
                .context("storage budget exceeded")?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                store.install_proxy_object(&proxy, &mut bridge, &digest)
            })
            .await?
            .map_err(|error| {
                let failure = crate::upstream::embedded_failure(&error);
                match failure {
                    Some(failure) => anyhow::Error::new(failure).context(error),
                    None => error,
                }
            })?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        match downloaded {
            Ok(()) => return Ok(digest),
            Err(error) => {
                if matches!(error.downcast_ref::<Failure>(), Some(Failure::Missing)) {
                    missing += 1;
                }
                // 包体只能匹配已固定摘要，授权和完整性拒绝不能通过切换下载地址降级为成功。
                if !runtime.failover(&error) {
                    return Err(error);
                }
            }
        }
    }
    if missing == configuration.upstreams.len() {
        return Err(NotFound.into());
    }
    bail!("public artifact upstream unavailable")
}

/// 业务作用：使用固定数量的互斥分片合并同一制品的并发回源，避免键表无界增长。
/// 参数说明：
/// - `state`：实例级固定分片。
/// - `key`：索引或对象的业务身份。
///
/// 返回：本键对应门禁；哈希碰撞只降低并发度。
fn gate<'a>(state: &'a ServerState, key: &str) -> &'a tokio::sync::Mutex<()> {
    let mut hash = DefaultHasher::new();
    key.hash(&mut hash);
    &state.proxy_locks[hash.finish() as usize % state.proxy_locks.len()]
}

/// 业务作用：在每次请求及重定向前核对目标主机和全部 DNS 结果，并将本次连接固定到已验证地址。
/// 参数说明：
/// - `url`：由受信任配置或已验证上游模板生成的目标。
/// - `upstream`：允许的索引、包体主机及私网策略。
/// - `validation`：只属于当前上游的条件请求标记。
/// - `deadline`：本次全部镜像和跳转共享的回源截止时刻。
/// - `metrics`：应用唯一指标源。
/// - `runtime`：该配置上游共享的冷却门禁与有界观测状态。
/// - `artifact`：是否计入包体请求，否则计入索引与配置请求。
///
/// 返回：最多五次已验证重定向后的响应；不继承环境代理，不转发业务凭据。
async fn request(
    mut url: Url,
    upstream: &UpstreamConfig,
    validation: Option<&naxus_store::UpstreamValidation>,
    deadline: tokio::time::Instant,
    metrics: &crate::metrics::Metrics,
    runtime: &UpstreamRuntime,
    artifact: bool,
) -> Result<reqwest::Response> {
    let deadline = deadline
        .min(tokio::time::Instant::now() + Duration::from_secs(upstream.request_timeout_seconds));
    let original_origin = url.origin();
    for _ in 0..=5 {
        runtime.check()?;
        ensure!(tokio::time::Instant::now() < deadline, Failure::Transient);
        ensure!(
            url.username().is_empty() && url.password().is_none() && url.fragment().is_none(),
            "invalid upstream URL"
        );
        ensure!(
            url.scheme() == "https" || (url.scheme() == "http" && upstream.allow_private_network),
            "upstream transport rejected"
        );
        let host = url
            .host_str()
            .context("upstream host missing")?
            .trim_matches(['[', ']']);
        let port = url
            .port_or_known_default()
            .context("upstream port missing")?;
        let download_origin = upstream
            .download_hosts
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(host))
            && ((url.scheme() == "https" && port == 443)
                || (upstream.allow_private_network && url.scheme() == "http" && port == 80));
        ensure!(
            url.origin() == upstream.index.origin() || download_origin,
            "upstream origin is not allowed"
        );
        let addresses = tokio::time::timeout(
            Duration::from_secs(10)
                .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
            tokio::net::lookup_host((host, port)),
        )
        .await
        .map_err(|_| Failure::Transient)?
        .map_err(|error| anyhow::Error::new(Failure::Transient).context(error))?
        .collect::<Vec<SocketAddr>>();
        ensure!(
            !addresses.is_empty() && addresses.len() <= 64,
            "invalid upstream DNS answer"
        );
        // 先验证所有候选地址，再固定 DNS 结果；重定向与后续连接不能重新解释为内网目标。
        ensure!(
            upstream.allow_private_network
                || addresses.iter().all(|address| public_address(address.ip())),
            "upstream private address rejected"
        );
        ensure!(tokio::time::Instant::now() < deadline, Failure::Transient);
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(deadline.saturating_duration_since(tokio::time::Instant::now()))
            .resolve_to_addrs(host, &addresses)
            .build()?;
        let mut request = client
            .get(url.clone())
            .header("accept-encoding", "identity")
            .header("user-agent", concat!("naxus/", env!("CARGO_PKG_VERSION")));
        if url.origin() == original_origin {
            if let Some(validation) = validation {
                if let Some(etag) = &validation.etag {
                    request = request.header("if-none-match", etag);
                } else if let Some(modified) = &validation.modified {
                    request = request.header("if-modified-since", modified);
                }
            }
        }
        // DNS 校验期间其他请求也可能取得限流证据，发出网络请求前需要再次复验。
        runtime.check()?;
        if artifact {
            metrics.artifact_requests.fetch_add(1, Ordering::Relaxed);
        } else {
            metrics.index_requests.fetch_add(1, Ordering::Relaxed);
        }
        let response = request.send().await.map_err(crate::upstream::transport)?;
        if response.status().is_redirection() && response.status() != StatusCode::NOT_MODIFIED {
            let location = response
                .headers()
                .get("location")
                .context("upstream redirect missing location")?
                .to_str()?;
            let next = url.join(location)?;
            ensure!(
                url.scheme() != "https" || next.scheme() == "https",
                "upstream TLS downgrade rejected"
            );
            url = next;
        } else {
            runtime.response(
                response.status(),
                response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok()),
                upstream.max_cooldown_seconds,
            )?;
            return Ok(response);
        }
    }
    bail!("upstream redirect limit exceeded")
}

/// 业务作用：拒绝默认回源连接中的本机、保留地址及内部网络，缩小恶意上游元数据的可达边界。
/// 参数说明：
/// - `address`：已经解析且即将固定的地址。
///
/// 返回：保守判定为公网单播地址才允许。
fn public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_documentation()
                && !ip.is_broadcast()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 198 && (18..=19).contains(&b))
                && !(a == 192 && b == 0 && c == 0)
                && !(a == 192 && b == 88 && c == 99)
        }
        IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() {
                return public_address(IpAddr::V4(ip));
            }
            let segments = ip.segments();
            // 隧道地址不能以外层 IPv6 前缀绕过内层 IPv4 门禁；文档网段也不作为公开回源目标。
            segments[0] & 0xe000 == 0x2000
                && segments[0] != 0x2002
                && !(segments[0] == 0x2001 && (segments[1] == 0x0db8 || segments[1] <= 0x01ff))
                && !(segments[0] == 0x3fff && segments[1] < 0x1000)
        }
    }
}

/// 业务作用：在读取上游控制正文时持续执行字节上限，不信任 Content-Length。
/// 参数说明：
/// - `response`：已经验证目标的响应。
/// - `limit`：允许读入内存的最大字节数。
/// - `bytes`：本次实际读取正文的累计指标。
///
/// 返回：完整有界正文；任何超额或传输错误拒绝缓存。
async fn bounded_body(
    mut response: reqwest::Response,
    limit: usize,
    bytes: &std::sync::atomic::AtomicU64,
) -> Result<Vec<u8>> {
    ensure!(
        response
            .content_length()
            .is_none_or(|length| length <= limit as u64),
        "upstream metadata exceeds limit"
    );
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(crate::upstream::transport)? {
        bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        ensure!(
            body.len().saturating_add(chunk.len()) <= limit,
            "upstream metadata exceeds limit"
        );
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// 业务作用：展开 Cargo 下载模板，保留上游原始制品定位规则。
/// 参数说明：
/// - `template`：上游 config.json 的 dl。
/// - `name`：合法 crate 名称。
/// - `version`：合法精确版本。
/// - `digest`：索引中的摘要。
///
/// 返回：仍须通过主机和 DNS 门禁的下载 URL。
fn download_url(template: &str, name: &str, version: &str, digest: &ArtifactDigest) -> Result<Url> {
    let path = naxus_registry::index_path(name)?;
    let prefix = path
        .rsplit_once('/')
        .map(|(prefix, _)| prefix)
        .unwrap_or("");
    let value = if template.contains('{') {
        template
            .replace("{crate}", name)
            .replace("{version}", version)
            .replace("{prefix}", prefix)
            .replace("{lowerprefix}", &prefix.to_ascii_lowercase())
            .replace("{sha256-checksum}", digest.as_str())
    } else {
        format!(
            "{}/{name}/{version}/download",
            template.trim_end_matches('/')
        )
    };
    ensure!(
        !value.contains(['{', '}']),
        "unknown upstream download template field"
    );
    Ok(Url::parse(&value)?)
}
