#![doc = include_str!("../README.md")]

mod admin;
mod config;
mod http;
mod maintenance;
mod metrics;
mod proxy;
mod upstream;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, OnceLock,
};

use anyhow::Result;
use nasa::application::{
    Application, ApplicationError, ApplicationFuture, ApplicationPhase, ComponentId,
    Initialization, InitializationContext, InitializerKind, InitializerSpec, ManagedResource,
    ShutdownContext,
};
use naxus_core::ArtifactDigest;
use naxus_store::{Store, StoreOptions};
use tokio::sync::Semaphore;

pub use config::bootstrap_loader;

/// 路由与应用资源共享的唯一仓库状态。
pub(crate) struct ServerState {
    config: config::Config,
    store: OnceLock<Arc<Store>>,
    token: OnceLock<Option<ArtifactDigest>>,
    draining: AtomicBool,
    work: Arc<Semaphore>,
    uploads: Arc<Semaphore>,
    downloads: Arc<Semaphore>,
    proxy_budget: Arc<Semaphore>,
    proxy_locks: Vec<tokio::sync::Mutex<()>>,
    login_budget: Arc<Semaphore>,
    login_window: std::sync::Mutex<(std::time::Instant, usize)>,
    dummy_password: OnceLock<String>,
    authorization: nasa::application::PolicyDecisionSnapshot,
    maintenance_wake: tokio::sync::Notify,
    read_readiness: OnceLock<nasa::application::ReadinessContributor>,
    write_readiness: OnceLock<nasa::application::ReadinessContributor>,
    storage_status: std::sync::RwLock<Option<naxus_store::StorageStatus>>,
    metrics: Arc<metrics::Metrics>,
    upstreams: Vec<Arc<upstream::UpstreamRuntime>>,
}

struct RepositoryInitialization(Arc<ServerState>);
struct RepositoryResource(Arc<ServerState>);

/// 业务作用：登记仓库资源和原始协议路由，不另行创建 listener、runtime 或信号所有者。
/// 参数说明：
/// - `app`：NASA 尚未封口的应用容器。
///
/// 返回：配置和路由登记成功后交给框架执行初始化；失败不开放业务请求。
pub fn install(app: &Application) -> Result<()> {
    let config: config::Config = app.config_section("naxus")?;
    let snapshot = app.config();
    let host = snapshot
        .value()
        .pointer("/server/host")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("127.0.0.1");
    config.validate(host)?;
    let upstreams = config
        .proxies
        .iter()
        .flat_map(|proxy| {
            proxy.upstreams.iter().enumerate().map(|(position, _)| {
                Arc::new(upstream::UpstreamRuntime::new(&proxy.name, position))
            })
        })
        .collect::<Vec<_>>();
    let metrics = Arc::new(metrics::Metrics::new(upstreams.clone()));
    let state = Arc::new(ServerState {
        config,
        store: OnceLock::new(),
        token: OnceLock::new(),
        draining: AtomicBool::new(false),
        work: Arc::new(Semaphore::new(32)),
        uploads: Arc::new(Semaphore::new(4)),
        downloads: Arc::new(Semaphore::new(64)),
        proxy_budget: Arc::new(Semaphore::new(16)),
        proxy_locks: (0..64).map(|_| tokio::sync::Mutex::new(())).collect(),
        login_budget: Arc::new(Semaphore::new(4)),
        login_window: std::sync::Mutex::new((std::time::Instant::now(), 0)),
        dummy_password: OnceLock::new(),
        authorization: admin::authorization()?,
        maintenance_wake: tokio::sync::Notify::new(),
        read_readiness: OnceLock::new(),
        write_readiness: OnceLock::new(),
        storage_status: std::sync::RwLock::new(None),
        metrics,
        upstreams,
    });
    app.register_metrics_source(state.metrics.clone())?;
    admin::install(app, state.clone())?;
    http::install(app, state.clone())?;
    app.register_initializer(
        InitializerSpec::new("naxus-repositories").kind(InitializerKind::Hosted),
        RepositoryInitialization(state),
    )?;
    Ok(())
}

