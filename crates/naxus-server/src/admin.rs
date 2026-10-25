use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::Path,
    sync::Arc,
    time::Duration,
};

use anyhow::{ensure, Context, Result};
use axum::{body::to_bytes, extract::Query, http::header::SET_COOKIE};
use nasa::{
    application::{
        AuthzDecision, PolicyDecisionSnapshot, PolicySet, Principal, RequireMode, RoutePolicy,
        UnmatchedRoutePolicy,
    },
    web::{
        from_fn_with_state, get, post, Extension, HeaderMap, IntoResponse, Json, Next,
        Path as RoutePath, Request, Response, State, StatusCode,
    },
};
use naxus_core::ArtifactDigest;
use naxus_store::{AccessIdentity, Store};
use serde::{de::DeserializeOwned, Deserialize};
use zeroize::Zeroizing;

use crate::{config::Config, http, ServerState};

type AdminResult<T> = std::result::Result<T, AdminError>;

struct AdminError(http::ApiError);

impl AdminError {
    /// 业务作用：按管理失败的 HTTP 类别提供稳定错误码，不通过消息文本判断业务状态。
    /// 参数说明：
    /// - `status`：认证、授权、校验、冲突或依赖失败的真实状态。
    /// - `message`：可以公开的固定安全提示。
    ///
    /// 返回：复用统一错误合同的管理失败，不携带口令、SQL 或本地路径。
    fn new(status: StatusCode, message: &'static str) -> Self {
        let code = match status {
            StatusCode::BAD_REQUEST => "invalid_request",
            StatusCode::UNAUTHORIZED => "unauthorized",
            StatusCode::FORBIDDEN => "forbidden",
            StatusCode::NOT_FOUND => "not_found",
            StatusCode::METHOD_NOT_ALLOWED => "method_not_allowed",
            StatusCode::CONFLICT => "repository_conflict",
            StatusCode::PAYLOAD_TOO_LARGE => "request_too_large",
            StatusCode::UNSUPPORTED_MEDIA_TYPE => "unsupported_media_type",
            StatusCode::UNPROCESSABLE_ENTITY => "validation_failed",
            StatusCode::TOO_MANY_REQUESTS => "rate_limited",
            StatusCode::SERVICE_UNAVAILABLE => "repository_unavailable",
            _ => "internal_error",
        };
        Self::with_code(status, code, message)
    }

    /// 业务作用：为 CSRF、来源门禁与排空等独立管理失败保留可供客户端识别的类别。
    /// 参数说明：
    /// - `status`：本次拒绝的真实 HTTP 状态。
    /// - `code`：固定业务错误码。
    /// - `message`：不包含底层诊断的安全提示。
    ///
    /// 返回：正文和响应头请求 ID 由统一响应器生成的错误。
    fn with_code(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self(http::ApiError::new(status, code, message))
    }
}

#[derive(Clone)]
struct BrowserProof {
    session: ArtifactDigest,
    csrf: Option<ArtifactDigest>,
}

struct BrowserProvider(Arc<ServerState>);

