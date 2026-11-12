use std::{io::Read, path::PathBuf, sync::OnceLock};

use anyhow::{ensure, Context, Result};
use nasa::yml::strict::{ConfigLoader, ProfileSelection};
use naxus_core::RepositoryPolicy;
use serde::Deserialize;
use url::Url;

/// 仓库服务的不可变引导设置。
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub public_base_url: Url,
    #[serde(default)]
    pub data: DataConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    pub repositories: Vec<RepositoryConfig>,
    #[serde(default = "default_proxies")]
    pub proxies: Vec<ProxyConfig>,
    #[serde(default)]
    pub admin: AdminConfig,
}

/// 同端口管理入口的首启材料，不包含公开默认口令。
#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AdminConfig {
    pub bootstrap_password_file: Option<PathBuf>,
}

/// 独立公开来源与保持顺序的同源镜像链。
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProxyConfig {
    pub name: String,
    pub logical_index: String,
    pub upstreams: Vec<UpstreamConfig>,
    #[serde(default = "index_ttl")]
    pub index_ttl_seconds: u64,
}

/// 上游索引及其显式允许的包体主机，私网访问必须由部署者单独允许。
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpstreamConfig {
    pub index: Url,
    #[serde(default)]
    pub download_hosts: Vec<String>,
    #[serde(default)]
    pub allow_private_network: bool,
    #[serde(default = "upstream_timeout")]
    pub request_timeout_seconds: u64,
    #[serde(default = "cooldown_limit")]
    pub max_cooldown_seconds: u64,
}

/// 业务作用：限制上游 Retry-After 对后续业务请求的影响时长。
/// 参数说明: 无。
///
/// 返回：默认最多冷却五分钟；冷却期间立即尝试下一镜像，不占用等待任务。
fn cooldown_limit() -> u64 {
    300
}

/// 业务作用：限制单次上游 HTTP 阶段，给后续镜像保留尝试时间。
/// 参数说明: 无。
///
/// 返回：默认三十秒，全部镜像仍共享一百二十秒总期限。
fn upstream_timeout() -> u64 {
    30
}

/// 业务作用：为公开索引设置有界刷新间隔，不让每次包体下载重复访问上游。
/// 参数说明: 无。
///
/// 返回：默认五分钟的索引新鲜度预算。
fn index_ttl() -> u64 {
    300
}

/// 业务作用：在没有显式代理列表时建立 crates.io 的精确镜像来源。
/// 参数说明: 无。
///
/// 返回：只有 crates.io 的默认代理；显式列表不会隐含追加默认上游。
fn default_proxies() -> Vec<ProxyConfig> {
    vec![ProxyConfig {
        name: "crates-io".to_owned(),
        logical_index: naxus_registry::CRATES_IO_INDEX.to_owned(),
        upstreams: vec![UpstreamConfig {
            index: Url::parse("https://index.crates.io/").expect("static URL"),
            download_hosts: vec!["static.crates.io".to_owned(), "crates.io".to_owned()],
            allow_private_network: false,
            request_timeout_seconds: upstream_timeout(),
            max_cooldown_seconds: cooldown_limit(),
        }],
        index_ttl_seconds: index_ttl(),
    }]
}

/// 本地持久数据位置与容量预算。
#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct DataConfig {
    pub root: PathBuf,
    pub hard_free_bytes: Option<u64>,
    pub max_artifact_bytes: Option<u64>,
}

/// Registry 的显式认证模式；无认证只允许回环部署。
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuthConfig {
    pub mode: String,
    pub token_file: Option<PathBuf>,
}

impl Default for AuthConfig {
    /// 业务作用：默认本地模式不要求部署外部凭据，监听约束另行强制校验。
    /// 参数说明: 无。
    ///
    /// 返回：仅可用于回环监听的 none 模式。
    fn default() -> Self {
        Self {
            mode: "none".to_owned(),
            token_file: None,
        }
    }
}

/// 启动时建立或核对的 hosted 仓库。
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RepositoryConfig {
    pub name: String,
    pub policy: RepositoryPolicy,
}