impl Initialization for RepositoryInitialization {
    /// 业务作用：在开放监听前独占并恢复数据卷，核对仓库策略及受保护凭据。
    /// 参数说明：
    /// - `context`：框架初始化与反向清理的所有权上下文。
    ///
    /// 返回：完整持久资源可用才登记成功；启动取消或任一校验失败时不开放路由。
    fn initialize<'a>(
        &'a mut self,
        context: &'a mut InitializationContext<'_>,
    ) -> ApplicationFuture<'a> {
        Box::pin(async move {
            let state = self.0.clone();
            let prepared = tokio::task::spawn_blocking(move || -> Result<_> {
                let options = StoreOptions {
                    max_artifact_bytes: state
                        .config
                        .data
                        .max_artifact_bytes
                        .unwrap_or(256 * 1024 * 1024),
                    reserve_free_bytes: state
                        .config
                        .data
                        .hard_free_bytes
                        .unwrap_or(2 * 1024 * 1024 * 1024),
                    ..StoreOptions::default()
                };
                let store = Arc::new(Store::open(state.config.data_root()?, options)?);
                for repository in &state.config.repositories {
                    store.create_repository(&repository.name, repository.policy)?;
                }
                for proxy in &state.config.proxies {
                    store.create_proxy(&proxy.name, &proxy.logical_index)?;
                }
                admin::bootstrap(&state.config, &store)?;
                let dummy = nasa::crypto::bcrypt(&nasa::crypto::random_ascii(48))?;
                Ok((store, state.config.token_digest()?, dummy))
            })
            .await
            .map_err(initialization_error)?
            .map_err(initialization_error)?;
            self.0
                .store
                .set(prepared.0)
                .map_err(|_| initialization_error("repository initialized twice"))?;
            self.0
                .token
                .set(prepared.1)
                .map_err(|_| initialization_error("authentication initialized twice"))?;
            self.0
                .dummy_password
                .set(prepared.2)
                .map_err(|_| initialization_error("password verification initialized twice"))?;
            let read = context.register_readiness(
                "repository-read",
                nasa::application::ReadinessPolicy {
                    affects_ready: true,
                    failure_threshold: 1,
                    recovery_threshold: 1,
                    stale_after: Some(std::time::Duration::from_secs(90)),
                },
            )?;
            read.observe(
                nasa::application::DependencyState::Ready,
                "repository_initialized",
                std::time::Instant::now(),
            );
            let write = context.register_readiness(
                "repository-write",
                nasa::application::ReadinessPolicy {
                    affects_ready: false,
                    failure_threshold: 1,
                    recovery_threshold: 1,
                    stale_after: Some(std::time::Duration::from_secs(90)),
                },
            )?;
            write.observe(
                nasa::application::DependencyState::Ready,
                "storage_initialized",
                std::time::Instant::now(),
            );
            self.0
                .read_readiness
                .set(read)
                .map_err(|_| initialization_error("read readiness initialized twice"))?;
            self.0
                .write_readiness
                .set(write)
                .map_err(|_| initialization_error("write readiness initialized twice"))?;
            context.register_managed_resource(None, RepositoryResource(self.0.clone()))?;
            Ok(())
        })
    }

    /// 业务作用：把仓库维护工厂交给 NASA，只在完整 Ready 后启动长期任务。
    /// 参数说明：
    /// - `context`：当前 hosted initializer 的任务登记边界。
    ///
    /// 返回：关键维护任务由框架持有并统一取消，不提前创建独立工作者。
    fn after<'a>(
        &'a mut self,
        context: &'a mut InitializationContext<'_>,
    ) -> ApplicationFuture<'a> {
        Box::pin(async move {
            let state = self.0.clone();
            context.stage_critical("maintenance", move |cancel| maintenance::run(state, cancel))
        })
    }
}

impl ManagedResource for RepositoryResource {
    /// 业务作用：停机或启动回滚时关闭新写入受理，已提交对象仍保留在持久卷。
    /// 参数说明：
    /// - `_context`：框架统一清理预算。
    ///
    /// 返回：同步发布排空状态，不将取消解释为发布成功。
    fn shutdown<'a>(&'a mut self, _context: &'a ShutdownContext) -> ApplicationFuture<'a> {
        Box::pin(async move {
            // 在资源释放前撤销新写入权，防止请求继续创建没有生命周期所有者的上传。
            self.0.draining.store(true, Ordering::Release);
            Ok(())
        })
    }
}

/// 业务作用：把仓库启动失败归入统一的初始化阶段，由框架完成反向清理。
/// 参数说明：
/// - `error`：内部配置、文件或数据库失败。
///
/// 返回：不包含凭据正文的生命周期错误。
fn initialization_error(error: impl std::fmt::Display) -> ApplicationError {
    ApplicationError::new(
        ComponentId::Application,
        ApplicationPhase::Initialization,
        error.to_string(),
    )
}