impl nasa::web::auth::AuthProvider for BrowserProvider {
    /// 业务作用：标识浏览器会话适配器，使身份方式保持稳定且不包含主体或凭据。
    /// 参数说明: 无。
    ///
    /// 返回：固定 provider 标识。
    fn id(&self) -> &'static str {
        "naxus-browser-session"
    }

    /// 业务作用：通过 NASA 身份合同复验当前会话与账号，将最小主体交给授权快照。
    /// 参数说明：
    /// - `input`：入口完成格式核验的只读凭据摘要，不含口令或原始会话值。
    ///
    /// 返回：仅账号有效且 CSRF 匹配时产生认证上下文；存储不可用时关闭访问。
    fn authenticate<'a>(
        &'a self,
        input: nasa::web::auth::AuthInput<'a>,
    ) -> nasa::web::auth::AuthFuture<
        'a,
        std::result::Result<nasa::web::auth::AuthOutcome, nasa::web::auth::AuthError>,
    > {
        use nasa::web::auth::{AuthContext, AuthError, AuthErrorKind, AuthOutcome};
        Box::pin(async move {
            let proof = input
                .extensions
                .get::<BrowserProof>()
                .cloned()
                .ok_or_else(|| AuthError::new(AuthErrorKind::Policy, "browser-proof-missing"))?;
            let identity = storage(&self.0, move |store| {
                store.browser_identity(&proof.session, proof.csrf.as_ref())
            })
            .await
            .map_err(|_| AuthError::new(AuthErrorKind::Backend, "browser-store-unavailable"))?
            .ok_or_else(|| AuthError::new(AuthErrorKind::Invalid, "browser-session-invalid"))?;
            let store = http::ready_store(&self.0)
                .map_err(|_| AuthError::new(AuthErrorKind::Backend, "browser-store-unavailable"))?;
            Ok(AuthOutcome::Authenticated(AuthContext {
                subject: identity.subject.clone().into(),
                tenant: store.service_id().into(),
                authentication_kind: "browser-session",
                principal: Arc::new(identity),
            }))
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    username: String,
    password: Zeroizing<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordChange {
    current_password: Zeroizing<String>,
    new_password: Zeroizing<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateUser {
    username: String,
    password: Zeroizing<String>,
    role: String,
    repositories: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserGrants {
    role: String,
    repositories: Vec<String>,
}

#[derive(Default, Deserialize)]
struct AuditQuery {
    #[serde(default)]
    before: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateToken {
    label: String,
    permission: String,
    repositories: Vec<String>,
    ttl_seconds: u64,
    #[serde(default)]
    allow_overwrite: bool,
    #[serde(default)]
    publish_crates: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PinRequest {
    snapshot: String,
    reason: String,
}

/// 业务作用：建立管理操作的完整授权快照，未显式声明的权限一律拒绝。
/// 参数说明: 无。
///
/// 返回：由 NASA 授权核心执行的固定 scope 策略，不依赖按钮可见性。
pub(crate) fn authorization() -> Result<PolicyDecisionSnapshot> {
    let policies = ["read", "publish", "manage", "purge", "users"]
        .into_iter()
        .map(|action| RoutePolicy {
            route_id: action.to_owned(),
            required_scopes: BTreeSet::from([action.to_owned()]),
            mode: RequireMode::All,
        })
        .collect();
    Ok(PolicyDecisionSnapshot::new(
        Arc::new(PolicySet::build(policies)?),
        UnmatchedRoutePolicy::Deny,
        1,
    ))
}

/// 业务作用：将当前角色转成最小 scope，并以同一 NASA 快照裁决管理操作。
/// 参数说明：
/// - `state`：不可变授权策略。
/// - `identity`：本次从数据库复验的身份。
/// - `action`：稳定业务操作名。
///
/// 返回：满足明确 scope 才成功，未知操作和初始换密状态拒绝。
fn authorize(state: &ServerState, identity: &AccessIdentity, action: &str) -> AdminResult<()> {
    let mut scopes = vec!["read"];
    if !identity.must_change {
        if identity.can_publish {
            scopes.push("publish");
        }
        if matches!(identity.role.as_str(), "admin" | "operator") {
            scopes.extend(["manage", "purge"]);
        }
        if identity.role == "admin" {
            scopes.push("users");
        }
    }
    let principal = Principal::with_scopes(scopes);
    if state.authorization.decide(action, &principal) == AuthzDecision::Permit {
        Ok(())
    } else {
        Err(AdminError::new(StatusCode::FORBIDDEN, "permission denied"))
    }
}

/// 业务作用：在空数据卷创建一次性管理员，口令先持久化再建立账户以覆盖启动中断窗口。
/// 参数说明：
/// - `configuration`：首启材料来源。
/// - `store`：已经独占并恢复的数据卷。
///
/// 返回：已有账号保持原身份；初始口令只写受限文件，不进入日志。
pub(crate) fn bootstrap(configuration: &Config, store: &Store) -> Result<()> {
    let (users, pending) = store.bootstrap_status()?;
    let generated = configuration.data_root()?.join("bootstrap/admin.password");
    if users > 0 {
        if pending == 0
            && configuration.admin.bootstrap_password_file.is_none()
            && generated.try_exists()?
        {
            fs::remove_file(&generated)?;
            fs::File::open(generated.parent().context("bootstrap has no parent")?)?.sync_all()?;
        }
        return Ok(());
    }
    let path = configuration
        .admin
        .bootstrap_password_file
        .as_ref()
        .unwrap_or(&generated);
    if configuration.admin.bootstrap_password_file.is_none() {
        let directory = path.parent().context("bootstrap has no parent")?;
        fs::create_dir_all(directory)?;
        ensure!(
            fs::symlink_metadata(directory)?.file_type().is_dir(),
            "bootstrap directory must not be a symlink"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        }
        if !path.try_exists()? {
            let value = Zeroizing::new(nasa::crypto::random_ascii(48));
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(path)?;
            file.write_all(value.as_bytes())?;
            file.sync_all()?;
            fs::File::open(directory)?.sync_all()?;
            fs::File::open(directory.parent().context("bootstrap root missing")?)?.sync_all()?;
        }
    }
    let password = read_secret(path)?;
    ensure!(
        (12..=72).contains(&password.len()) && !password.contains('\0'),
        "invalid bootstrap password length"
    );
    store.bootstrap_admin(&nasa::crypto::bcrypt(&password)?)?;
    tracing::info!(path = %path.display(), "初始管理员需要登录后设置新口令，初始材料位于受限文件");
    Ok(())
}

/// 业务作用：读取只供首启使用的受保护口令文件，拒绝符号链接和过大材料。
/// 参数说明：
/// - `path`：由部署配置或数据根确定的精确路径。
///
/// 返回：有界且释放时零化的口令文本。
fn read_secret(path: &Path) -> Result<Zeroizing<String>> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.file_type().is_file() && metadata.len() <= 1024,
        "invalid bootstrap material file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "bootstrap material must be private"
        );
    }
    let mut text = Zeroizing::new(String::new());
    fs::File::open(path)?.take(1025).read_to_string(&mut text)?;
    Ok(Zeroizing::new(
        text.trim_end_matches(['\r', '\n']).to_owned(),
    ))
}

/// 业务作用：装配同源页面、会话及管理 API，与仓库协议共享框架唯一端口。
/// 参数说明：
/// - `app`：NASA 应用容器。
/// - `state`：仓库与认证资源。
///
/// 返回：页面、API 及未知管理路径均登记到同一安全边界；不创建独立运行时或监听器。
pub(crate) fn install(app: &nasa::Application, state: Arc<ServerState>) -> Result<()> {
    app.configure_router(move |router| {
        router.merge(
            axum::Router::new()
                .route("/admin", get(redirect))
                .route("/admin/", get(page))
                .route("/admin/app.js", get(javascript))
                .route("/admin/style.css", get(stylesheet))
                .route("/admin/api/v1/login", post(login))
                .route("/admin/api/v1/session", get(session))
                .route("/admin/api/v1/logout", post(logout))
                .route("/admin/api/v1/password", post(change_password))
                .route("/admin/api/v1/repositories", get(repositories))
                .route(
                    "/admin/api/v1/proxies/{proxy}/packages",
                    get(proxy_packages),
                )
                .route(
                    "/admin/api/v1/repositories/{repository}/packages",
                    get(packages),
                )
                .route(
                    "/admin/api/v1/repositories/{repository}/packages/{name}/{version}/revisions",
                    get(revisions),
                )
                .route("/admin/api/v1/hosted/previews", post(preview_hosted))
                .route("/admin/api/v1/deletion-previews", post(preview_deletion))
                .route(
                    "/admin/api/v1/deletion-previews/{id}/commit",
                    post(commit_deletion),
                )
                .route("/admin/api/v1/deletions", get(deletions))
                .route("/admin/api/v1/deletions/{id}", get(deletion))
                .route("/admin/api/v1/deletions/{id}/retry", post(retry_deletion))
                .route(
                    "/admin/api/v1/repositories/{repository}/pins",
                    get(retention_pins).post(pin_snapshot),
                )
                .route(
                    "/admin/api/v1/repositories/{repository}/pins/{id}/release",
                    post(release_pin),
                )
                .route(
                    "/admin/api/v1/hosted/previews/{id}/commit",
                    post(commit_hosted),
                )
                .route("/admin/api/v1/users", get(users).post(create_user))
                .route("/admin/api/v1/users/{id}/enabled", post(user_enabled))
                .route("/admin/api/v1/users/{id}/grants", post(set_user_grants))
                .route("/admin/api/v1/tokens", get(tokens).post(create_token))
                .route("/admin/api/v1/tokens/{id}/revoke", post(revoke_token))
                .route("/admin/api/v1/audit", get(audit))
                // 显式捕获管理前缀的未知路径，使其仍经过管理安全边界，不落入 registry 认证兜底。
                .route("/admin/{*path}", axum::routing::any(not_found))
                .layer(Extension(state.clone()))
                .layer(from_fn_with_state(state, boundary)),
        )
    })?;
    Ok(())
}

/// 业务作用：将未知管理页面和 API 路径限制在 Admin 协议内，避免声明 Cargo 认证。
/// 参数说明: 无。
///
/// 返回：统一的 404 管理错误，不读取仓库内容或要求 registry 凭据。
async fn not_found() -> AdminError {
    AdminError::new(StatusCode::NOT_FOUND, "management resource not found")
}

/// 业务作用：为管理写请求先校验同源边界，并将响应限制为不可缓存、不可嵌入的页面或 JSON。
/// 参数说明：
/// - `state`：对外规范地址与排空状态。
/// - `request`：原始管理请求。
/// - `next`：匹配的管理处理器。
///
/// 返回：通过来源边界的响应；拒绝跨站写请求，安全头覆盖全部管理资源。
async fn boundary(State(state): State<Arc<ServerState>>, request: Request, next: Next) -> Response {
    let request_id = request
        .extensions()
        .get::<nasa::application::RequestId>()
        .map(|id| id.as_str().to_owned())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    http::RESPONSE_REQUEST_ID.scope(request_id, async move {
    let api_request = request.uri().path().starts_with("/admin/api/v1/");
    let writes = !matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    );
    let mut response = if writes && state.draining.load(std::sync::atomic::Ordering::Acquire) {
        AdminError::with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_draining",
            "service is draining",
        )
        .into_response()
    } else if writes && !same_origin(&state, request.headers()) {
        AdminError::with_code(
            StatusCode::FORBIDDEN,
            "origin_rejected",
            "same-origin request required",
        )
        .into_response()
    } else {
        next.run(request).await
    };
    if api_request
        && (response.status().is_client_error() || response.status().is_server_error())
        && response.extensions().get::<http::ErrorResponse>().is_none()
    {
        // 框架的参数或方法拒绝也属于管理协议，不能回显原始参数文本或产生另一种错误结构。
        let allow = response.headers().get("allow").cloned();
        response =
            AdminError::new(response.status(), "management request rejected").into_response();
        if let Some(allow) = allow {
            response.headers_mut().insert("allow", allow);
        }
    }
    for (name, value) in [("cache-control", "no-store"), ("x-content-type-options", "nosniff"), ("x-frame-options", "DENY"), ("referrer-policy", "no-referrer"), ("content-security-policy", "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'")] {
        response.headers_mut().insert(axum::http::HeaderName::from_static(name), value.parse().expect("static security header"));
    }
    response
    }).await
}

/// 业务作用：按规范对外来源验证 Origin，不信任代理转发的 Host 或用户提交的来源字段。
/// 参数说明：
/// - `state`：固定的规范来源。
/// - `headers`：请求头。
///
/// 返回：恰好一个合法且同源的 Origin 时为 true。
fn same_origin(state: &ServerState, headers: &HeaderMap) -> bool {
    headers.get_all("origin").iter().count() == 1
        && headers
            .get("origin")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| url::Url::parse(value).ok())
            .is_some_and(|origin| {
                origin.origin() == state.config.public_base_url.origin()
                    && origin.path() == "/"
                    && origin.query().is_none()
                    && origin.fragment().is_none()
                    && origin.username().is_empty()
                    && origin.password().is_none()
            })
}

/// 业务作用：读取恰好一个会话 Cookie，拒绝同名重复值导致代理与应用解释不一致。
/// 参数说明：
/// - `headers`：浏览器请求头。
///
/// 返回：高熵会话值的摘要；缺失或重复按未登录拒绝。
fn session_digest(headers: &HeaderMap) -> AdminResult<ArtifactDigest> {
    let mut values = Vec::new();
    for header in headers.get_all("cookie") {
        for item in header
            .to_str()
            .map_err(|_| AdminError::new(StatusCode::UNAUTHORIZED, "sign in required"))?
            .split(';')
        {
            if let Some(value) = item.trim().strip_prefix("naxus_admin=") {
                values.push(value);
            }
        }
    }
    if values.len() != 1
        || values[0].len() != 64
        || !values[0].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AdminError::new(
            StatusCode::UNAUTHORIZED,
            "sign in required",
        ));
    }
    Ok(ArtifactDigest::of(values[0].as_bytes()))
}