impl Config {
    /// 业务作用：在资源初始化前拒绝无认证远程发布和不能稳定生成来源地址的配置。
    /// 参数说明：
    /// - `host`：框架实际使用的监听主机。
    ///
    /// 返回：监听、外部来源、认证和仓库配置一致时成功。
    pub(crate) fn validate(&self, host: &str) -> Result<()> {
        let local = |value: &str| {
            value == "localhost"
                || value
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        };
        // 对外 URL 决定凭据传输边界；反向代理后的内部 listener 可以继续使用 HTTP。
        naxus_core::validate_service_url(&self.public_base_url)?;
        ensure!(
            matches!(self.auth.mode.as_str(), "none" | "token" | "managed"),
            "unsupported registry authentication mode"
        );
        if self.auth.mode == "none" {
            let advertised_local = match self.public_base_url.host() {
                Some(url::Host::Domain(name)) => name == "localhost",
                Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                None => false,
            };
            // 无凭据发布只能存在于本机边界，不能被对外监听或对外来源配置意外放大。
            ensure!(
                local(host.trim_matches(['[', ']'])) && advertised_local,
                "remote repository access requires authentication"
            );
        } else if self.auth.mode == "token" {
            ensure!(
                self.auth.token_file.is_some(),
                "token mode requires a protected token_file"
            );
        }
        ensure!(
            !self.repositories.is_empty() && self.repositories.len() <= 128,
            "invalid repository count"
        );
        ensure!(
            self.public_base_url
                .path()
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric()
                    || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~')),
            "public URL path must use unreserved path segments"
        );
        for repository in &self.repositories {
            naxus_core::validate_name(&repository.name)?;
        }
        ensure!(self.proxies.len() <= 32, "too many proxy sources");
        let mut names = std::collections::BTreeSet::new();
        let mut sources = std::collections::BTreeSet::new();
        for proxy in &self.proxies {
            naxus_core::validate_name(&proxy.name)?;
            ensure!(
                proxy.name != "naxus" && !proxy.name.starts_with("naxus-proxy-"),
                "reserved proxy name"
            );
            let identity = naxus_core::registry_identity(&proxy.logical_index)?;
            ensure!(
                proxy.name != "crates-io" || identity == naxus_registry::CRATES_IO_INDEX,
                "crates-io proxy must preserve the canonical source"
            );
            ensure!(
                names.insert(&proxy.name) && sources.insert(identity),
                "duplicate proxy identity"
            );
            ensure!(
                !proxy.upstreams.is_empty()
                    && proxy.upstreams.len() <= 16
                    && proxy.index_ttl_seconds <= 86400,
                "invalid proxy budget"
            );
            let logical = proxy
                .logical_index
                .strip_prefix("sparse+")
                .unwrap_or(&proxy.logical_index);
            let logical = Url::parse(logical)?;
            ensure!(
                logical.scheme() == "https"
                    || (logical.scheme() == "http"
                        && proxy
                            .upstreams
                            .iter()
                            .all(|upstream| upstream.allow_private_network)),
                "invalid proxy logical source"
            );
            for upstream in &proxy.upstreams {
                ensure!(
                    (1..=3600).contains(&upstream.max_cooldown_seconds),
                    "invalid upstream cooldown limit"
                );
                ensure!(
                    (1..=120).contains(&upstream.request_timeout_seconds),
                    "invalid upstream request timeout"
                );
                ensure!(
                    upstream.index.username().is_empty()
                        && upstream.index.password().is_none()
                        && upstream.index.query().is_none()
                        && upstream.index.fragment().is_none()
                        && upstream.index.path().ends_with('/'),
                    "invalid upstream index URL"
                );
                ensure!(
                    upstream.index.scheme() == "https"
                        || (upstream.index.scheme() == "http" && upstream.allow_private_network),
                    "upstream requires HTTPS"
                );
                ensure!(
                    upstream.download_hosts.len() <= 32
                        && upstream
                            .download_hosts
                            .iter()
                            .all(|host| !host.is_empty() && !host.contains(['/', ':', '@', '*'])),
                    "invalid upstream download host"
                );
            }
        }
        Ok(())
    }

