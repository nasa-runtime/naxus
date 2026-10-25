use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use anyhow::{ensure, Context, Result};
use naxus_client::ClientConfig;
use uuid::Uuid;

/// 单个命令使用的临时目录，不修改 Cargo 全局缓存或业务 manifest。
pub(crate) struct OperationDirectory(pub PathBuf);

impl OperationDirectory {
    /// 业务作用：在工作区文件系统中创建本命令的独立解析或发布目录。
    /// 参数说明：
    /// - `root`：已经持有客户端锁的工作区根。
    ///
    /// 返回：精确 UUID 目录，不使用系统临时根。
    pub fn create(root: &Path) -> Result<Self> {
        let path = root
            .join(".naxus/transactions")
            .join(Uuid::new_v4().to_string());
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for OperationDirectory {
    /// 业务作用：命令正常返回时清理自己的 UUID 操作目录，不触碰活动恢复日志材料。
    /// 参数说明: 无。
    ///
    /// 返回：只尝试删除本命令已创建的精确目录；失败保留后交给后续维护。
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// 业务作用：使用启动此插件的 Cargo，避免同一命令混入默认的旧工具链。
/// 参数说明: 无。
///
/// 返回：受父 Cargo 或 rustup 工具链选择约束的可执行程序。
pub fn executable() -> OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}

/// 业务作用：在解析前拒绝不支持事务专属 Cargo.lock 的工具链。
/// 参数说明: 无。
///
/// 返回：Cargo 至少为 1.97 时成功，不改变用户默认工具链。
pub fn check_version() -> Result<()> {
    let output = Command::new(executable()).arg("--version").output()?;
    ensure!(output.status.success(), "cannot inspect Cargo version");
    let version = String::from_utf8(output.stdout)?;
    let version = version
        .split_whitespace()
        .nth(1)
        .context("unrecognized Cargo version")?;
    ensure!(
        semver::Version::parse(version)? >= semver::Version::new(1, 97, 0),
        "cargo naxus requires Cargo 1.97 or newer"
    );
    Ok(())
}

/// 业务作用：在 Cargo 有机会访问网络前拒绝绕过单入口的 patch、replace 和递归配置入口。
/// 参数说明：
/// - `root`：真实工作区根。
///
/// 返回：当前 manifest、祖先 Cargo 配置和 Cargo home 不含不受管解析覆盖才成功。
pub fn validate_resolution_configuration(root: &Path) -> Result<()> {
    let mut files = vec![root.join("Cargo.toml")];
    for parent in root.ancestors() {
        files.push(parent.join(".cargo/config.toml"));
        files.push(parent.join(".cargo/config"));
    }
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")));
    if let Some(home) = cargo_home {
        files.push(home.join("config.toml"));
        files.push(home.join("config"));
    }
    for path in files {
        if !path.try_exists()? {
            continue;
        }
        ensure!(
            fs::metadata(&path)?.len() <= 1024 * 1024,
            "Cargo resolution configuration exceeds limit"
        );
        let value: toml::Value = toml::from_str(&fs::read_to_string(&path)?)
            .map_err(|_| anyhow::anyhow!("invalid Cargo resolution configuration"))?;
        // 这些覆盖可在 registry 映射之外引入 Git 或本地替代，固定索引不能证明其最终来源。
        ensure!(value.get("patch").is_none() && value.get("replace").is_none() && value.get("include").is_none(), "Cargo patch, replace and configuration includes are not supported by the Naxus single-endpoint policy");
    }
    Ok(())
}

/// 业务作用：在生成完整快照映射前，用稳定逻辑 registry 定位真实工作区。
/// 参数说明：
/// - `config`：唯一服务与仓库配置。
/// - `manifest`：可选用户 manifest。
///
/// 返回：规范化工作区根，不解析或改写依赖锁文件。
pub fn workspace(config: &ClientConfig, manifest: Option<&Path>) -> Result<PathBuf> {
    let mapping = format!(
        "registries.naxus.index={}",
        toml::Value::String(format!(
            "sparse+{}/repositories/{}/index/",
            config.service.endpoint.as_str().trim_end_matches('/'),
            config.service.repository
        ))
    );
    let mut command = Command::new(executable());
    command.args([
        "locate-project",
        "--workspace",
        "--message-format",
        "plain",
        "--config",
        &mapping,
    ]);
    for source in &config.sources {
        command.arg("--config").arg(format!(
            "registries.{}.index={}",
            source.name,
            toml::Value::String(source.logical_index.clone())
        ));
    }
    if let Some(manifest) = manifest {
        command.arg("--manifest-path").arg(manifest);
    }
    let output = checked(&mut command)?;
    let manifest = PathBuf::from(String::from_utf8(output.stdout)?.trim());
    Ok(manifest
        .parent()
        .context("Cargo returned a manifest without parent")?
        .canonicalize()?)
}

/// 业务作用：读取稳定选项的显式值，不经 shell 展开用户参数。
/// 参数说明：
/// - `arguments`：原始命令参数。
/// - `key`：需要读取的选项名。
///
/// 返回：该选项后面的值，未提供则为 None。
pub fn option<'a>(arguments: &'a [String], key: &str) -> Option<&'a str> {
    arguments
        .iter()
        .position(|value| value == key)
        .and_then(|index| arguments.get(index + 1))
        .map(String::as_str)
}