/// 业务作用：复验每个管理请求的账户状态与 Cookie，会话不能代替 Cargo 发布凭据。
/// 参数说明：
/// - `state`：受管存储。
/// - `headers`：Cookie 与写请求 CSRF。
/// - `write`：是否需要核对显式 CSRF 材料。
/// - `initial`：是否允许首次登录的强制换密状态。
///
/// 返回：当前最小身份；CSRF、账户状态或初始门禁不满足时拒绝。
async fn identity(
    state: &Arc<ServerState>,
    headers: &HeaderMap,
    write: bool,
    initial: bool,
) -> AdminResult<AccessIdentity> {
    let session = session_digest(headers)?;
    let csrf = if write {
        if headers.get_all("x-csrf-token").iter().count() != 1 {
            return Err(AdminError::with_code(
                StatusCode::FORBIDDEN,
                "csrf_rejected",
                "CSRF token required",
            ));
        }
        let token = headers
            .get("x-csrf-token")
            .and_then(|value| value.to_str().ok())
            .filter(|value| value.len() == 64)
            .ok_or(AdminError::with_code(
                StatusCode::FORBIDDEN,
                "csrf_rejected",
                "CSRF token required",
            ))?;
        Some(ArtifactDigest::of(token.as_bytes()))
    } else {
        None
    };
    use nasa::web::auth::{AuthInput, AuthOutcome, AuthProvider};
    let mut extensions = axum::http::Extensions::new();
    extensions.insert(BrowserProof {
        session: session.clone(),
        csrf,
    });
    let provider = BrowserProvider(state.clone());
    let authenticated = provider
        .authenticate(AuthInput {
            method: if write {
                "POST"
            } else {
                "GET"
            },
            route_id: "naxus-admin-session",
            path: "/admin/",
            headers,
            extensions: &extensions,
        })
        .await;
    let outcome = match authenticated {
        Ok(outcome) => outcome,
        Err(error) => {
            if write && error.kind == nasa::web::auth::AuthErrorKind::Invalid {
                // 有效会话的 CSRF 拒绝属于当前操作权限，不能让浏览器把它误判为身份已经失效。
                let valid_session =
                    storage(state, move |store| store.browser_identity(&session, None))
                        .await?
                        .is_some();
                if valid_session {
                    return Err(AdminError::with_code(
                        StatusCode::FORBIDDEN,
                        "csrf_rejected",
                        "CSRF token rejected",
                    ));
                }
            }
            return Err(AdminError::new(
                error.status(),
                "sign in unavailable or session invalid",
            ));
        }
    };
    let AuthOutcome::Authenticated(context) = outcome else {
        return Err(AdminError::new(
            StatusCode::UNAUTHORIZED,
            "sign in required",
        ));
    };
    let identity = context
        .principal_as::<AccessIdentity>()
        .cloned()
        .ok_or(AdminError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "session identity unavailable",
        ))?;
    if identity.must_change && !initial {
        return Err(AdminError::new(
            StatusCode::FORBIDDEN,
            "set a new password first",
        ));
    }
    Ok(identity)
}