    /// 业务作用：为本机和容器选取同一合同下的数据根，不把源码目录当作制品缓存。
    /// 参数说明: 无。
    ///
    /// 返回：显式数据根或用户目录下的独立仓库；无法确定默认用户目录时拒绝启动。
    pub(crate) fn data_root(&self) -> Result<PathBuf> {
        if !self.data.root.as_os_str().is_empty() {
            return Ok(self.data.root.clone());
        }
        Ok(PathBuf::from(
            std::env::var_os("HOME").context("cannot locate default repository directory")?,
        )
        .join(".naxus/repository"))
    }

    /// 业务作用：从可信只读材料生成内存中的凭据摘要，不保留或输出令牌正文。
    /// 参数说明: 无。
    ///
    /// 返回：none 模式为空，token 模式返回校验摘要；材料缺失、过大或强度不足时拒绝启动。
    pub(crate) fn token_digest(&self) -> Result<Option<naxus_core::ArtifactDigest>> {
        if self.auth.mode != "token" {
            return Ok(None);
        }
        let path = self
            .auth
            .token_file
            .as_ref()
            .context("missing token file")?;
        let metadata = std::fs::symlink_metadata(path)?;
        ensure!(
            metadata.file_type().is_file() && metadata.len() <= 1024,
            "invalid token material file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                metadata.permissions().mode() & 0o077 == 0,
                "token file must be readable only by its owner"
            );
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(1025)
            .read_to_end(&mut bytes)?;
        let material = nasa::secret::SecretBytes::new(bytes);
        ensure!(material.len() <= 1024, "token material exceeds size limit");
        let material = material.expose().trim_ascii();
        ensure!(
            (32..=512).contains(&material.len())
                && material.iter().all(|byte| byte.is_ascii_graphic()),
            "invalid token material"
        );
        Ok(Some(naxus_core::ArtifactDigest::of(material)))
    }
}

