use std::{
    io,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

use anyhow::{ensure, Context};
use axum::body::{to_bytes, Body};
use futures_util::StreamExt;
use nasa::web::{
    from_fn_with_state, get, post, put, Extension, HeaderMap, IntoResponse, Json, Next, Path,
    Request, Response, State, StatusCode,
};
use naxus_core::ArtifactDigest;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio_util::io::{ReaderStream, StreamReader, SyncIoBridge};
use uuid::Uuid;

use crate::ServerState;

type ApiResult<T> = Result<T, ApiError>;

#[derive(Clone)]
struct Principal {
    subject: String,
    repositories: Vec<String>,
    can_publish: bool,
    token_digest: Option<ArtifactDigest>,
}

pub(crate) struct ApiError {
    pub(crate) status: StatusCode,
    code: &'static str,
    message: &'static str,
}

/// 标记已经生成统一错误体的响应，使管理边界只转换框架拒绝而不覆盖业务错误码。
#[derive(Clone, Copy)]
pub(crate) struct ErrorResponse;

tokio::task_local! {
    pub(crate) static RESPONSE_REQUEST_ID: String;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeginBatch {
    expected_head: String,
    idempotency_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveRequest {
    repository: String,
    client_protocol: u32,
    #[serde(default)]
    packages: Vec<naxus_core::Coordinate>,
    #[serde(default)]
    fixed_snapshot: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionRequest {
    mode: String,
    #[serde(default)]
    upstream_start: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MaterializeRequest {
    items: Vec<naxus_store::MaterializationItem>,
}

/// 业务作用：装配 Cargo 原始协议与扩展接口，使它们共享 NASA 的唯一监听器。
/// 参数说明：
/// - `app`：尚未封口的应用容器。
/// - `state`：仓库、认证和资源预算的共享状态。
///
/// 返回：完整路由登记成功；路径冲突或框架拒绝时不开放服务。
pub(crate) fn install(app: &nasa::Application, state: Arc<ServerState>) -> anyhow::Result<()> {
    app.configure_router(move |router| {
        router.merge(axum::Router::new()
            .route("/api/naxus/v1/capabilities", get(capabilities))
            .route("/api/naxus/v1/identity", get(caller_identity))
            .route("/api/naxus/v1/repositories", get(repositories))
            .route("/api/naxus/v1/proxies", get(proxies))
            .route(
                "/api/naxus/v1/materializations/{id}",
                get(materialization_valid),
            )
            .route("/proxies/{proxy}/index/{*path}", get(proxy_index))
            .route("/proxies/{proxy}/sessions", post(begin_proxy_session))
            .route("/proxies/{proxy}/views", get(proxy_views))
            .route(
                "/proxies/{proxy}/sessions/{session}",
                axum::routing::delete(finish_proxy_session),
            )
            .route(
                "/proxies/{proxy}/sessions/{session}/index/{*path}",
                get(proxy_session_index),
            )
            .route(
                "/proxies/{proxy}/sessions/{session}/materializations",
                post(materialize_graph),
            )
            .route(
                "/proxies/{proxy}/crates/{name}/{version}/download",
                get(proxy_download),
            )
            .route("/api/naxus/v1/resolutions", post(resolve))
            .route("/repositories/{repository}/api/v1/crates/new", put(cargo_publish))
            .route("/repositories/{repository}/api/v1/crates/{name}/{version}/yank", axum::routing::delete(cargo_yank))
            .route("/repositories/{repository}/api/v1/crates/{name}/{version}/unyank", put(cargo_unyank))
            .route(
                "/api/naxus/v1/repositories/{repository}/leases/{lease}",
                post(renew_lease).delete(release_lease),
            )
            .route("/api/naxus/v1/repositories/{repository}/head", get(head))
            .route(
                "/api/naxus/v1/repositories/{repository}/snapshots/{snapshot}",
                get(snapshot),
            )
            .route(
                "/api/naxus/v1/repositories/{repository}/batches",
                post(begin_batch),
            )
            .route(
                "/api/naxus/v1/repositories/{repository}/batches/{batch}",
                get(batch).delete(abort_batch),
            )
            .route(
                "/api/naxus/v1/repositories/{repository}/batches/{batch}/crates/{name}/{version}",
                put(upload),
            )
            .route(
                "/api/naxus/v1/repositories/{repository}/batches/{batch}/commit",
                post(commit_batch),
            )
            .route(
                "/repositories/{repository}/index/{*path}",
                get(logical_index),
            )
            .route(
                "/repositories/{repository}/snapshots/{snapshot}/index/{*path}",
                get(immutable_index),
            )
            .route("/repositories/{repository}/objects/{digest}", get(object))
            .route("/repositories/{repository}/crates/{name}/{version}/{digest}", get(current_object))
                .route("/repositories/{repository}/snapshots/{snapshot}/crates/{name}/{version}/{digest}", get(snapshot_object))
            .layer(Extension(state.clone()))
            .layer(from_fn_with_state(state, guard)))
    })?;
    Ok(())
}

/// 业务作用：在读取正文前执行认证与新写入门禁，保持 Cargo 401 和认证挑战语义。
/// 参数说明：
/// - `state`：受管认证与排空状态。
/// - `request`：原始入站请求。
/// - `next`：已经匹配的仓库路由。
///
/// 返回：认证主体被绑定的响应；未授权或排空时不读取包体、不创建批次。
async fn guard(
    State(state): State<Arc<ServerState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let request_id = request
        .extensions()
        .get::<nasa::application::RequestId>()
        .map(|id| id.as_str().to_owned())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    RESPONSE_REQUEST_ID
        .scope(request_id.clone(), async move {
            let headers = request.headers().clone();
            let writes = !matches!(
                *request.method(),
                axum::http::Method::GET | axum::http::Method::HEAD
            );
            let public_capabilities = request.uri().path() == "/api/naxus/v1/capabilities";
            let result = async {
                if state.draining.load(Ordering::Acquire) && writes {
                    return Err(ApiError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "service_draining",
                        "repository is draining",
                    ));
                }
                let Some(expected) = state.token.get() else {
                    return Err(ApiError::unavailable());
                };
                if public_capabilities {
                    return Ok(Principal {
                        subject: "anonymous".to_owned(),
                        repositories: Vec::new(),
                        can_publish: false,
                        token_digest: None,
                    });
                }
                if state.config.auth.mode == "none" && !headers.contains_key("authorization") {
                    return Ok(Principal {
                        subject: "local".to_owned(),
                        repositories: vec!["*".to_owned()],
                        can_publish: true,
                        token_digest: None,
                    });
                }
                ensure_single_authorization(&headers)?;
                let value = headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(ApiError::unauthorized)?;
                let value = value.strip_prefix("Bearer ").unwrap_or(value);
                let actual = ArtifactDigest::of(value.as_bytes());
                if let Some(expected) = expected {
                    if bool::from(
                        actual
                            .as_str()
                            .as_bytes()
                            .ct_eq(expected.as_str().as_bytes()),
                    ) {
                        return Ok(Principal {
                            subject: format!("token:{}", expected.as_str()),
                            repositories: vec!["*".to_owned()],
                            can_publish: true,
                            token_digest: None,
                        });
                    }
                }
                let token_digest = actual.clone();
                let identity = blocking(&state, move |store| store.token_identity(&actual))
                    .await?
                    .ok_or_else(ApiError::unauthorized)?;
                Ok(Principal {
                    subject: identity.subject,
                    repositories: identity.repositories,
                    can_publish: identity.can_publish,
                    token_digest: Some(token_digest),
                })
            }
            .await;
            let mut response = match result {
                Ok(principal) => {
                    if let Err(error) = authorize_registry(&state, &mut request, &principal).await {
                        return error.into_response();
                    }
                    request.extensions_mut().insert(principal);
                    next.run(request).await
                }
                Err(error) => error.into_response(),
            };
            if !response.headers().contains_key("x-request-id") {
                response.headers_mut().insert(
                    "x-request-id",
                    request_id.parse().expect("validated request id header"),
                );
            }
            response
        })
        .await
}

/// 业务作用：在读取包体前复验令牌的操作权限和路由仓库归属，禁止只读 token 发布或串仓读取。
/// 参数说明：
/// - `state`：当前存储。
/// - `request`：已经匹配的原始请求。
/// - `principal`：当前请求认证身份。
///
/// 返回：权限与仓库 UUID 匹配才继续；未授权请求不进入上传处理。
async fn authorize_registry(
    state: &Arc<ServerState>,
    request: &mut Request,
    principal: &Principal,
) -> ApiResult<()> {
    use axum::extract::FromRequestParts;
    let route = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|route| route.as_str())
        .unwrap_or("");
    if (route.contains("/batches") || route.contains("/api/v1/crates/"))
        && !matches!(
            *request.method(),
            axum::http::Method::GET | axum::http::Method::HEAD
        )
        && !principal.can_publish
    {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "permission_denied",
            "publish permission required",
        ));
    }
    if route.contains("{repository}") {
        let (mut parts, body) =
            std::mem::replace(request, Request::new(Body::empty())).into_parts();
        let parameters =
            Path::<std::collections::BTreeMap<String, String>>::from_request_parts(&mut parts, &())
                .await
                .map_err(|_| ApiError::bad("invalid repository path"));
        *request = Request::from_parts(parts, body);
        let repository = parameters?
            .0
            .get("repository")
            .cloned()
            .ok_or_else(ApiError::not_found)?;
        let grants = principal.repositories.clone();
        blocking(state, move |store| {
            repository_permission(&store, &repository, &grants)
        })
        .await?;
    }
    Ok(())
}

/// 业务作用：按持久仓库 UUID 而非请求别名裁决对象访问范围。
/// 参数说明：
/// - `store`：当前存储。
/// - `repository`：路由或控制请求中的仓库名称或 UUID。
/// - `grants`：令牌允许的仓库 UUID。
///
/// 返回：仓库存在且范围匹配才成功，不通过名称歧义扩大授权。
fn repository_permission(
    store: &naxus_store::Store,
    repository: &str,
    grants: &[String],
) -> anyhow::Result<()> {
    let repository = store
        .repositories()?
        .into_iter()
        .find(|item| item.name == repository || item.id == repository)
        .context(naxus_store::RepositoryFailure::NotFound(
            "unknown repository",
        ))?;
    ensure!(
        grants
            .iter()
            .any(|grant| grant == "*" || grant == &repository.id),
        naxus_store::RepositoryFailure::Forbidden("repository permission denied")
    );
    Ok(())
}

/// 业务作用：避免多个认证头在代理与应用之间产生不同主体解释。
/// 参数说明：
/// - `headers`：原始请求头集合。
///
/// 返回：恰好一个 Authorization 时成功，其余情况按未认证拒绝。
fn ensure_single_authorization(headers: &HeaderMap) -> ApiResult<()> {
    if headers.get_all("authorization").iter().count() != 1 {
        return Err(ApiError::unauthorized());
    }
    Ok(())
}

/// 业务作用：公开最小协议协商信息，不暴露仓库名、凭据或本地路径。
/// 参数说明：
/// - `state`：已经完成初始化的服务状态。
///
/// 返回：稳定服务 UUID、协议范围和实际支持能力；初始化未完成返回 503。
async fn capabilities(Extension(state): Extension<Arc<ServerState>>) -> ApiResult<Response> {
    let store = ready_store(&state)?;
    Ok(Json(serde_json::json!({
        "service_uuid": store.service_id(),
        "protocol_min": 1,
        "protocol_max": 1,
        "authentication": state.config.auth.mode,
        "max_upload_bytes": state.config.data.max_artifact_bytes.unwrap_or(256 * 1024 * 1024),
        "checksum": "sha256",
        "public_base_url": state.config.public_base_url,
        "features": ["immutable-snapshots", "atomic-batches"],
    }))
    .into_response())
}

/// 业务作用：向已认证发布客户端提供稳定主体绑定，不公开账号目录或 token 校验材料。
/// 参数说明：
/// - `principal`：当前请求已核验的 Cargo 身份。
///
/// 返回：主体的不可逆摘要，供中断发布恢复时拒绝另一主体接管原操作。
async fn caller_identity(Extension(principal): Extension<Principal>) -> ApiResult<Response> {
    Ok(Json(serde_json::json!({
        "subject_id": ArtifactDigest::of(principal.subject.as_bytes())
    }))
    .into_response())
}

/// 业务作用：向已认证客户端提供仓库名称到稳定 UUID 的映射。
/// 参数说明：
/// - `state`：当前服务资源。
/// - `principal`：用于筛选可见仓库的当前认证主体。
///
/// 返回：确定性排序的仓库描述，不合并来源身份。
async fn repositories(
    Extension(state): Extension<Arc<ServerState>>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Response> {
    Ok(Json(
        blocking(&state, move |store| {
            Ok(store
                .repositories()?
                .into_iter()
                .filter(|repository| {
                    principal
                        .repositories
                        .iter()
                        .any(|grant| grant == "*" || grant == &repository.id)
                })
                .collect::<Vec<_>>())
        })
        .await?,
    )
    .into_response())
}

/// 业务作用：向已认证业务客户端提供保持独立来源的代理映射。
/// 参数说明：
/// - `state`：当前服务资源。
///
/// 返回：配置启用的代理及稳定身份，不暴露网络回源配置。
async fn proxies(Extension(state): Extension<Arc<ServerState>>) -> ApiResult<Response> {
    let enabled = state
        .config
        .proxies
        .iter()
        .map(|proxy| proxy.name.clone())
        .collect::<std::collections::BTreeSet<_>>();
    Ok(Json(
        blocking(&state, move |store| {
            Ok(store
                .proxies()?
                .into_iter()
                .filter(|proxy| enabled.contains(&proxy.name))
                .collect::<Vec<_>>())
        })
        .await?,
    )
    .into_response())
}

/// 业务作用：让锁定构建确认公开物化证据未被主动撤销。
/// 参数说明：
/// - `state`：当前服务资源。
/// - `id`：工作区持有的集合身份。
///
/// 返回：公开集合仍有效时 valid 为 true，允许团队共享锁文件，不返回集合内容。
async fn materialization_valid(
    Extension(state): Extension<Arc<ServerState>>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let valid = blocking(&state, move |store| store.materialization_valid(&id)).await?;
    Ok(Json(serde_json::json!({
        "valid": valid
    }))
    .into_response())
}

/// 业务作用：创建归属于当前主体的公开解析视图，启用显式刷新或缓存优先策略。
/// 参数说明：
/// - `state`：当前服务资源。
/// - `proxy`：配置启用的公开来源。
/// - `principal`：认证主体。
/// - `request`：解析策略。
///
/// 返回：有界 session 身份与期限；未知来源不创建记录。
async fn begin_proxy_session(
    Extension(state): Extension<Arc<ServerState>>,
    Path(proxy): Path<String>,
    Extension(principal): Extension<Principal>,
    request: Request,
) -> ApiResult<Response> {
    let configuration = state
        .config
        .proxies
        .iter()
        .find(|item| item.name == proxy)
        .ok_or_else(ApiError::not_found)?;
    let request: SessionRequest = decode(request).await?;
    if request.upstream_start >= configuration.upstreams.len() {
        return Err(ApiError::bad("invalid proxy view"));
    }
    if !matches!(request.mode.as_str(), "cache-first" | "revalidate") {
        return Err(ApiError::bad("invalid proxy session mode"));
    }
    Ok((
        StatusCode::CREATED,
        Json(
            blocking(&state, move |store| {
                store.begin_proxy_view(
                    &proxy,
                    &principal.subject,
                    &request.mode,
                    request.upstream_start,
                )
            })
            .await?,
        ),
    )
        .into_response())
}

/// 业务作用：提供同源镜像视图数量，允许客户端有界重试完整依赖图而不解析 Cargo 错误文本。
/// 参数说明：
/// - `state`：当前启用的来源配置。
/// - `proxy`：独立公开来源名称。
///
/// 返回：可创建的视图数量，不暴露上游地址或改变来源身份。
async fn proxy_views(
    Extension(state): Extension<Arc<ServerState>>,
    Path(proxy): Path<String>,
) -> ApiResult<Response> {
    let configuration = state
        .config
        .proxies
        .iter()
        .find(|item| item.name == proxy)
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(serde_json::json!({
        "count": configuration.upstreams.len()
    }))
    .into_response())
}

/// 业务作用：释放当前主体已完成命令的短期索引视图。
/// 参数说明：
/// - `state`：当前服务资源。
/// - `proxy`：公开来源。
/// - `session`：解析视图身份。
/// - `principal`：认证主体。
///
/// 返回：删除成功为 204，不删除长期物化对象。
async fn finish_proxy_session(
    Extension(state): Extension<Arc<ServerState>>,
    Path((proxy, session)): Path<(String, String)>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Response> {
    blocking(&state, move |store| {
        let session = store.proxy_session(&proxy, &session, &principal.subject)?;
        store.finish_proxy_session(&session)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// 业务作用：提供一次解析中固定的公开索引，重读同一名称始终返回首次固定内容。
/// 参数说明：
/// - `state`：当前服务资源。
/// - `proxy`：公开来源名称。
/// - `session`：短期解析身份。
/// - `path`：sparse 相对路径。
/// - `principal`：认证主体。
/// - `headers`：条件请求头。
///
/// 返回：固定 JSONL 或同源下载模板；过期、越权或未知视图拒绝。
async fn proxy_session_index(
    Extension(state): Extension<Arc<ServerState>>,
    Path((proxy, session, path)): Path<(String, String, String)>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let selected_proxy = proxy.clone();
    let session = blocking(&state, move |store| {
        store.proxy_session(&selected_proxy, &session, &principal.subject)
    })
    .await?;
    if path == "config.json" {
        return proxy_index(Extension(state), Path((proxy, path)), headers).await;
    }
    let name = path.rsplit('/').next().ok_or_else(ApiError::not_found)?;
    if naxus_registry::index_path(name).ok().as_deref() != Some(path.as_str()) {
        return Err(ApiError::not_found());
    }
    let body = crate::proxy::session_index(&state, &session, name)
        .await
        .map_err(ApiError::proxy)?;
    cached(body, &headers, true, "application/json")
}

/// 业务作用：在客户端提交锁之前核对并持久化完整公开依赖集合。
/// 参数说明：
/// - `state`：当前服务资源。
/// - `proxy`：独立公开来源。
/// - `session`：本次固定解析身份。
/// - `principal`：认证主体。
/// - `request`：Cargo.lock 中的精确坐标与摘要。
///
/// 返回：所有对象、索引证据都具备的物化身份；任何一项失败不提交集合。
async fn materialize_graph(
    Extension(state): Extension<Arc<ServerState>>,
    Path((proxy, session)): Path<(String, String)>,
    Extension(principal): Extension<Principal>,
    request: Request,
) -> ApiResult<Response> {
    let request: MaterializeRequest = decode(request).await?;
    if request.items.len() > 4096 {
        return Err(ApiError::bad("materialization exceeds coordinate budget"));
    }
    let mut coordinates = std::collections::BTreeSet::new();
    for item in &request.items {
        naxus_core::validate_name(&item.name)
            .map_err(|_| ApiError::bad("invalid materialization name"))?;
        semver::Version::parse(&item.version)
            .map_err(|_| ApiError::bad("invalid materialization version"))?;
        if !coordinates.insert((&item.name, &item.version)) {
            return Err(ApiError::bad("duplicate materialization coordinate"));
        }
    }
    let selected_proxy = proxy.clone();
    let actor = principal.subject.clone();
    let session = blocking(&state, move |store| {
        store.proxy_session(&selected_proxy, &session, &actor)
    })
    .await?;
    for item in &request.items {
        crate::proxy::session_index(&state, &session, &item.name)
            .await
            .map_err(ApiError::proxy)?;
        let digest = crate::proxy::materialize(&state, &proxy, &item.name, &item.version)
            .await
            .map_err(ApiError::proxy)?;
        if digest != item.digest {
            return Err(ApiError::bad(
                "public coordinate checksum differs from index evidence",
            ));
        }
    }
    Ok(Json(
        blocking(&state, move |store| {
            store.commit_materialization(&session, &principal.subject, &request.items)
        })
        .await?,
    )
    .into_response())
}

/// 业务作用：返回本服务下载模板及原始公开索引，使业务主机无需连接上游。
/// 参数说明：
/// - `state`：当前资源和配置。
/// - `proxy`：公开来源名称。
/// - `path`：Cargo sparse 相对路径。
/// - `headers`：缓存和显式刷新请求头。
///
/// 返回：原始索引 JSONL 或本地 config.json；未知来源与路径拒绝。
async fn proxy_index(
    Extension(state): Extension<Arc<ServerState>>,
    Path((proxy, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if !state
        .config
        .proxies
        .iter()
        .any(|configuration| configuration.name == proxy)
    {
        return Err(ApiError::not_found());
    }
    if path == "config.json" {
        let base = state.config.public_base_url.as_str().trim_end_matches('/');
        return encoded(
            &serde_json::json!({
                "dl": format!("{base}/proxies/{proxy}/crates/{{crate}}/{{version}}/download"),
                "auth-required": state.config.auth.mode != "none"
            }),
            &headers,
            false,
        );
    }
    let name = path.rsplit('/').next().ok_or_else(ApiError::not_found)?;
    if naxus_registry::index_path(name).ok().as_deref() != Some(path.as_str()) {
        return Err(ApiError::not_found());
    }
    let force = headers
        .get("x-naxus-revalidate")
        .is_some_and(|value| value == "true");
    let body = crate::proxy::index(&state, &proxy, name, force)
        .await
        .map_err(ApiError::proxy)?;
    cached(body, &headers, false, "application/json")
}

/// 业务作用：将公开依赖先缓存为已校验对象，再由本机文件流返回给 Cargo。
/// 参数说明：
/// - `state`：回源与下载资源预算。
/// - `proxy`：公开来源。
/// - `name`：crate 名称。
/// - `version`：精确版本。
/// - `headers`：对象条件请求头。
///
/// 返回：原始 .crate 字节；同摘要命中持久缓存后不回源。
async fn proxy_download(
    Extension(state): Extension<Arc<ServerState>>,
    Path((proxy, name, version)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let digest = crate::proxy::materialize(&state, &proxy, &name, &version)
        .await
        .map_err(ApiError::proxy)?;
    let permit = state
        .downloads
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::overloaded())?;
    let expected = digest.clone();
    let file = blocking(&state, move |store| {
        ensure!(
            store.proxy_has_object(&proxy, &expected)?,
            naxus_store::RepositoryFailure::NotFound("unknown proxy object")
        );
        store.open_object(&expected)
    })
    .await?;
    file_response(file, digest, headers, permit)
}

/// 业务作用：一次固定仓库 HEAD 与全部已选修订，避免逐坐标查询混入不同时刻。
/// 参数说明：
/// - `state`：受管仓库资源。
/// - `principal`：认证主体。
/// - `request`：协议版本、仓库和精确坐标集合。
///
/// 返回：一致快照与保留租约；协议不兼容时不建立新租约。
async fn resolve(
    Extension(state): Extension<Arc<ServerState>>,
    Extension(principal): Extension<Principal>,
    request: Request,
) -> ApiResult<Response> {
    let request: ResolveRequest = decode(request).await?;
    if request.client_protocol != 1 {
        return Err(ApiError::bad("unsupported client protocol"));
    }
    if request.packages.len() > 4096 {
        return Err(ApiError::bad("too many resolution coordinates"));
    }
    for coordinate in &request.packages {
        naxus_core::validate_name(&coordinate.name)
            .map_err(|_| ApiError::bad("invalid resolution name"))?;
        semver::Version::parse(&coordinate.version)
            .map_err(|_| ApiError::bad("invalid resolution version"))?;
    }
    Ok(Json(
        blocking(&state, move |store| {
            repository_permission(&store, &request.repository, &principal.repositories)?;
            store.resolve_at(
                &request.repository,
                &request.packages,
                &principal.subject,
                request.fixed_snapshot.as_deref(),
            )
        })
        .await?,
    )
    .into_response())
}

/// 业务作用：让持有有效权限的原主体持续保护长时间构建使用的历史快照。
/// 参数说明：
/// - `state`：受管存储资源。
/// - `principal`：当前认证主体。
/// - `repository`：授权仓库名称或 UUID。
/// - `lease`：需要延长保护期的租约 UUID。
///
/// 返回：新的到期时间；失效或不属于当前主体的租约拒绝续期。
async fn renew_lease(
    Extension(state): Extension<Arc<ServerState>>,
    Extension(principal): Extension<Principal>,
    Path((repository, lease)): Path<(String, String)>,
) -> ApiResult<Response> {
    let proof = blocking(&state, move |store| {
        store.renew_lease_proof(&repository, &lease, &principal.subject)
    })
    .await?;
    Ok(Json(proof).into_response())
}

/// 业务作用：主动释放原主体本次构建建立的保留引用。
/// 参数说明：
/// - `state`：受管存储资源。
/// - `principal`：当前认证主体。
/// - `repository`：授权仓库名称或 UUID。
/// - `lease`：当前主体需要释放的租约 UUID。
///
/// 返回：重复释放也返回空成功响应，不影响其他主体的租约。
async fn release_lease(
    Extension(state): Extension<Arc<ServerState>>,
    Extension(principal): Extension<Principal>,
    Path((repository, lease)): Path<(String, String)>,
) -> ApiResult<Response> {
    blocking(&state, move |store| {
        store.release_lease(&repository, &lease, &principal.subject)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// 业务作用：读取当前已完整提交的仓库入口，供客户端固定后续快照。
/// 参数说明：
/// - `state`：当前服务资源。
/// - `repository`：仓库名称或 UUID。
/// - `headers`：条件读取请求头。
///
/// 返回：带强 ETag 的当前快照；未变化返回 304。
async fn head(
    Extension(state): Extension<Arc<ServerState>>,
    Path(repository): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let value = blocking(&state, move |store| store.head(&repository)).await?;
    encoded(&value, &headers, false)
}

/// 业务作用：核对归属后读取不可变快照，不回退到当前 HEAD。
/// 参数说明：
/// - `state`：当前服务资源。
/// - `repository`：已授权的仓库。
/// - `snapshot`：固定快照 ID。
/// - `headers`：条件读取头。
///
/// 返回：不可变描述或 304；错误归属不开放读取。
async fn snapshot(
    Extension(state): Extension<Arc<ServerState>>,
    Path((repository, snapshot)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let value = blocking(&state, move |store| store.snapshot(&repository, &snapshot)).await?;
    encoded(&value, &headers, true)
}

/// 业务作用：在读取制品前创建固定目标和主体的持久上传批次。
/// 参数说明：
/// - `state`：服务资源。
/// - `repository`：仓库名称或 UUID。
/// - `principal`：认证流水线固定的主体。
/// - `request`：有界 JSON 请求。
///
/// 返回：持久批次；并发 HEAD 或幂等键冲突不开始上传。
async fn begin_batch(
    Extension(state): Extension<Arc<ServerState>>,
    Path(repository): Path<String>,
    Extension(principal): Extension<Principal>,
    request: Request,
) -> ApiResult<Response> {
    let body: BeginBatch = decode(request).await?;
    if body.idempotency_key.is_empty() || body.idempotency_key.len() > 256 {
        return Err(ApiError::bad("invalid publish identity"));
    }
    let batch = blocking(&state, move |store| {
        store.begin_batch(
            &repository,
            &principal.subject,
            &body.expected_head,
            &body.idempotency_key,
        )
    })
    .await?;
    Ok((StatusCode::CREATED, Json(batch)).into_response())
}

/// 业务作用：读取仅属于当前主体和仓库的上传状态。
/// 参数说明：
/// - `state`：服务资源。
/// - `repository`：仓库身份。
/// - `batch`：上传批次 ID。
/// - `principal`：当前认证主体。
///
/// 返回：可恢复的持久状态，不泄漏其他主体的批次。
async fn batch(
    Extension(state): Extension<Arc<ServerState>>,
    Path((repository, batch)): Path<(String, String)>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Response> {
    Ok(Json(
        blocking(&state, move |store| {
            store.batch(&repository, &batch, &principal.subject)
        })
        .await?,
    )
    .into_response())
}

/// 业务作用：停止尚未提交的上传，不删除正式修订或逆转已提交 HEAD。
/// 参数说明：
/// - `state`：服务资源。
/// - `repository`：仓库身份。
/// - `batch`：目标批次。
/// - `principal`：认证主体。
///
/// 返回：终止后的持久状态；已提交批次仍报告原终态。
async fn abort_batch(
    Extension(state): Extension<Arc<ServerState>>,
    Path((repository, batch)): Path<(String, String)>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Response> {
    Ok(Json(
        blocking(&state, move |store| {
            store.abort_batch(&repository, &batch, &principal.subject)
        })
        .await?,
    )
    .into_response())
}

/// 业务作用：按 Cargo 原生长度帧接收单包发布，完整静态校验后原子发布新快照。
/// 参数说明：
/// - `state`：唯一服务与并发上传预算。
/// - `repository`：已授权的 hosted 仓库。
/// - `principal`：具有发布权限的认证主体。
/// - `request`：Cargo 元数据及原始归档流。
///
/// 返回：Cargo warnings 对象；帧长度、元数据、依赖闭包或 HEAD 冲突时不公开部分发布。
async fn cargo_publish(
    Extension(state): Extension<Arc<ServerState>>,
    Path(repository): Path<String>,
    Extension(principal): Extension<Principal>,
    request: Request,
) -> ApiResult<Response> {
    let expected_head = blocking(&state, {
        let repository = repository.clone();
        move |store| store.head(&repository)
    })
    .await?;
    let permit = state
        .uploads
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::overloaded())?;
    let store = ready_store(&state)?;
    let maximum = state
        .config
        .data
        .max_artifact_bytes
        .unwrap_or(256 * 1024 * 1024);
    let base = state
        .config
        .public_base_url
        .as_str()
        .trim_end_matches('/')
        .to_owned();
    let proxy_sources = state
        .config
        .proxies
        .iter()
        .map(|proxy| {
            naxus_core::registry_identity(&proxy.logical_index).expect("validated proxy source")
        })
        .collect::<std::collections::BTreeSet<_>>();
    let body = request.into_body().into_data_stream();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    let stream = futures_util::stream::unfold(Some(body), move |body| async move {
        let mut body = body?;
        match tokio::time::timeout_at(deadline, body.next()).await {
            Ok(Some(Ok(bytes))) => Some((Ok::<_, io::Error>(bytes), Some(body))),
            Ok(Some(Err(error))) => Some((Err(io::Error::other(error)), None)),
            Ok(None) => None,
            Err(_) => Some((
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "upload deadline exceeded",
                )),
                None,
            )),
        }
    });
    let mut reader = SyncIoBridge::new(StreamReader::new(Box::pin(stream)));
    tokio::task::spawn_blocking(move || -> Result<(), ApiError> {
        use std::io::Read;
        let _permit = permit;
        let info = store
            .repositories()
            .map_err(ApiError::backend)?
            .into_iter()
            .find(|item| item.id == expected_head.repository_id)
            .ok_or_else(ApiError::not_found)?;
        // 归档语义失败与提交前置条件属于不同协议阶段；后者不能被解释为包体损坏。
        let entry = (|| -> anyhow::Result<_> {
            let metadata_length = read_frame_length(&mut reader)?;
            ensure!(
                metadata_length <= 4 * 1024 * 1024,
                "publish metadata exceeds limit"
            );
            let mut metadata = vec![0; metadata_length as usize];
            reader.read_exact(&mut metadata)?;
            let metadata: naxus_registry::PublishMetadata = serde_json::from_slice(&metadata)?;
            naxus_core::validate_name(&metadata.name)?;
            semver::Version::parse(&metadata.vers)?;
            let length = read_frame_length(&mut reader)?;
            ensure!(
                length > 0 && length as u64 <= maximum,
                "artifact exceeds upload limit"
            );
            let logical = format!("sparse+{base}/repositories/{}/index/", info.name);
            let uuid_alias = format!("sparse+{base}/repositories/{}/index/", info.id);
            // 多读取一个字节即可在暂存阶段拒绝尾随帧；不足声明长度同样不能提升为正式对象。
            let (_, entry) = store.install_validated(
                &mut (&mut reader).take(length as u64 + 1),
                None,
                |file, object| {
                    ensure!(
                        object.size == length as u64,
                        "Cargo publish frame length mismatch"
                    );
                    let mut entry = naxus_registry::inspect_archive(
                        file,
                        &metadata.name,
                        &metadata.vers,
                        object.sha256.clone(),
                        &logical,
                        &naxus_registry::ArchiveLimits::default(),
                    )?;
                    metadata.validate_archive(&entry, &[logical, uuid_alias.clone()])?;
                    for dependency in &mut entry.deps {
                        if dependency.registry.as_deref() == Some(uuid_alias.as_str()) {
                            dependency.registry = None;
                        }
                        // 原生发布不能绕过单入口来源约束，归档有效也不足以证明依赖来源可被业务安全消费。
                        ensure!(
                            dependency
                                .registry
                                .as_ref()
                                .is_none_or(|source| naxus_core::registry_identity(source)
                                    .is_ok_and(|identity| proxy_sources.contains(&identity))),
                            "dependency registry has no configured Naxus proxy mapping"
                        );
                    }
                    Ok(entry)
                },
            )?;
            Ok(entry)
        })()
        .map_err(ApiError::upload)?;
        let actor = principal.subject;
        let batch = store
            .begin_batch(
                &repository,
                &actor,
                &expected_head.id,
                &Uuid::new_v4().to_string(),
            )
            .map_err(ApiError::backend)?;
        let outcome = store
            .add_batch_item(&repository, &batch.id, &actor, &entry)
            .and_then(|_| {
                store.commit_batch_authorized(
                    &repository,
                    &batch.id,
                    &actor,
                    principal.token_digest.as_ref(),
                )
            });
        if outcome.is_err() {
            let _ = store.abort_batch(&repository, &batch.id, &actor);
        }
        outcome.map_err(ApiError::backend)?;
        Ok(())
    })
    .await
    .map_err(|_| ApiError::unavailable())??;
    Ok(Json(serde_json::json!({
        "warnings": {
            "invalid_categories": [],
            "invalid_badges": [],
            "other": []
        }
    }))
    .into_response())
}

/// 业务作用：按 Cargo 协议读取固定长度声明，短帧不能被当作空归档。
/// 参数说明：
/// - `reader`：受超时与并发预算保护的上传流。
///
/// 返回：四字节 little-endian 长度，EOF 或 I/O 失败明确拒绝。
fn read_frame_length(reader: &mut dyn io::Read) -> anyhow::Result<u32> {
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    Ok(u32::from_le_bytes(length))
}

/// 业务作用：按 Cargo API 撤下指定版本的新解析资格，历史快照与对象保持可读。
/// 参数说明：
/// - `state`：受管存储。
/// - `path`：仓库、包名与版本。
/// - `principal`：已授权发布主体。
///
/// 返回：新快照提交完成后返回 ok，不执行物理删除。
async fn cargo_yank(
    Extension(state): Extension<Arc<ServerState>>,
    Path(path): Path<(String, String, String)>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Response> {
    change_yank(&state, path, principal, true).await
}

/// 业务作用：按 Cargo API 恢复指定版本的后续解析资格。
/// 参数说明：
/// - `state`：受管存储。
/// - `path`：仓库、包名与版本。
/// - `principal`：已授权发布主体。
///
/// 返回：新快照提交完成后返回 ok，已恢复版本重复请求复用 HEAD。
async fn cargo_unyank(
    Extension(state): Extension<Arc<ServerState>>,
    Path(path): Path<(String, String, String)>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Response> {
    change_yank(&state, path, principal, false).await
}

/// 业务作用：以预期 HEAD 提交单坐标的可选性变化，避免并发发布被覆盖。
/// 参数说明：
/// - `state`：受管存储。
/// - `path`：精确仓库和坐标。
/// - `principal`：认证主体。
/// - `yanked`：是否拒绝后续正常解析选择。
///
/// 返回：原子提交后返回 Cargo ok；坐标不存在、对象失效或并发 HEAD 变化时拒绝。
async fn change_yank(
    state: &Arc<ServerState>,
    path: (String, String, String),
    principal: Principal,
    yanked: bool,
) -> ApiResult<Response> {
    let (repository, name, version) = path;
    naxus_core::validate_name(&name).map_err(|_| ApiError::bad("invalid package name"))?;
    semver::Version::parse(&version).map_err(|_| ApiError::bad("invalid package version"))?;
    blocking(state, move |store| {
        let head = store.head(&repository)?;
        let mut entry = store
            .index_entries(&head.id, &name)?
            .into_iter()
            .find(|entry| entry.vers == version)
            .context(naxus_store::RepositoryFailure::NotFound(
                "unknown package version",
            ))?;
        entry.yanked = yanked;
        store.commit_authorized(
            &naxus_store::CommitRequest {
                repository,
                expected_head: head.id,
                idempotency_key: Uuid::new_v4().to_string(),
                actor: principal.subject,
                entries: vec![entry],
            },
            principal.token_digest.as_ref(),
        )
    })
    .await?;
    Ok(Json(serde_json::json!({
        "ok": true
    }))
    .into_response())
}

/// 业务作用：流式保存并检查精确制品，仅向已授权开放批次登记通过检查的元数据。
/// 参数说明：
/// - `state`：服务资源与上传预算。
/// - `repository`：批次所属仓库。
/// - `batch`：持久批次 ID。
/// - `name`：真实 package 名称。
/// - `version`：精确版本。
/// - `principal`：认证主体。
/// - `request`：含长度、摘要与原始 `.crate` 的请求。
///
/// 返回：已校验索引元数据；任一中断不改变 HEAD，不开放未提交对象下载。
async fn upload(
    Extension(state): Extension<Arc<ServerState>>,
    Path((repository, batch, name, version)): Path<(String, String, String, String)>,
    Extension(principal): Extension<Principal>,
    request: Request,
) -> ApiResult<Response> {
    let digest: ArtifactDigest = request
        .headers()
        .get("x-checksum-sha256")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::bad("missing SHA-256 declaration"))?
        .parse()
        .map_err(|_| ApiError::bad("invalid SHA-256 declaration"))?;
    let length: u64 = request
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| ApiError::bad("missing Content-Length"))?;
    if length
        > state
            .config
            .data
            .max_artifact_bytes
            .unwrap_or(256 * 1024 * 1024)
    {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "artifact_too_large",
            "artifact exceeds upload limit",
        ));
    }
    let actor = principal.subject;
    let check_repository = repository.clone();
    let check_batch = batch.clone();
    let check_actor = actor.clone();
    let before = blocking(&state, move |store| {
        store.batch(&check_repository, &check_batch, &check_actor)
    })
    .await?;
    if before.state != "open" {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "batch_not_open",
            "batch no longer accepts uploads",
        ));
    }
    let permit = state
        .uploads
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::overloaded())?;
    let store = ready_store(&state)?;
    let base = state
        .config
        .public_base_url
        .as_str()
        .trim_end_matches('/')
        .to_owned();
    let body = request.into_body().into_data_stream();
    let proxy_sources = state
        .config
        .proxies
        .iter()
        .map(|proxy| {
            naxus_core::registry_identity(&proxy.logical_index).expect("validated proxy source")
        })
        .collect::<std::collections::BTreeSet<_>>();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    let stream = futures_util::stream::unfold(Some(body), move |body| async move {
        let mut body = body?;
        match tokio::time::timeout_at(deadline, body.next()).await {
            Ok(Some(Ok(bytes))) => Some((Ok::<_, io::Error>(bytes), Some(body))),
            Ok(Some(Err(error))) => Some((Err(io::Error::other(error)), None)),
            Ok(None) => None,
            Err(_) => Some((
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "upload deadline exceeded",
                )),
                None,
            )),
        }
    });
    let mut reader = SyncIoBridge::new(StreamReader::new(Box::pin(stream)));
    let entry = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let _permit = permit;
        let repository_info = store
            .repositories()?
            .into_iter()
            .find(|item| item.id == before.repository_id)
            .context(naxus_store::RepositoryFailure::NotFound(
                "unknown repository",
            ))?;
        let logical = format!("sparse+{base}/repositories/{}/index/", repository_info.name);
        let (_, entry) = store.install_validated(&mut reader, Some(&digest), |file, object| {
            ensure!(
                object.size == length,
                "upload length differs from Content-Length"
            );
            let mut entry = naxus_registry::inspect_archive(
                file,
                &name,
                &version,
                digest.clone(),
                &logical,
                &naxus_registry::ArchiveLimits::default(),
            )?;
            let uuid_alias = format!("sparse+{base}/repositories/{}/index/", repository_info.id);
            for dependency in &mut entry.deps {
                if dependency.registry.as_deref() == Some(uuid_alias.as_str()) {
                    dependency.registry = None;
                }
                // 未配置代理映射的独立 registry 会让 Cargo 绕过统一入口，必须在制品可见前拒绝。
                ensure!(
                    dependency
                        .registry
                        .as_deref()
                        .is_none_or(|source| naxus_core::registry_identity(source)
                            .is_ok_and(|identity| proxy_sources.contains(&identity))),
                    "dependency registry has no configured Naxus proxy mapping"
                );
            }
            Ok(entry)
        })?;
        store.add_batch_item(&repository, &batch, &actor, &entry)?;
        Ok(entry)
    })
    .await
    .map_err(|_| ApiError::unavailable())?
    .map_err(ApiError::upload)?;
    Ok((StatusCode::CREATED, Json(entry)).into_response())
}

/// 业务作用：提交已封闭批次，使全部修订与终态一起可见。
/// 参数说明：
/// - `state`：服务资源。
/// - `repository`：批次所属仓库。
/// - `batch`：目标批次。
/// - `principal`：认证主体。
///
/// 返回：完整提交的不可变快照；失败不出现部分 HEAD。
async fn commit_batch(
    Extension(state): Extension<Arc<ServerState>>,
    Path((repository, batch)): Path<(String, String)>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Response> {
    Ok(Json(
        blocking(&state, move |store| {
            store.commit_batch_authorized(
                &repository,
                &batch,
                &principal.subject,
                principal.token_digest.as_ref(),
            )
        })
        .await?,
    )
    .into_response())
}

/// 业务作用：兼容读取逻辑入口，并在单次请求内固定当前快照。
/// 参数说明：
/// - `state`：服务资源。
/// - `repository`：仓库身份。
/// - `path`：sparse 相对路径。
/// - `headers`：条件读取头。
///
/// 返回：本次已固定快照的索引；逻辑入口不设置不可变长期缓存。
async fn logical_index(
    Extension(state): Extension<Arc<ServerState>>,
    Path((repository, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    index(state, repository, None, path, headers).await
}

/// 业务作用：读取明确指定的历史 sparse 索引，不查询后来的 HEAD。
/// 参数说明：
/// - `state`：服务资源。
/// - `repository`：仓库身份。
/// - `snapshot`：不可变快照。
/// - `path`：sparse 相对路径。
/// - `headers`：条件读取头。
///
/// 返回：固定索引或 304；未知或错误归属不回退。
async fn immutable_index(
    Extension(state): Extension<Arc<ServerState>>,
    Path((repository, snapshot, path)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    index(state, repository, Some(snapshot), path, headers).await
}

/// 业务作用：统一生成受归属校验的 config.json 和 sparse JSONL，保持 Cargo 的原始协议。
/// 参数说明：
/// - `state`：当前资源。
/// - `repository`：仓库身份。
/// - `snapshot`：固定快照；None 表示本次从逻辑 HEAD 固定。
/// - `path`：完整 sparse 相对路径。
/// - `headers`：条件读取头。
///
/// 返回：经过摘要缓存标记的协议响应；非法 sparse 路径按不存在处理。
async fn index(
    state: Arc<ServerState>,
    repository: String,
    snapshot: Option<String>,
    path: String,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let immutable = snapshot.is_some();
    let base = state
        .config
        .public_base_url
        .as_str()
        .trim_end_matches('/')
        .to_owned();
    let auth_required = state.config.auth.mode != "none";
    let body = blocking(&state, move |store| {
        let selected = match snapshot {
            Some(snapshot) => store.snapshot(&repository, &snapshot)?,
            None => store.head(&repository)?,
        };
        if path == "config.json" {
            // Cargo 会缓存逻辑入口的 config.json；其下载模板不能绑到首次读取的 HEAD，否则后来新增版本无法下载。
            let download = if immutable {
                format!("{base}/repositories/{repository}/snapshots/{}/crates/{{crate}}/{{version}}/{{sha256-checksum}}", selected.id)
            } else {
                format!("{base}/repositories/{repository}/crates/{{crate}}/{{version}}/{{sha256-checksum}}")
            };
            let mut configuration = serde_json::json!({
                "dl": download,
                "auth-required": auth_required,
            });
            if !immutable {
                configuration["api"] = serde_json::json!(format!("{base}/repositories/{repository}"));
            }
            return Ok(serde_json::to_vec(&configuration)?);
        }
        let name = path.rsplit('/').next().context(naxus_store::RepositoryFailure::NotFound("invalid sparse path"))?;
        ensure!(
            naxus_registry::index_path(name)? == path,
            naxus_store::RepositoryFailure::NotFound("invalid sparse path")
        );
        let entries = store.index_entries(&selected.id, name)?;
        ensure!(!entries.is_empty(), naxus_store::RepositoryFailure::NotFound("unknown package"));
        let mut bytes = Vec::new();
        for entry in entries {
            serde_json::to_writer(&mut bytes, &entry)?;
            bytes.push(b'\n');
        }
        Ok(bytes)
    })
    .await?;
    cached(body, &headers, immutable, "application/json")
}

/// 业务作用：为原生 Cargo 的稳定逻辑入口下载当前坐标，避免共享字节的旧修订撤销误伤当前版本。
/// 参数说明：
/// - `state`：受管资源和下载预算。
/// - `repository`：目标 hosted 仓库名称或 UUID。
/// - `name`：需要下载的 Cargo 包名。
/// - `version`：需要下载的精确版本。
/// - `digest`：客户端索引声明的包体摘要。
/// - `headers`：条件读取头。
///
/// 返回：当前坐标仍选择相同摘要且修订有效时流式读取；固定历史另由 snapshot 路由裁决。
async fn current_object(
    Extension(state): Extension<Arc<ServerState>>,
    Path((repository, name, version, digest)): Path<(String, String, String, ArtifactDigest)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let permit = state
        .downloads
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::overloaded())?;
    let expected = digest.clone();
    let file = blocking(&state, move |store| {
        let head = store.head(&repository)?;
        store.open_snapshot_object(
            &repository,
            &head.id,
            &naxus_core::Coordinate {
                name,
                version,
            },
            &expected,
        )
    })
    .await?;
    file_response(file, digest, headers, permit)
}

/// 业务作用：只下载仓库已提交的精确对象，并在响应结束前持有下载预算。
/// 参数说明：
/// - `state`：资源与下载预算。
/// - `repository`：已授权仓库。
/// - `digest`：精确对象摘要。
/// - `headers`：条件读取头。
///
/// 返回：不可变二进制流或 304；未提交、缺失或串仓对象不开放读取。
async fn object(
    Extension(state): Extension<Arc<ServerState>>,
    Path((repository, digest)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let digest: ArtifactDigest = digest.parse().map_err(|_| ApiError::not_found())?;
    let permit = state
        .downloads
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::overloaded())?;
    let expected = digest.clone();
    let file = blocking(&state, move |store| {
        store.open_repository_object(&repository, &expected)
    })
    .await?;
    file_response(file, digest, headers, permit)
}

/// 业务作用：按索引模板绑定的快照与坐标下载精确修订，永久撤销不会被同摘要新代次掩盖。
/// 参数说明：
/// - `state`：读取预算和存储。
/// - `repository`：已授权仓库。
/// - `snapshot`：不可变快照身份。
/// - `name`：crate 名称。
/// - `version`：精确版本。
/// - `digest`：索引声明的对象摘要。
/// - `headers`：条件请求头。
///
/// 返回：受保护原始包体，历史撤销明确返回 410。
async fn snapshot_object(
    Extension(state): Extension<Arc<ServerState>>,
    Path((repository, snapshot, name, version, digest)): Path<(
        String,
        String,
        String,
        String,
        String,
    )>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let digest: ArtifactDigest = digest.parse().map_err(|_| ApiError::not_found())?;
    let expected = digest.clone();
    let permit = state
        .downloads
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::overloaded())?;
    let file = blocking(&state, move |store| {
        store.open_snapshot_object(
            &repository,
            &snapshot,
            &naxus_core::Coordinate {
                name,
                version,
            },
            &expected,
        )
    })
    .await?;
    file_response(file, digest, headers, permit)
}

/// 业务作用：发送已验证对象并持有下载预算直到文件流结束或连接取消。
/// 参数说明：
/// - `file`：已经完成对象校验的文件句柄。
/// - `digest`：响应不可变身份。
/// - `headers`：条件请求头。
/// - `permit`：活动下载预算。
///
/// 返回：带长度与强 ETag 的二进制响应或 304。
fn file_response(
    file: naxus_store::ObjectReader,
    digest: ArtifactDigest,
    headers: HeaderMap,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> ApiResult<Response> {
    let etag = format!("\"{digest}\"");
    if headers
        .get("if-none-match")
        .is_some_and(|value| value.as_bytes() == etag.as_bytes())
    {
        return Ok(StatusCode::NOT_MODIFIED.into_response());
    }
    let size = file.metadata().map_err(|_| ApiError::unavailable())?.len();
    let (file, lease) = file.into_parts();
    let stream = ReaderStream::new(tokio::fs::File::from_std(file));
    let stream = futures_util::stream::unfold(
        (stream, permit, lease),
        |(mut stream, permit, lease)| async move {
            stream
                .next()
                .await
                .map(|chunk| (chunk, (stream, permit, lease)))
        },
    );
    let mut response = Body::from_stream(stream).into_response();
    response.headers_mut().insert(
        "content-type",
        "application/octet-stream".parse().expect("static header"),
    );
    response.headers_mut().insert(
        "content-length",
        size.to_string().parse().expect("numeric header"),
    );
    response
        .headers_mut()
        .insert("etag", etag.parse().expect("digest header"));
    response.headers_mut().insert(
        "cache-control",
        "private, max-age=31536000, immutable"
            .parse()
            .expect("static header"),
    );
    Ok(response)
}

/// 业务作用：把同步文件和数据库工作放入有界阻塞域，取消请求后仍保留任务预算直到结束。
/// 参数说明：
/// - `state`：当前预算和受管存储。
/// - `operation`：不执行网络等待的单次存储操作。
///
/// 返回：任务完成结果；过载立即拒绝，不建立无界阻塞队列。
pub(crate) async fn blocking<T: Send + 'static>(
    state: &Arc<ServerState>,
    operation: impl FnOnce(Arc<naxus_store::Store>) -> anyhow::Result<T> + Send + 'static,
) -> ApiResult<T> {
    storage(state, operation).await.map_err(ApiError::backend)
}

/// 业务作用：在有界阻塞域内保留领域错误链，供代理阶段使用同一存在性和资源分类。
/// 参数说明：
/// - `state`：当前预算和唯一存储资源。
/// - `operation`：不执行网络等待的单次存储操作。
///
/// 返回：完整领域结果；过载为类型化配额错误，未就绪或任务异常不创建临时资源。
pub(crate) async fn storage<T: Send + 'static>(
    state: &Arc<ServerState>,
    operation: impl FnOnce(Arc<naxus_store::Store>) -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    let permit = state
        .work
        .clone()
        .try_acquire_owned()
        .map_err(|_| naxus_store::ResourceLimitExceeded("repository work budget exceeded"))?;
    // 资源只由初始化阶段发布；未就绪时不能绕过生命周期重新打开同一数据根。
    let store = state
        .store
        .get()
        .cloned()
        .context("repository unavailable")?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation(store)
    })
    .await
    .context("repository task unavailable")?
}