/// 业务作用：执行有界登录校验，失败不暴露用户名是否存在。
/// 参数说明：
/// - `state`：CPU 预算、退避与账户存储。
/// - `request`：用户名和口令。
///
/// 返回：成功时设置受保护 Cookie；强制换密状态只开放换密与退出。
async fn login(
    Extension(state): Extension<Arc<ServerState>>,
    request: Request,
) -> AdminResult<Response> {
    let body: Login = decode(request).await?;
    if naxus_core::validate_name(&body.username).is_err()
        || body.password.is_empty()
        || body.password.len() > 72
        || body.password.contains('\0')
    {
        return Err(AdminError::with_code(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "invalid credentials",
        ));
    }
    {
        let mut window = state.login_window.lock().map_err(|_| {
            AdminError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "authentication unavailable",
            )
        })?;
        if window.0.elapsed() >= Duration::from_secs(60) {
            *window = (std::time::Instant::now(), 0);
        }
        if window.1 >= 60 {
            return Err(AdminError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "login budget exceeded",
            ));
        }
        window.1 += 1;
    }
    let permit = state
        .login_budget
        .clone()
        .try_acquire_owned()
        .map_err(|_| AdminError::new(StatusCode::TOO_MANY_REQUESTS, "authentication busy"))?;
    let dummy = state.dummy_password.get().cloned().ok_or(AdminError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "authentication unavailable",
    ))?;
    let session = Zeroizing::new(nasa::crypto::generate_salt(32));
    let csrf = Zeroizing::new(nasa::crypto::generate_salt(32));
    let session_hash = ArtifactDigest::of(session.as_bytes());
    let csrf_hash = ArtifactDigest::of(csrf.as_bytes());
    let accepted = storage(&state, move |store| {
        let _permit = permit;
        let user = store.login_user(&body.username)?;
        let hash = user
            .as_ref()
            .map(|user| user.password_hash.as_str())
            .unwrap_or(&dummy);
        let valid = nasa::crypto::bcrypt_check(&body.password, hash);
        if let Some(user) = user {
            store.finish_login(&user, valid && user.available, &session_hash, &csrf_hash)
        } else {
            Ok(false)
        }
    })
    .await?;
    if !accepted {
        return Err(AdminError::with_code(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "invalid credentials",
        ));
    }
    let mut response = Json(serde_json::json!({
        "authenticated": true
    }))
    .into_response();
    cookies(&state, &mut response, &session, &csrf, false)?;
    Ok(response)
}

/// 业务作用：返回当前浏览器身份，页面刷新不依赖保存明文口令。
/// 参数说明：
/// - `state`：当前服务资源。
/// - `headers`：浏览器 Cookie。
///
/// 返回：当前角色、仓库范围与强制换密状态。
async fn session(
    Extension(state): Extension<Arc<ServerState>>,
    headers: HeaderMap,
) -> AdminResult<Response> {
    Ok(Json(identity(&state, &headers, false, true).await?).into_response())
}

/// 业务作用：撤销当前浏览器会话并清除客户端 Cookie。
/// 参数说明：
/// - `state`：当前服务资源。
/// - `headers`：会话与 CSRF。
///
/// 返回：持久会话已撤销的 204 响应。
async fn logout(
    Extension(state): Extension<Arc<ServerState>>,
    headers: HeaderMap,
) -> AdminResult<Response> {
    identity(&state, &headers, true, true).await?;
    let session = session_digest(&headers)?;
    storage(&state, move |store| store.logout(&session)).await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    cookies(&state, &mut response, "", "", true)?;
    Ok(response)
}

/// 业务作用：使用旧口令与 CSRF 双重证明完成换密，随后撤销全部旧凭据。
/// 参数说明：
/// - `state`：账户存储与 CPU 预算。
/// - `headers`：当前会话与 CSRF。
/// - `request`：旧口令与新口令。
///
/// 返回：换密成功后清除 Cookie 并要求重新登录；初始明文文件同步移除。
async fn change_password(
    Extension(state): Extension<Arc<ServerState>>,
    headers: HeaderMap,
    request: Request,
) -> AdminResult<Response> {
    let actor = identity(&state, &headers, true, true).await?;
    let body: PasswordChange = decode(request).await?;
    password_length(&body.new_password)?;
    if body.current_password.len() > 72 || body.current_password == body.new_password {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            "a different valid password is required",
        ));
    }
    let permit = state
        .login_budget
        .clone()
        .try_acquire_owned()
        .map_err(|_| AdminError::new(StatusCode::TOO_MANY_REQUESTS, "authentication busy"))?;
    storage(&state, move |store| {
        let _permit = permit;
        let user = store
            .login_user(&actor.username)?
            .context("account unavailable")?;
        ensure!(
            user.id == actor.subject
                && user.available
                && nasa::crypto::bcrypt_check(&body.current_password, &user.password_hash),
            naxus_store::RepositoryFailure::InvalidRequest("current password rejected")
        );
        store.change_password(&user, &nasa::crypto::bcrypt(&body.new_password)?)
    })
    .await?;
    let configuration = state.config.clone();
    storage(&state, move |store| bootstrap(&configuration, &store)).await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    cookies(&state, &mut response, "", "", true)?;
    Ok(response)
}

/// 业务作用：限制 BCrypt 口令的完整 UTF-8 字节长度，禁止算法静默截断。
/// 参数说明：
/// - `password`：待设置的新口令。
///
/// 返回：十二至七十二字节且无 NUL 时成功。
fn password_length(password: &str) -> AdminResult<()> {
    if !(12..=72).contains(&password.len()) || password.contains('\0') {
        return Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            "password must contain 12 to 72 UTF-8 bytes without NUL",
        ));
    }
    Ok(())
}