/// 业务作用：在框架 preflight 前按显式参数、APP_CONFIG_FILE 和默认路径选择主配置，固定环境与 YAML 语义。
/// 参数说明: 无。
///
/// 返回：仓库命令对应的严格加载器；帮助或非法命令在创建运行时前结束。
pub fn bootstrap_loader() -> nasa::yml::strict::Result<ConfigLoader> {
    static LOADER: OnceLock<nasa::yml::strict::Result<ConfigLoader>> = OnceLock::new();
    LOADER
        .get_or_init(|| {
            let arguments = std::env::args().skip(1).collect::<Vec<_>>();
            if arguments
                .first()
                .is_some_and(|argument| matches!(argument.as_str(), "backup" | "restore"))
            {
                match backup_command(&arguments) {
                    Ok(()) => std::process::exit(0),
                    Err(error) => {
                        eprintln!("naxus: {error:#}");
                        std::process::exit(1);
                    }
                }
            }
            if arguments
                .first()
                .is_some_and(|argument| argument == "healthcheck")
            {
                let code = if healthcheck(&arguments[1..]).is_ok() {
                    0
                } else {
                    1
                };
                std::process::exit(code);
            }
            if arguments == ["--version"] {
                println!("naxus {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            if arguments == ["--help"] || arguments == ["-h"] {
                println!(concat!(
                    "naxus serve [--config <application.yml>]\n",
                    "naxus healthcheck [--url <loopback-readyz-URL>]\n",
                    "naxus backup --data-dir <stopped-data-root> --output <new-directory>\n",
                    "naxus restore --backup <backup-directory> --data-dir <new-data-root>\n\n",
                    "Admin and Cargo registry share server.port.\n",
                    "Configuration: --config > APP_CONFIG_FILE > zcf/application.yml.\n",
                    "An unreadable or invalid selected file prevents startup."
                ));
                std::process::exit(0);
            }
            let loader = ConfigLoader::standard()?.profile(ProfileSelection::Disabled);
            match arguments.as_slice() {
                [] => prepare_loader(loader),
                [command] if command == "serve" => prepare_loader(loader),
                [command, flag, path] if command == "serve" && flag == "--config" => {
                    prepare_loader(loader.base_file(path)?)
                }
                _ => {
                    eprintln!("usage: naxus serve [--config <application.yml>]");
                    std::process::exit(2);
                }
            }
        })
        .clone()
}

/// 业务作用：在独占停服数据根时生成或恢复一致备份，不启动监听器或重置仓库身份。
/// 参数说明：
/// - `arguments`：明确的源目录与全新目标目录。
///
/// 返回：文件、元数据及引用全部核验成功才退出零；已有服务占用或目标存在时拒绝。
fn backup_command(arguments: &[String]) -> Result<()> {
    use std::path::Path;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let _entered = runtime.enter();
    match arguments {
        [command, input, root, output, destination] if command == "backup" && input == "--data-dir" && output == "--output" => {
            ensure!(Path::new(root).join("naxus.sqlite3").is_file(), "source repository does not exist");
            let store = naxus_store::Store::open(root, naxus_store::StoreOptions::default())?;
            let manifest = store.backup(Path::new(destination))?;
            println!("Backed up service {} with {} objects", manifest.service_uuid, manifest.objects.len());
        }
        [command, input, backup, output, destination] if command == "restore" && input == "--backup" && output == "--data-dir" => {
            naxus_store::Store::restore(Path::new(backup), Path::new(destination), naxus_store::StoreOptions::default())?;
            println!("Restored Naxus data root");
        }
        _ => anyhow::bail!("usage: naxus backup --data-dir <stopped-data-root> --output <new-directory>; naxus restore --backup <backup-directory> --data-dir <new-data-root>"),
    }
    Ok(())
}

/// 业务作用：为容器和进程管理器执行有界的本机 readiness 检查，不启动第二个服务或日志系统。
/// 参数说明：
/// - `arguments`：可选的显式回环 readiness URL。
///
/// 返回：五秒内取得成功状态才成功，禁止重定向与环境代理，不读取认证材料。
fn healthcheck(arguments: &[String]) -> Result<()> {
    let url = match arguments {
        [] => std::env::var("NAXUS_HEALTH_URL").unwrap_or_else(|_| {
            format!(
                "http://127.0.0.1:{}/readyz",
                std::env::var("NAXUS_PORT").unwrap_or_else(|_| "7443".to_owned())
            )
        }),
        [option, value] if option == "--url" => value.clone(),
        _ => anyhow::bail!("invalid healthcheck arguments"),
    };
    let url = Url::parse(&url)?;
    let local = match url.host() {
        Some(url::Host::Domain(name)) => name == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    ensure!(
        local
            && matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "healthcheck requires an unauthenticated loopback URL"
    );
    let response = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(2))
        .timeout(std::time::Duration::from_secs(5))
        .build()?
        .get(url)
        .send()?;
    ensure!(response.status().is_success(), "repository is not ready");
    Ok(())
}

static PREPARED_STORE: std::sync::Mutex<Option<(PathBuf, naxus_store::PreparedStore)>> =
    std::sync::Mutex::new(None);

/// 业务作用：在 db 组件建连前取得数据卷权威，并核对数据库配置与仓库文件布局一致。
/// 参数说明：
/// - `loader`：已经固定文件位置与环境快照的严格配置入口。
///
/// 返回：完成旧配置默认值装配的入口；不满足持久化合同或实例冲突时在任何数据库连接前拒绝。
fn prepare_loader(loader: ConfigLoader) -> nasa::yml::strict::Result<ConfigLoader> {
    use nasa::yml::strict::{ConfigError, ConfigPath, ErrorKind};
    let (loader, loaded) = legacy_datasource_defaults(loader)?;
    let config: Config = loaded.bind_at(&ConfigPath::parse("naxus")?)?;
    let result = (|| -> Result<()> {
        config.validate(
            loaded
                .tree()
                .pointer("/server/host")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("127.0.0.1"),
        )?;
        validate_datasource(loaded.tree(), &config)?;
        let root = config.data_root()?;
        let prepared = naxus_store::Store::prepare(&root, store_options(&config))?;
        let mut slot = PREPARED_STORE
            .lock()
            .map_err(|_| anyhow::anyhow!("repository preparation unavailable"))?;
        ensure!(slot.is_none(), "repository preparation already exists");
        *slot = Some((root.canonicalize()?, prepared));
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("naxus storage preparation: {error:#}");
        return Err(ConfigError::new(ErrorKind::Validation));
    }
    Ok(loader)
}

/// 业务作用：为未声明数据源的旧配置补充仓库 SQLite 默认值，保持原文件和冻结环境的装配边界。
///
/// 参数说明：
/// - `loader`：已选定主文件、关闭 profile 的严格加载器。
///
/// 返回：完整数据源配置及加载器；显式数据源不被覆盖，来源变化、编码或配置不合法时拒绝启动。
fn legacy_datasource_defaults(
    loader: ConfigLoader,
) -> nasa::yml::strict::Result<(ConfigLoader, nasa::yml::strict::LoadedConfig)> {
    use std::collections::BTreeMap;

    use nasa::yml::strict::{ConfigError, ConfigPath, EnvironmentSnapshot, ErrorKind, ValueHint};

    let loaded = loader.load()?;
    if loaded.tree().pointer("/datasources/naxus").is_some() {
        // 显式配置必须自行满足持久化约束，不能用默认值掩盖错误驱动、路径或连接数。
        return Ok((loader, loaded));
    }
    if loaded
        .tree()
        .get("datasources")
        .is_some_and(|value| !value.is_object())
    {
        return Err(ConfigError::new(ErrorKind::Binding).at(&ConfigPath::parse("datasources")?));
    }
    let config: Config = loaded.bind_at(&ConfigPath::parse("naxus")?)?;
    let mut environment = BTreeMap::new();
    for (name, _) in std::env::vars_os() {
        let Ok(name) = name.into_string() else {
            continue;
        };
        if let Some(value) = loader.environment().get(&name)? {
            // 仅枚举名称；值始终取自原加载器快照，不接受两次装配之间出现的新环境值。
            environment.insert(name, value.to_owned());
        }
    }
    let mut policy = loader.load_policy().clone();
    let root = if config.data.root.as_os_str().is_empty() {
        let home = loader
            .environment()
            .get("HOME")?
            .ok_or_else(|| ConfigError::new(ErrorKind::Missing))?;
        let root = PathBuf::from(home).join(".naxus/repository");
        environment.retain(|name, _| !name.eq_ignore_ascii_case("APP__NAXUS__DATA__ROOT"));
        environment.insert(
            "APP__NAXUS__DATA__ROOT".to_owned(),
            root.to_str()
                .ok_or_else(|| ConfigError::new(ErrorKind::Encoding))?
                .to_owned(),
        );
        policy
            .hints
            .insert(ConfigPath::parse("naxus.data.root")?, ValueHint::Literal);
        root
    } else {
        config.data.root
    };
    let database = std::path::absolute(root.join("naxus.sqlite3"))
        .map_err(|_| ConfigError::new(ErrorKind::Unreadable))?;
    let path = database
        .to_str()
        .ok_or_else(|| ConfigError::new(ErrorKind::Encoding))?;
    for (name, value) in [
        ("DRIVER", "sqlite".to_owned()),
        ("SQLITE__PATH", path.to_owned()),
        ("SQLITE__CREATE_IF_MISSING", "true".to_owned()),
        ("SQLITE__JOURNAL_MODE", "wal".to_owned()),
        ("SQLITE__SYNCHRONOUS", "full".to_owned()),
        ("SQLITE__FOREIGN_KEYS", "true".to_owned()),
        ("SQLITE__BUSY_TIMEOUT_MS", "5000".to_owned()),
        ("MIN_CONNECTIONS", "1".to_owned()),
        ("MAX_CONNECTIONS", "1".to_owned()),
        ("TRANSACTION_TIMEOUT_MS", "300000".to_owned()),
        ("MAX_RESULT_ROWS", "1000000".to_owned()),
        ("MAX_RESULT_BYTES", "268435456".to_owned()),
        (
            "MIN_FREE_BYTES",
            config
                .data
                .hard_free_bytes
                .unwrap_or(2 * 1024 * 1024 * 1024)
                .to_string(),
        ),
    ] {
        environment.insert(format!("APP__DATASOURCES__NAXUS__{name}"), value);
        if !matches!(name, "DRIVER" | "SQLITE__PATH") {
            policy.hints.insert(
                ConfigPath::parse(&format!(
                    "datasources.naxus.{}",
                    name.to_ascii_lowercase().replace("__", ".")
                ))?,
                ValueHint::Scalar,
            );
        }
    }
    policy.hints.insert(
        ConfigPath::parse("datasources.naxus.sqlite.path")?,
        ValueHint::Literal,
    );
    let base = loader
        .base_path()
        .ok_or_else(|| ConfigError::new(ErrorKind::Missing))?;
    let compatible = ConfigLoader::memory(EnvironmentSnapshot::from_pairs(environment)?)
        .base_file(base)?
        .profile(ProfileSelection::Disabled)
        .policy(policy);
    let candidate = compatible.load()?;
    // 默认值只适用于已观察到的旧来源；装配期间文件被替换时，必须重新启动以复验显式配置。
    if candidate.source_fingerprint() != loaded.source_fingerprint() {
        return Err(ConfigError::new(ErrorKind::SourceChanged));
    }
    Ok((compatible, candidate))
}

/// 业务作用：约束受管数据源和对象根保持同一持久身份，避免元数据与文件落在不同目录。
/// 参数说明：
/// - `tree`：宿主使用的完整配置。
/// - `config`：仓库业务配置。
///
/// 返回：路径、driver、同步和连接策略符合仓库合同才成功。
pub(crate) fn validate_datasource(tree: &serde_json::Value, config: &Config) -> Result<()> {
    let source = tree
        .pointer("/datasources/naxus")
        .context("datasources.naxus must configure the repository SQLite datasource")?;
    ensure!(
        source.get("driver").and_then(serde_json::Value::as_str) == Some("sqlite"),
        "repository datasource driver must be sqlite"
    );
    let path = source
        .pointer("/sqlite/path")
        .and_then(serde_json::Value::as_str)
        .context("datasources.naxus.sqlite.path is required")?;
    let expected = std::path::absolute(config.data_root()?.join("naxus.sqlite3"))?;
    ensure!(
        std::path::absolute(path)? == expected,
        "repository datasource path must be naxus.data.root/naxus.sqlite3"
    );
    ensure!(
        source
            .pointer("/sqlite/journal_mode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("wal")
            == "wal",
        "repository requires WAL journal mode"
    );
    ensure!(
        source
            .pointer("/sqlite/synchronous")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("full")
            == "full",
        "repository requires FULL synchronous mode"
    );
    ensure!(
        source
            .pointer("/sqlite/foreign_keys")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        "repository requires foreign keys"
    );
    ensure!(
        !source
            .pointer("/sqlite/read_only")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        "repository requires a writable datasource"
    );
    ensure!(
        source
            .get("max_connections")
            .and_then(serde_json::Value::as_u64)
            == Some(1),
        "repository requires one metadata connection"
    );
    Ok(())
}

/// 业务作用：将仓库容量设置传给存储层，保留制品与元数据的持久化预算。
/// 参数说明：
/// - `config`：已验证的业务配置。
///
/// 返回：使用显式值或既有默认值的存储预算。
fn store_options(config: &Config) -> naxus_store::StoreOptions {
    naxus_store::StoreOptions {
        max_artifact_bytes: config.data.max_artifact_bytes.unwrap_or(256 * 1024 * 1024),
        reserve_free_bytes: config
            .data
            .hard_free_bytes
            .unwrap_or(2 * 1024 * 1024 * 1024),
        ..Default::default()
    }
}

/// 业务作用：把启动前取得的唯一实例权威交给初始化，配置变化时拒绝绑定另一个数据根。
/// 参数说明：
/// - `config`：Application 最终装配的业务配置。
///
/// 返回：只交付一次的准备对象；缺失或根目录变化时失败。
pub(crate) fn take_prepared(config: &Config) -> Result<naxus_store::PreparedStore> {
    let mut slot = PREPARED_STORE
        .lock()
        .map_err(|_| anyhow::anyhow!("repository preparation unavailable"))?;
    let (root, prepared) = slot
        .take()
        .context("repository must use bootstrap_loader before db startup")?;
    ensure!(
        root == config.data_root()?.canonicalize()?,
        "repository root changed during startup"
    );
    Ok(prepared)
}