/// 业务作用：拒绝初始化尚未发布的仓库访问，避免路由自行创建第二个资源实例。
/// 参数说明：
/// - `state`：受管共享状态。
///
/// 返回：已完成初始化的唯一存储句柄，否则 503。
pub(crate) fn ready_store(state: &ServerState) -> ApiResult<Arc<naxus_store::Store>> {
    state.store.get().cloned().ok_or_else(ApiError::unavailable)
}

/// 业务作用：为管理元数据限定 JSON 内存预算，不把制品大小预算用于控制请求。
/// 参数说明：
/// - `request`：原始入站请求。
///
/// 返回：有界且可反序列化的控制请求；过大或非法结构拒绝。
async fn decode<T: DeserializeOwned>(request: Request) -> ApiResult<T> {
    let body = to_bytes(request.into_body(), 1024 * 1024)
        .await
        .map_err(|_| ApiError::bad("invalid or oversized JSON body"))?;
    serde_json::from_slice(&body).map_err(|_| ApiError::bad("invalid JSON request"))
}

/// 业务作用：确定性序列化控制元数据后使用内容 ETag。
/// 参数说明：
/// - `value`：不包含凭据的响应对象。
/// - `headers`：条件读取头。
/// - `immutable`：是否属于不可变快照。
///
/// 返回：JSON 原始响应或 304，序列化失败不返回半正文。
fn encoded(value: &impl Serialize, headers: &HeaderMap, immutable: bool) -> ApiResult<Response> {
    cached(
        serde_json::to_vec(value).map_err(|_| ApiError::unavailable())?,
        headers,
        immutable,
        "application/json",
    )
}