/// 业务作用：显示当前主体有权读取的 hosted 与公开来源，不借总览泄露其他仓库。
/// 参数说明：
/// - `state`：当前存储。
/// - `headers`：浏览器会话。
///
/// 返回：有权限的仓库与代理身份。
async fn repositories(
    Extension(state): Extension<Arc<ServerState>>,
    headers: HeaderMap,
) -> AdminResult<Response> {
    let actor = identity(&state, &headers, false, false).await?;
    authorize(&state, &actor, "read")?;
    let status = state
        .storage_status
        .read()
        .map_err(|_| {
            AdminError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "storage observation unavailable",
            )
        })?
        .clone();
    let metrics = state.metrics.overview();
    let enabled = state
        .config
        .proxies
        .iter()
        .map(|proxy| proxy.name.clone())
        .collect::<BTreeSet<_>>();
    Ok(Json(
        storage(&state, move |store| {
            let repositories = store
                .repositories()?
                .into_iter()
                .filter(|repository| {
                    actor
                        .repositories
                        .iter()
                        .any(|allowed| allowed == "*" || allowed == &repository.id)
                })
                .collect::<Vec<_>>();
            let proxies = store
                .proxies()?
                .into_iter()
                .filter(|proxy| enabled.contains(&proxy.name))
                .collect::<Vec<_>>();
            Ok(serde_json::json!({
                "hosted": repositories,
                "proxies": proxies,
                "service_uuid": store.service_id(),
                "storage": status,
                "proxy_metrics": metrics
            }))
        })
        .await?,
    )
    .into_response())
}

/// 业务作用：按目标仓库读取当前坐标与可见性，避免页面用包名跨仓库查询。
/// 参数说明：
/// - `state`：当前存储。
/// - `repository`：路径中的仓库身份。
/// - `headers`：当前会话。
/// - `query`：精确名称和分页游标。
///
/// 返回：有权仓库的有界坐标列表。
async fn packages(
    Extension(state): Extension<Arc<ServerState>>,
    RoutePath(repository): RoutePath<String>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<CatalogQuery>,
) -> AdminResult<Response> {
    let actor = identity(&state, &headers, false, false).await?;
    Ok(Json(
        storage(&state, move |store| {
            let repository = read_permission(&store, &actor, &repository)?;
            store.catalog(&repository, query.name.as_deref(), &query.after)
        })
        .await?,
    )
    .into_response())
}