/// 业务作用：构造传给 Cargo 的独立配置，固定私有快照和公开代理单入口。
/// 参数说明：
/// - `config`：项目服务映射。
/// - `configuration`：credential provider 使用的项目配置。
/// - `index`：已选私有索引 URL。
/// - `lock`：可选的事务 Cargo.lock 绝对路径。
///
/// 返回：不包含凭据正文的完整 Cargo 配置文本。
pub fn configuration(
    config: &ClientConfig,
    configuration: &Path,
    index: &str,
    lock: Option<&Path>,
) -> Result<String> {
    configuration_with_sessions(config, configuration, index, lock, &BTreeMap::new())
}

/// 业务作用：生成一次解析的 hosted 与全部公开来源映射，业务网络只连接当前 Naxus。
/// 参数说明：
/// - `config`：服务端确认过的来源身份。
/// - `configuration`：credential provider 的稳定项目配置。
/// - `index`：固定的 hosted 快照。
/// - `lock`：解析时使用的事务 Cargo.lock。
/// - `sessions`：公开来源名称到短期 session UUID 的映射。
///
/// 返回：保留逻辑来源身份且不包含凭据的 Cargo 配置。
pub fn configuration_with_sessions(
    config: &ClientConfig,
    configuration: &Path,
    index: &str,
    lock: Option<&Path>,
    sessions: &BTreeMap<String, String>,
) -> Result<String> {
    let sibling = std::env::current_exe()?
        .parent()
        .context("client executable has no parent")?
        .join(if cfg!(windows) {
            "cargo-credential-naxus.exe"
        } else {
            "cargo-credential-naxus"
        });
    let provider = if sibling.is_file() {
        sibling
    } else {
        PathBuf::from("cargo-credential-naxus")
    };
    let arguments = vec![
        provider.to_string_lossy().into_owned(),
        "--config".to_owned(),
        configuration.to_string_lossy().into_owned(),
    ];
    let mut value = toml::Table::new();
    let mut registries = toml::Table::new();
    let mut mapped = vec![("naxus".to_owned(), index.to_owned())];
    let mut sources = toml::Table::new();
    for source in &config.sources {
        let local = format!("naxus-proxy-{}", source.id);
        let suffix = sessions
            .get(&source.name)
            .map(|id| format!("sessions/{id}/"))
            .unwrap_or_default();
        mapped.push((
            local.clone(),
            format!(
                "sparse+{}/proxies/{}/{suffix}index/",
                config.service.endpoint.as_str().trim_end_matches('/'),
                source.name
            ),
        ));
        let crates_io = naxus_core::registry_identity(&source.logical_index)?
            == naxus_registry::CRATES_IO_INDEX;
        let original = if crates_io {
            "crates-io".to_owned()
        } else {
            format!("naxus-original-{}", source.id)
        };
        let mut replacement = toml::Table::new();
        replacement.insert("replace-with".to_owned(), toml::Value::String(local));
        if !crates_io {
            replacement.insert(
                "registry".to_owned(),
                toml::Value::String(source.logical_index.clone()),
            );
            mapped.push((source.name.clone(), source.logical_index.clone()));
        }
        sources.insert(original, toml::Value::Table(replacement));
    }
    if !sources.contains_key("crates-io") {
        // 没有启用公开代理时仍封住默认外连，不能因为配置缺失而绕过唯一入口。
        let alias = "naxus-unconfigured-public".to_owned();
        mapped.push((
            alias.clone(),
            format!(
                "sparse+{}/proxies/crates-io/index/",
                config.service.endpoint.as_str().trim_end_matches('/')
            ),
        ));
        let mut replacement = toml::Table::new();
        replacement.insert("replace-with".to_owned(), toml::Value::String(alias));
        sources.insert("crates-io".to_owned(), toml::Value::Table(replacement));
    }
    for (name, index) in mapped {
        let mut registry = toml::Table::new();
        registry.insert("index".to_owned(), toml::Value::String(index));
        registry.insert(
            "credential-provider".to_owned(),
            toml::Value::try_from(&arguments)?,
        );
        registries.insert(name, toml::Value::Table(registry));
    }
    value.insert("registries".to_owned(), toml::Value::Table(registries));
    value.insert("source".to_owned(), toml::Value::Table(sources));
    if let Some(lock) = lock {
        let mut resolver = toml::Table::new();
        resolver.insert(
            "lockfile-path".to_owned(),
            toml::Value::String(lock.to_string_lossy().into_owned()),
        );
        value.insert("resolver".to_owned(), toml::Value::Table(resolver));
    }
    Ok(toml::to_string_pretty(&value)?)
}

/// 业务作用：调用 Cargo 并保存其诊断，不将构建失败误报成已完成。
/// 参数说明：
/// - `command`：已经按参数数组构造的 Cargo 进程。
///
/// 返回：成功退出的原始输出；失败只输出 Cargo 自身的 stderr。
pub fn checked(command: &mut Command) -> Result<Output> {
    let output = command.output()?;
    ensure!(
        output.status.success(),
        "Cargo failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}

/// 业务作用：执行已经固定来源的用户 Cargo 命令并保留交互输出。
/// 参数说明：
/// - `root`：业务工作区根。
/// - `command`：build/check/test/run。
/// - `configuration`：本次固定来源配置。
/// - `arguments`：原始业务构建参数。
///
/// 返回：实际构建或运行的退出码，命令期间不刷新 HEAD。
pub fn execute(
    root: &Path,
    command: &str,
    configuration: &Path,
    arguments: &[String],
) -> Result<u8> {
    let mut process = Command::new(executable());
    process
        .current_dir(root)
        .arg(command)
        .arg("--config")
        .arg(configuration)
        .arg("--locked")
        .args(arguments)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    Ok(process.status()?.code().unwrap_or(1).clamp(0, 255) as u8)
}