/// 业务作用：按精确响应字节应用缓存合同，错误与逻辑 HEAD 不使用长期不可变缓存。
/// 参数说明：
/// - `body`：完整响应字节。
/// - `headers`：条件读取头。
/// - `immutable`：不可变读取标志。
/// - `content_type`：协议正文类型。
///
/// 返回：带 ETag 和适当缓存头的原始 HTTP 响应。
fn cached(
    body: Vec<u8>,
    headers: &HeaderMap,
    immutable: bool,
    content_type: &'static str,
) -> ApiResult<Response> {
    let etag = format!("\"{}\"", ArtifactDigest::of(&body));
    let mut response = if headers
        .get("if-none-match")
        .is_some_and(|value| value.as_bytes() == etag.as_bytes())
    {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        ([("content-type", content_type)], body).into_response()
    };
    response
        .headers_mut()
        .insert("etag", etag.parse().expect("digest header"));
    response.headers_mut().insert(
        "cache-control",
        if immutable {
            "private, max-age=31536000, immutable"
        } else {
            "no-cache"
        }
        .parse()
        .expect("static header"),
    );
    Ok(response)
}

impl ApiError {
    /// 业务作用：区分公开坐标缺失与回源故障，不将上游失败误报为不存在。
    /// 参数说明：
    /// - `error`：代理的有界回源结果。
    ///
    /// 返回：明确缺失为 404，上游认证和授权拒绝以独立 502 错误码表达，其余回源失败为 502；内部 URL 不进入响应。
    fn proxy(error: anyhow::Error) -> Self {
        if error
            .downcast_ref::<naxus_store::RepositoryFailure>()
            .is_some()
            || error
                .downcast_ref::<naxus_store::ResourceLimitExceeded>()
                .is_some()
        {
            Self::backend(error)
        } else if error.downcast_ref::<crate::proxy::NotFound>().is_some() {
            Self::not_found()
        } else if matches!(
            error.downcast_ref::<crate::upstream::Failure>(),
            Some(crate::upstream::Failure::Unauthorized)
        ) {
            Self::new(
                StatusCode::BAD_GATEWAY,
                "upstream_unauthorized",
                "public registry upstream authentication rejected",
            )
        } else if matches!(
            error.downcast_ref::<crate::upstream::Failure>(),
            Some(crate::upstream::Failure::Forbidden)
        ) {
            Self::new(
                StatusCode::BAD_GATEWAY,
                "upstream_forbidden",
                "public registry upstream authorization rejected",
            )
        } else {
            Self::new(
                StatusCode::BAD_GATEWAY,
                "upstream_unavailable",
                "public registry upstream unavailable",
            )
        }
    }
    /// 业务作用：构造稳定且不回显内部内容的协议错误。
    /// 参数说明：
    /// - `status`：真实 HTTP 状态。
    /// - `code`：稳定错误类别。
    /// - `message`：不包含请求正文或路径的提示。
    ///
    /// 返回：由 Cargo 和管理客户端共同理解的错误描述。
    pub(crate) fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
        }
    }

    /// 业务作用：生成认证挑战，避免未授权请求进入存储层。
    /// 参数说明: 无。
    ///
    /// 返回：HTTP 401 错误。
    fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "repository authentication required",
        )
    }

    /// 业务作用：拒绝无效或无法解释的控制输入。
    /// 参数说明：
    /// - `message`：预定义的安全提示。
    ///
    /// 返回：HTTP 400 错误。
    fn bad(message: &'static str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }

    /// 业务作用：统一不存在的仓库、包或对象响应。
    /// 参数说明: 无。
    ///
    /// 返回：不暴露其他仓库存在性的 HTTP 404。
    fn not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "repository resource not found",
        )
    }

    /// 业务作用：阻止不完整资源或阻塞任务异常继续受理业务。
    /// 参数说明: 无。
    ///
    /// 返回：HTTP 503，调用方不能据此认为发布已提交。
    fn unavailable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "repository_unavailable",
            "repository is temporarily unavailable",
        )
    }

    /// 业务作用：在有界预算耗尽时立即拒绝，避免连接与任务无界增长。
    /// 参数说明: 无。
    ///
    /// 返回：HTTP 429 过载响应。
    fn overloaded() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "repository_overloaded",
            "repository operation budget exceeded",
        )
    }

    /// 业务作用：把存储失败映射为稳定协议分类，内部诊断仅进入服务日志。
    /// 参数说明：
    /// - `error`：同步操作的原始错误链。
    ///
    /// 返回：固定批次过大为 413，受管资源配额为 429；其余保留权限、冲突及存储分类，不回显底层 SQL 和路径。
    fn backend(error: anyhow::Error) -> Self {
        tracing::warn!(reason = %error, "repository operation rejected");
        // 错误链保留领域类型，用户输入和本地诊断都不能改变公开协议分类。
        if let Some(failure) = error.downcast_ref::<naxus_store::RepositoryFailure>() {
            return match failure {
                naxus_store::RepositoryFailure::InvalidRequest(_) => {
                    Self::bad("invalid repository request")
                }
                naxus_store::RepositoryFailure::NotFound(_) => Self::not_found(),
                naxus_store::RepositoryFailure::Conflict(_) => Self::new(
                    StatusCode::CONFLICT,
                    "repository_conflict",
                    "repository state or publish precondition conflicts",
                ),
                naxus_store::RepositoryFailure::Forbidden(_) => Self::new(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "resource is outside the authorized scope",
                ),
                naxus_store::RepositoryFailure::InsufficientStorage => Self::new(
                    StatusCode::INSUFFICIENT_STORAGE,
                    "insufficient_storage",
                    "repository storage capacity is exhausted",
                ),
            };
        }
        // 固定发布上限要求改变发布意图，不能让客户端把同一输入当作可重试限流。
        if error
            .downcast_ref::<naxus_store::PublishBatchTooLarge>()
            .is_some()
        {
            return Self::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "batch_too_large",
                "publish batch exceeds fixed limits; reduce the batch and start a new publication",
            );
        }
        if error
            .downcast_ref::<naxus_store::ResourceLimitExceeded>()
            .is_some()
        {
            return Self::overloaded();
        }
        if error.downcast_ref::<naxus_store::ArtifactGone>().is_some() {
            return Self::new(
                StatusCode::GONE,
                "artifact_cleared",
                "artifact revision was permanently cleared",
            );
        }
        if error
            .downcast_ref::<rusqlite::Error>()
            .is_some_and(|error| matches!(error, rusqlite::Error::QueryReturnedNoRows))
        {
            Self::not_found()
        } else {
            Self::unavailable()
        }
    }

    /// 业务作用：区分上传容量和归档静态校验失败，不将错误字节声明为可发布制品。
    /// 参数说明：
    /// - `error`：流式保存或归档检查错误。
    ///
    /// 返回：类型化的容量、配额、固定上限和仓库拒绝保留分类，其他制品错误返回 422。
    fn upload(error: anyhow::Error) -> Self {
        if error
            .downcast_ref::<naxus_store::PublishBatchTooLarge>()
            .is_some()
            || error
                .downcast_ref::<naxus_store::ResourceLimitExceeded>()
                .is_some()
            || error
                .downcast_ref::<naxus_store::RepositoryFailure>()
                .is_some()
        {
            return Self::backend(error);
        }
        tracing::warn!(reason = %error, "artifact validation rejected");
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_artifact",
            "artifact checksum, archive or manifest validation failed",
        )
    }
}