/// 业务作用：查询独立公开来源的缓存事实，公开依赖清理仍由带权限预览执行。
/// 参数说明：
/// - `state`：当前服务资源。
/// - `proxy`：公开来源身份。
/// - `headers`：浏览器会话。
/// - `query`：分页游标。
///
/// 返回：单来源的精确坐标和本地缓存状态，不返回上游凭据。
async fn proxy_packages(
    Extension(state): Extension<Arc<ServerState>>,
    RoutePath(proxy): RoutePath<String>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<CatalogQuery>,
) -> AdminResult<Response> {
    identity(&state, &headers, false, false).await?;
    Ok(Json(
        storage(&state, move |store| {
            store.proxy_catalog(&proxy, &query.after)
        })
        .await?,
    )
    .into_response())
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct CatalogQuery {
    name: Option<String>,
    after: String,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RevisionQuery {
    before: i64,
}

/// 业务作用：查询同一仓库精确坐标的历史修订，为回退选择提供真实对象身份。
/// 参数说明：
/// - `state`：当前存储。
/// - `repository`：授权仓库。
/// - `name`：精确包名。
/// - `version`：精确版本。
/// - `query`：上一页返回的历史游标，省略时从最新修订开始。
/// - `headers`：当前会话。
///
/// 返回：最多二百条修订及下一页游标，越权查询不返回其他仓库内容。
async fn revisions(
    Extension(state): Extension<Arc<ServerState>>,
    RoutePath((repository, name, version)): RoutePath<(String, String, String)>,
    Query(query): Query<RevisionQuery>,
    headers: HeaderMap,
) -> AdminResult<Response> {
    let actor = identity(&state, &headers, false, false).await?;
    Ok(Json(
        storage(&state, move |store| {
            let repository = read_permission(&store, &actor, &repository)?;
            store.revision_history_page(&repository, &name, &version, query.before)
        })
        .await?,
    )
    .into_response())
}

/// 业务作用：为包移除、yank 与回退生成精确影响预览，尚不改变仓库可见性。
/// 参数说明：
/// - `state`：当前授权和存储。
/// - `headers`：管理会话和 CSRF。
/// - `request`：明确选择的仓库、坐标及动作。
///
/// 返回：绑定当前 HEAD 和主体的有期限预览。
async fn preview_hosted(
    Extension(state): Extension<Arc<ServerState>>,
    headers: HeaderMap,
    request: Request,
) -> AdminResult<Response> {
    let actor = identity(&state, &headers, true, false).await?;
    authorize(&state, &actor, "manage")?;
    let action: naxus_store::HostedAction = decode(request).await?;
    Ok((
        StatusCode::CREATED,
        Json(storage(&state, move |store| store.preview_hosted(&actor, &action)).await?),
    )
        .into_response())
}

/// 业务作用：确认预览后复验权限与 HEAD，再原子提交新的管理快照。
/// 参数说明：
/// - `state`：当前授权与存储。
/// - `id`：先前显示的预览 UUID。
/// - `headers`：管理会话与 CSRF。
/// - `request`：已确认范围的指纹及幂等键。
///
/// 返回：完整提交的新快照；过期、范围变化或权限撤销不产生部分变更。
async fn commit_hosted(
    Extension(state): Extension<Arc<ServerState>>,
    RoutePath(id): RoutePath<String>,
    headers: HeaderMap,
    request: Request,
) -> AdminResult<Response> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Confirmation {
        fingerprint: ArtifactDigest,
        idempotency_key: String,
    }
    let actor = identity(&state, &headers, true, false).await?;
    authorize(&state, &actor, "manage")?;
    let confirmation: Confirmation = decode(request).await?;
    Ok(Json(
        storage(&state, move |store| {
            store.commit_hosted_management(
                &actor,
                &id,
                &confirmation.fingerprint,
                &confirmation.idempotency_key,
            )
        })
        .await?,
    )
    .into_response())
}

/// 业务作用：在物理清除前显示准确的存储代次、共享引用与活动保护，缓存清理不隐含封禁。
/// 参数说明：
/// - `state`：当前授权和存储。
/// - `headers`：管理会话及 CSRF。
/// - `request`：精确来源、版本或修订和历史撤销选择。
///
/// 返回：可提交或被保护阻挡的影响预览，不在此时处理正式文件。
async fn preview_deletion(
    Extension(state): Extension<Arc<ServerState>>,
    headers: HeaderMap,
    request: Request,
) -> AdminResult<Response> {
    let actor = identity(&state, &headers, true, false).await?;
    authorize(&state, &actor, "purge")?;
    let scope: naxus_store::DeletionScope = decode(request).await?;
    Ok((
        StatusCode::CREATED,
        Json(storage(&state, move |store| store.preview_deletion(&actor, &scope)).await?),
    )
        .into_response())
}

/// 业务作用：确认范围后持久受理清除任务，再唤醒 NASA 唯一维护工作者，页面断开不取消任务。
/// 参数说明：
/// - `state`：当前授权和受管任务唤醒器。
/// - `id`：已经显示的预览身份。
/// - `headers`：管理会话和 CSRF。
/// - `request`：用户确认的预览指纹。
///
/// 返回：202 与持久任务状态，不把受理解释为已经释放字节。
async fn commit_deletion(
    Extension(state): Extension<Arc<ServerState>>,
    RoutePath(id): RoutePath<String>,
    headers: HeaderMap,
    request: Request,
) -> AdminResult<Response> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Confirmation {
        fingerprint: ArtifactDigest,
    }
    let actor = identity(&state, &headers, true, false).await?;
    authorize(&state, &actor, "purge")?;
    let confirmation: Confirmation = decode(request).await?;
    let task = storage(&state, move |store| {
        store.accept_deletion(&actor, &id, &confirmation.fingerprint)
    })
    .await?;
    // 删除权威已先进入数据库；即使请求取消错过此通知，维护周期仍会发现并执行该任务。
    state.maintenance_wake.notify_one();
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

/// 业务作用：向授权运维提供持久清除任务列表，隐藏其他仓库的管理范围。
/// 参数说明：
/// - `state`：当前授权和存储。
/// - `headers`：管理会话。
///
/// 返回：有权查看的任务及实际释放字节，未知作用域不放行。
async fn deletions(
    Extension(state): Extension<Arc<ServerState>>,
    headers: HeaderMap,
) -> AdminResult<Response> {
    let actor = identity(&state, &headers, false, false).await?;
    authorize(&state, &actor, "purge")?;
    Ok(Json(
        storage(&state, move |store| {
            let mut visible = Vec::new();
            for task in store.deletion_tasks()? {
                if deletion_read_allowed(&store, &actor, &task)? {
                    visible.push(task);
                }
            }
            Ok(visible)
        })
        .await?,
    )
    .into_response())
}

/// 业务作用：按任务实际作用域校验查询权限，不通过可猜测 UUID 泄露其他仓库任务。
/// 参数说明：
/// - `state`：当前授权和存储。
/// - `id`：持久任务 UUID。
/// - `headers`：管理会话。
///
/// 返回：有权查询的任务阶段，不在 GET 中执行删除。
async fn deletion(
    Extension(state): Extension<Arc<ServerState>>,
    RoutePath(id): RoutePath<String>,
    headers: HeaderMap,
) -> AdminResult<Response> {
    let actor = identity(&state, &headers, false, false).await?;
    authorize(&state, &actor, "purge")?;
    Ok(Json(
        storage(&state, move |store| {
            let task = store.deletion_task(&id)?;
            ensure!(
                deletion_read_allowed(&store, &actor, &task)?,
                naxus_store::RepositoryFailure::Forbidden("task scope permission denied")
            );
            Ok(task)
        })
        .await?,
    )
    .into_response())
}

/// 业务作用：按任务的真实来源限制管理查询，公开缓存管理需要全域运维授权。
/// 参数说明：
/// - `store`：当前存储。
/// - `actor`：已验证运维身份。
/// - `task`：任务持久范围。
///
/// 返回：目标范围属于当前授权时为 true；越权或已移除仓库不开放，真实存储失败保留错误类型。
fn deletion_read_allowed(
    store: &Store,
    actor: &AccessIdentity,
    task: &naxus_store::DeletionTask,
) -> Result<bool> {
    if actor.repositories.iter().any(|scope| scope == "*") {
        return Ok(true);
    }
    if task.scope.kind != "hosted" {
        return Ok(false);
    }
    match read_permission(store, actor, &task.scope.repository) {
        Ok(_) => Ok(true),
        Err(error)
            if matches!(
                error.downcast_ref::<naxus_store::RepositoryFailure>(),
                Some(
                    naxus_store::RepositoryFailure::NotFound(_)
                        | naxus_store::RepositoryFailure::Forbidden(_)
                )
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

/// 业务作用：以仓库稳定 UUID 核对管理查询范围，不依赖界面隐藏未授权选项。
/// 参数说明：
/// - `store`：当前存储。
/// - `actor`：已验证浏览器身份。
/// - `repository`：请求的仓库名称或 UUID。
///
/// 返回：目标存在且授予读取权限时提供稳定 UUID；未知或未授权分别保留存在性与权限错误类型。
fn read_permission(store: &Store, actor: &AccessIdentity, repository: &str) -> Result<String> {
    let repository = store
        .repositories()?
        .into_iter()
        .find(|item| item.name == repository || item.id == repository)
        .context(naxus_store::RepositoryFailure::NotFound(
            "unknown repository",
        ))?;
    ensure!(
        actor
            .repositories
            .iter()
            .any(|grant| grant == "*" || grant == &repository.id),
        naxus_store::RepositoryFailure::Forbidden("repository read permission denied")
    );
    Ok(repository.id)
}

/// 业务作用：由仍有权限的运维立即重试既有清除任务，保留原对象代次和进度。
/// 参数说明：
/// - `state`：受管存储及维护通知。
/// - `id`：既有持久任务 UUID。
/// - `headers`：当前会话与 CSRF。
///
/// 返回：已有任务的当前状态；无权限或 CSRF 不满足时不解除退避。
async fn retry_deletion(
    Extension(state): Extension<Arc<ServerState>>,
    RoutePath(id): RoutePath<String>,
    headers: HeaderMap,
) -> AdminResult<Response> {
    let actor = identity(&state, &headers, true, false).await?;
    authorize(&state, &actor, "purge")?;
    let task = storage(&state, move |store| store.retry_deletion(&actor, &id)).await?;
    state.maintenance_wake.notify_one();
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

/// 业务作用：让运维查看目标仓库的显式保留点，解释历史清除为何被阻挡。
/// 参数说明：
/// - `state`：受管存储。
/// - `repository`：界面选择的仓库。
/// - `headers`：当前管理会话。
///
/// 返回：有目标仓库权限时展示保留点，不产生引用变化。
async fn retention_pins(
    Extension(state): Extension<Arc<ServerState>>,
    RoutePath(repository): RoutePath<String>,
    headers: HeaderMap,
) -> AdminResult<Response> {
    let actor = identity(&state, &headers, false, false).await?;
    authorize(&state, &actor, "manage")?;
    Ok(Json(
        storage(&state, move |store| {
            let repository = read_permission(&store, &actor, &repository)?;
            store.retention_pins(&repository)
        })
        .await?,
    )
    .into_response())
}

/// 业务作用：运维通过写请求保留明确快照，保护持续需要的历史构建。
/// 参数说明：
/// - `state`：受管存储。
/// - `repository`：目标仓库。
/// - `headers`：会话与 CSRF。
/// - `request`：精确快照及保留原因。
///
/// 返回：权限及可读性复验后返回保留点 UUID，已撤销历史不能重新开放。
async fn pin_snapshot(
    Extension(state): Extension<Arc<ServerState>>,
    RoutePath(repository): RoutePath<String>,
    headers: HeaderMap,
    request: Request,
) -> AdminResult<Response> {
    let actor = identity(&state, &headers, true, false).await?;
    authorize(&state, &actor, "manage")?;
    let body: PinRequest = decode(request).await?;
    let id = storage(&state, move |store| {
        store.pin_snapshot(&actor, &repository, &body.snapshot, &body.reason)
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": id
        })),
    )
        .into_response())
}

/// 业务作用：运维明确解除一个同仓保留点，清除仍需另行预览并检查其他引用。
/// 参数说明：
/// - `state`：受管存储。
/// - `repository`：目标 hosted 仓库名称或 UUID。
/// - `id`：需要解除的保留点 UUID。
/// - `headers`：管理会话与 CSRF。
///
/// 返回：当前权限通过后解除精确保留点，不删除快照或对象。
async fn release_pin(
    Extension(state): Extension<Arc<ServerState>>,
    RoutePath((repository, id)): RoutePath<(String, String)>,
    headers: HeaderMap,
) -> AdminResult<Response> {
    let actor = identity(&state, &headers, true, false).await?;
    authorize(&state, &actor, "manage")?;
    storage(&state, move |store| {
        store.release_pin(&actor, &repository, &id)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// 业务作用：只向管理员列出用户授权，不向普通读者暴露账户信息。
/// 参数说明：
/// - `state`：当前授权与存储。
/// - `headers`：管理会话。
///
/// 返回：无口令材料的用户列表。
async fn users(
    Extension(state): Extension<Arc<ServerState>>,
    headers: HeaderMap,
) -> AdminResult<Response> {
    let actor = identity(&state, &headers, false, false).await?;
    authorize(&state, &actor, "users")?;
    Ok(Json(storage(&state, move |store| store.users()).await?).into_response())
}

/// 业务作用：由有权管理员创建受限用户，哈希工作执行于有界阻塞域。
/// 参数说明：
/// - `state`：授权、CPU 与存储资源。
/// - `headers`：会话与 CSRF。
/// - `request`：新用户和明确授权范围。
///
/// 返回：新用户 UUID，失败不部分创建授权。
async fn create_user(
    Extension(state): Extension<Arc<ServerState>>,
    headers: HeaderMap,
    request: Request,
) -> AdminResult<Response> {
    let actor = identity(&state, &headers, true, false).await?;
    authorize(&state, &actor, "users")?;
    let body: CreateUser = decode(request).await?;
    password_length(&body.password)?;
    let permit = state
        .login_budget
        .clone()
        .try_acquire_owned()
        .map_err(|_| AdminError::new(StatusCode::TOO_MANY_REQUESTS, "authentication busy"))?;
    let id = storage(&state, move |store| {
        let _permit = permit;
        store.create_user(
            &actor,
            &body.username,
            &nasa::crypto::bcrypt(&body.password)?,
            &body.role,
            &body.repositories,
        )
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": id
        })),
    )
        .into_response())
}

/// 业务作用：在提交时复验管理员权限后启停账户，禁用立即撤销已有身份材料。
/// 参数说明：
/// - `state`：当前授权与账户存储。
/// - `id`：目标用户 UUID。
/// - `headers`：管理会话和 CSRF。
/// - `request`：目标启用状态。
///
/// 返回：启停成功的 204，最后管理员保护由同一数据库事务执行。
async fn user_enabled(
    Extension(state): Extension<Arc<ServerState>>,
    RoutePath(id): RoutePath<String>,
    headers: HeaderMap,
    request: Request,
) -> AdminResult<Response> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Enabled {
        enabled: bool,
    }
    let actor = identity(&state, &headers, true, false).await?;
    authorize(&state, &actor, "users")?;
    let body: Enabled = decode(request).await?;
    storage(&state, move |store| {
        store.set_user_enabled(&actor, &id, body.enabled)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// 业务作用：管理员更新账号授权，撤销旧会话和 token 后按新范围重新登录。
/// 参数说明：
/// - `state`：受管存储和授权快照。
/// - `id`：目标用户 UUID。
/// - `headers`：会话及 CSRF。
/// - `request`：明确的新角色与仓库范围。
///
/// 返回：新授权和认证代次原子提交，最后管理员和权限边界不满足时拒绝。
async fn set_user_grants(
    Extension(state): Extension<Arc<ServerState>>,
    RoutePath(id): RoutePath<String>,
    headers: HeaderMap,
    request: Request,
) -> AdminResult<Response> {
    let actor = identity(&state, &headers, true, false).await?;
    authorize(&state, &actor, "users")?;
    let body: UserGrants = decode(request).await?;
    storage(&state, move |store| {
        store.set_user_grants(&actor, &id, &body.role, &body.repositories)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// 业务作用：显示当前用户能够管理的访问令牌，明文和校验摘要均不返回。
/// 参数说明：
/// - `state`：当前存储。
/// - `headers`：管理会话。
///
/// 返回：令牌用途、权限、期限与撤销状态。
async fn tokens(
    Extension(state): Extension<Arc<ServerState>>,
    headers: HeaderMap,
) -> AdminResult<Response> {
    let actor = identity(&state, &headers, false, false).await?;
    Ok(Json(storage(&state, move |store| store.tokens(&actor)).await?).into_response())
}

/// 业务作用：为当前用户创建独立 Cargo 令牌，并仅在本次响应展示明文。
/// 参数说明：
/// - `state`：当前存储。
/// - `headers`：会话与 CSRF。
/// - `request`：用途、权限、仓库范围与期限。
///
/// 返回：令牌 UUID 与一次性明文，数据库仅保存摘要。
async fn create_token(
    Extension(state): Extension<Arc<ServerState>>,
    headers: HeaderMap,
    request: Request,
) -> AdminResult<Response> {
    let actor = identity(&state, &headers, true, false).await?;
    let body: CreateToken = decode(request).await?;
    let token = Zeroizing::new(nasa::crypto::generate_salt(32));
    let digest = ArtifactDigest::of(token.as_bytes());
    let id = storage(&state, move |store| {
        store.create_scoped_token(
            &actor,
            &digest,
            &naxus_store::TokenGrant {
                label: &body.label,
                permission: &body.permission,
                repositories: &body.repositories,
                ttl_seconds: body.ttl_seconds,
                allow_overwrite: body.allow_overwrite,
                publish_crates: &body.publish_crates,
            },
        )
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": id,
            "token": token.as_str()
        })),
    )
        .into_response())
}

/// 业务作用：撤销当前用户拥有的令牌，管理员可处理全域令牌。
/// 参数说明：
/// - `state`：当前存储。
/// - `id`：令牌管理身份。
/// - `headers`：会话与 CSRF。
///
/// 返回：撤销已持久化的 204，所有权由存储提交时再次核对。
async fn revoke_token(
    Extension(state): Extension<Arc<ServerState>>,
    RoutePath(id): RoutePath<String>,
    headers: HeaderMap,
) -> AdminResult<Response> {
    let actor = identity(&state, &headers, true, false).await?;
    storage(&state, move |store| store.revoke_token(&actor, &id)).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// 业务作用：向运维与管理员提供有界安全审计，不让读者查询全域主体活动。
/// 参数说明：
/// - `state`：当前授权与存储。
/// - `headers`：管理会话。
///
/// - `query`：向更早记录翻页的稳定序号游标。
///
/// 返回：最新或指定更早窗口中的审计记录。
async fn audit(
    Extension(state): Extension<Arc<ServerState>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<AuditQuery>,
) -> AdminResult<Response> {
    let actor = identity(&state, &headers, false, false).await?;
    authorize(&state, &actor, "manage")?;
    Ok(
        Json(storage(&state, move |store| store.security_audit_page(query.before)).await?)
            .into_response(),
    )
}

/// 业务作用：只从规范对外路径构造 Cookie 边界，HTTPS 部署强制 Secure。
/// 参数说明：
/// - `state`：规范对外 URL。
/// - `response`：登录或退出响应。
/// - `session`：高熵会话材料。
/// - `csrf`：浏览器可读的独立 CSRF 材料。
/// - `clear`：是否注销并使 Cookie 立即过期。
///
/// 返回：两个 Cookie 均成功写入；认证 Cookie 始终 HttpOnly。
fn cookies(
    state: &ServerState,
    response: &mut Response,
    session: &str,
    csrf: &str,
    clear: bool,
) -> AdminResult<()> {
    let path = format!(
        "{}/admin/",
        state.config.public_base_url.path().trim_end_matches('/')
    );
    let secure = if state.config.public_base_url.scheme() == "https" {
        "; Secure"
    } else {
        ""
    };
    for (name, value, http_only) in [
        ("naxus_admin", session, "; HttpOnly"),
        ("naxus_csrf", csrf, ""),
    ] {
        let value = format!(
            "{name}={value}; Path={path}; SameSite=Strict; Max-Age={}{secure}{http_only}",
            if clear {
                0
            } else {
                28800
            }
        );
        response.headers_mut().append(
            SET_COOKIE,
            value.parse().map_err(|_| {
                AdminError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "cookie configuration unavailable",
                )
            })?,
        );
    }
    Ok(())
}

/// 业务作用：拒绝超额管理正文，避免口令和用户管理占用制品级内存预算。
/// 参数说明：
/// - `request`：完整管理请求。
///
/// 返回：最大六十四 KiB 的强类型 JSON。
async fn decode<T: DeserializeOwned>(request: Request) -> AdminResult<T> {
    let body = to_bytes(request.into_body(), 64 * 1024)
        .await
        .map_err(|_| AdminError::new(StatusCode::BAD_REQUEST, "invalid request body"))?;
    serde_json::from_slice(&body)
        .map_err(|_| AdminError::new(StatusCode::BAD_REQUEST, "invalid JSON request"))
}

/// 业务作用：复用仓库实例的有界阻塞执行域，认证与管理不创建旁路连接池。
/// 参数说明：
/// - `state`：当前预算。
/// - `operation`：单次同步账户或仓库操作。
///
/// 返回：成功结果；内部错误不携带敏感材料返回浏览器。
async fn storage<T: Send + 'static>(
    state: &Arc<ServerState>,
    operation: impl FnOnce(Arc<Store>) -> Result<T> + Send + 'static,
) -> AdminResult<T> {
    http::blocking(state, operation).await.map_err(AdminError)
}

/// 业务作用：将无末尾斜线的管理入口转向相同服务的规范页面路径。
/// 参数说明：
/// - `state`：规范服务地址。
///
/// 返回：固定 Location 的 308，不接受请求控制的跳转目标。
async fn redirect(Extension(state): Extension<Arc<ServerState>>) -> Response {
    (
        StatusCode::PERMANENT_REDIRECT,
        [(
            "location",
            format!(
                "{}/admin/",
                state.config.public_base_url.as_str().trim_end_matches('/')
            ),
        )],
    )
        .into_response()
}

/// 业务作用：提供镜像内嵌的管理页面，不依赖公网前端服务。
/// 参数说明: 无。
///
/// 返回：同源 HTML 页面。
async fn page() -> Response {
    (
        [("content-type", "text/html; charset=utf-8")],
        include_str!("admin/index.html"),
    )
        .into_response()
}

/// 业务作用：提供页面所需的同源脚本，使严格 CSP 不需要开放外部脚本。
/// 参数说明: 无。
///
/// 返回：随二进制交付的 JavaScript。
async fn javascript() -> Response {
    (
        [("content-type", "text/javascript; charset=utf-8")],
        include_str!("admin/app.js"),
    )
        .into_response()
}

/// 业务作用：提供页面所需的本地样式，避免管理登录依赖 CDN。
/// 参数说明: 无。
///
/// 返回：随二进制交付的 CSS。
async fn stylesheet() -> Response {
    (
        [("content-type", "text/css; charset=utf-8")],
        include_str!("admin/style.css"),
    )
        .into_response()
}

impl IntoResponse for AdminError {
    /// 业务作用：把管理失败转换成不含认证材料的稳定响应。
    /// 参数说明: 无。
    ///
    /// 返回：统一 code/message/request_id 正文、相同的响应头 ID 和禁止缓存；不声明 Cargo 认证挑战。
    fn into_response(self) -> Response {
        self.0.response(false)
    }
}