impl ApiError {
    /// 业务作用：保留真实 HTTP 失败状态，并提供 Cargo 规定的 errors.detail。
    /// 参数说明：
    /// - `cargo_challenge`：是否为 registry 认证声明 Cargo challenge；浏览器会话使用 false。
    ///
    /// 返回：正文与响应头关联 ID 一致且禁止缓存的错误响应，按需附加 Cargo 认证挑战。
    pub(crate) fn response(self, cargo_challenge: bool) -> Response {
        let request_id = RESPONSE_REQUEST_ID
            .try_with(Clone::clone)
            .unwrap_or_else(|_| Uuid::new_v4().to_string());
        let mut response = (
            self.status,
            Json(serde_json::json!({
                "code": self.code,
                "message": self.message,
                "request_id": request_id,
                "errors": [{
                    "detail": self.message
                }],
            })),
        )
            .into_response();
        response.headers_mut().insert(
            "x-request-id",
            request_id.parse().expect("validated request id header"),
        );
        response
            .headers_mut()
            .insert("cache-control", "no-store".parse().expect("static header"));
        response.extensions_mut().insert(ErrorResponse);
        if cargo_challenge && self.status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert("www-authenticate", "Cargo".parse().expect("static header"));
        }
        response
    }
}

impl IntoResponse for ApiError {
    /// 业务作用：为 registry 与扩展协议输出同一错误体，保留 Cargo 认证挑战。
    /// 参数说明: 无。
    ///
    /// 返回：真实状态、稳定错误码、相同的正文及响应头请求 ID，不回显底层诊断。
    fn into_response(self) -> Response {
        self.response(true)
    }
}
