mod build;
mod cargo;
mod lease;
mod publication;
mod publish;
mod remote;

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{ensure, Context, Result};
use naxus_client::{ClientConfig, ServiceConfig};

/// 业务作用：提供 Cargo 子命令入口，统一快照构建与原子发布，保留原构建退出状态。
/// 参数说明: 无。
///
/// 返回：操作成功返回零；协议、锁定或构建失败返回非零，不把部分进度当作成功。
fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("naxus: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// 业务作用：解析命令并在任何依赖解析前固定工作区与单一服务配置。
/// 参数说明: 无。
///
/// 返回：相应 Cargo 命令的退出码；非法配置或不受支持命令明确失败。
fn run() -> Result<u8> {
    let mut arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.first().is_some_and(|value| value == "naxus") {
        arguments.remove(0);
    }
    if arguments.is_empty() || matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("cargo naxus configure --endpoint <URL> --repository <name>\ncargo naxus publish [--workspace] [--atomic] [-p <package>]\ncargo naxus publish --abort-pending\ncargo naxus build|check|test|run [Cargo options]\ncargo naxus update\n\nPending publication is recovered before the next online command.\nCredentials: NAXUS_TOKEN or service.token_file in .naxus/config.toml");
        return Ok(0);
    }
    if arguments[0] == "--version" {
        println!("cargo-naxus {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }
    cargo::check_version()?;
    let command = arguments.remove(0);
    normalize_arguments(&mut arguments)?;
    if command == "configure" {
        validate_control_arguments(
            &arguments,
            &["--endpoint", "--repository", "--manifest-path"],
            &[],
        )?;
        return configure(&arguments);
    }
    if matches!(command.as_str(), "publish" | "install") {
        validate_control_arguments(
            &arguments,
            &["-p", "--package", "--repository", "--manifest-path"],
            &["--workspace", "--atomic", "--no-verify", "--abort-pending"],
        )?;
    }
    ensure!(
        matches!(
            command.as_str(),
            "publish" | "install" | "build" | "check" | "test" | "run" | "update"
        ),
        "unsupported Naxus command: {command}"
    );
    let manifest = cargo::option(&arguments, "--manifest-path")
        .map(|path| std::env::current_dir().map(|directory| directory.join(path)))
        .transpose()?;
    let start = manifest
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or(std::env::current_dir()?);
    let path = find_config(&start)?;
    let config = ClientConfig::load(&path)?;
    let root = cargo::workspace(&config, manifest.as_deref())?;
    let _guard = naxus_client::ClientGuard::acquire(&root)?;
    naxus_client::recover_locks(&root)?;
    let control = &arguments[..arguments
        .iter()
        .position(|argument| argument == "--")
        .unwrap_or(arguments.len())];
    let abort = matches!(command.as_str(), "publish" | "install")
        && control.iter().any(|argument| argument == "--abort-pending");
    ensure!(
        !abort
            || !control.iter().any(|argument| matches!(
                argument.as_str(),
                "--workspace" | "--atomic" | "--no-verify" | "--repository" | "-p" | "--package"
            )),
        "--abort-pending cannot select a new publication"
    );
    ensure!(
        !control
            .iter()
            .any(|argument| matches!(argument.as_str(), "--offline" | "--frozen"))
            || !root.join(".naxus/publication.json").try_exists()?,
        "pending publication requires online recovery before an offline build"
    );
    // 旧发布必须先达到明确终态，不能在无主清理或新命令中把已送达的副作用当作未发生。
    let recovered = publication::recover(&root, &path, &config, abort)?;
    naxus_client::cleanup_operations(&root)?;
    if recovered && matches!(command.as_str(), "publish" | "install") {
        return Ok(0);
    }
    cargo::validate_resolution_configuration(&root)?;
    match command.as_str() {
        "publish" | "install" => publish::execute(&root, &path, config, &arguments),
        "build" | "check" | "test" | "run" | "update" => {
            build::execute(&root, &path, config, &command, &arguments)
        }
        _ => anyhow::bail!("unsupported Naxus command: {command}"),
    }
}

/// 业务作用：在改变工作目录前固定显式路径，并阻止调用者覆盖已经验证的来源和锁事务配置。
/// 参数说明：
/// - `arguments`：原始 Cargo 子命令选项；双横线后的运行参数保持原样。
///
/// 返回：稳定选项被展开为名称和值，业务路径固定到原调用目录；不支持的配置覆盖明确拒绝。
fn normalize_arguments(arguments: &mut Vec<String>) -> Result<()> {
    let directory = std::env::current_dir()?;
    let original = std::mem::take(arguments);
    let mut iterator = original.into_iter();
    while let Some(argument) = iterator.next() {
        if argument == "--" {
            arguments.push(argument);
            arguments.extend(iterator);
            break;
        }
        ensure!(
            argument != "--config"
                && !argument.starts_with("--config=")
                && argument != "--lockfile-path"
                && !argument.starts_with("--lockfile-path=")
                && !argument.starts_with("-Z"),
            "Cargo source and lockfile configuration overrides are not supported by cargo naxus"
        );
        let option = argument.split_once('=').filter(|(key, _)| {
            matches!(
                *key,
                "--manifest-path" | "--target-dir" | "--package" | "--repository" | "--endpoint"
            )
        });
        if let Some((key, value)) = option {
            ensure!(!value.is_empty(), "option {key} requires a value");
            arguments.push(key.to_owned());
            arguments.push(if matches!(key, "--manifest-path" | "--target-dir") {
                directory.join(value).to_string_lossy().into_owned()
            } else {
                value.to_owned()
            });
        } else if matches!(argument.as_str(), "--manifest-path" | "--target-dir") {
            let value = iterator.next().context("path option requires a value")?;
            ensure!(!value.starts_with('-'), "path option requires a path");
            arguments.push(argument);
            arguments.push(directory.join(value).to_string_lossy().into_owned());
        } else {
            arguments.push(argument);
        }
    }
    Ok(())
}

/// 业务作用：为配置与发布使用显式选项合同，禁止把 dry-run 等未知意图静默解释为实际发布。
/// 参数说明：
/// - `arguments`：已规范化的控制参数。
/// - `valued`：必须带值的允许选项。
/// - `switches`：没有值的允许开关。
///
/// 返回：参数全部受支持才继续，不执行任何远端写操作。
fn validate_control_arguments(
    arguments: &[String],
    valued: &[&str],
    switches: &[&str],
) -> Result<()> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if valued.contains(&argument.as_str()) {
            let value = arguments
                .get(index + 1)
                .context("option requires a value")?;
            ensure!(
                !value.is_empty() && !value.starts_with('-'),
                "option {argument} requires a value"
            );
            index += 2;
        } else {
            ensure!(
                switches.contains(&argument.as_str()),
                "unsupported control option: {argument}"
            );
            index += 1;
        }
    }
    Ok(())
}

/// 业务作用：登记一次业务服务入口并核对其稳定身份，不把令牌写入配置。
/// 参数说明：
/// - `arguments`：用户提供的地址、仓库和可选 manifest 路径。
///
/// 返回：配置成功写入工作区；已有不同服务配置不被静默覆盖。
fn configure(arguments: &[String]) -> Result<u8> {
    let endpoint =
        cargo::option(arguments, "--endpoint").context("configure requires --endpoint")?;
    let repository =
        cargo::option(arguments, "--repository").context("configure requires --repository")?;
    let mut config = ClientConfig {
        service: ServiceConfig {
            endpoint: endpoint.parse()?,
            repository: repository.to_owned(),
            service_uuid: None,
            token_file: None,
        },
        sources: Vec::new(),
    };
    config.validate()?;
    let manifest = cargo::option(arguments, "--manifest-path").map(PathBuf::from);
    let start = manifest
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or(std::env::current_dir()?);
    let provisional = if let Some(path) = existing_config(&start)? {
        let previous = ClientConfig::load(&path)?;
        // 先核对已登记入口，再读取原配置的凭据路径；重新配置不能把既有 token 发往另一个服务。
        ensure!(
            previous.service.endpoint == config.service.endpoint
                && previous.service.repository == config.service.repository,
            "workspace is already configured for a different service or repository"
        );
        config = previous;
        path
    } else {
        start.join(".naxus/config.toml")
    };
    let discovery = remote::Remote::new(config.clone(), provisional)?;
    discovery.capabilities()?;
    config.sources = discovery.get("/api/naxus/v1/proxies")?;
    config.validate()?;
    let root = cargo::workspace(&config, manifest.as_deref())?;
    let _guard = naxus_client::ClientGuard::acquire(&root)?;
    let path = root.join(".naxus/config.toml");
    naxus_client::recover_locks(&root)?;
    if path.try_exists()? {
        let previous = ClientConfig::load(&path)?;
        publication::recover(&root, &path, &previous, false)?;
    }
    naxus_client::cleanup_operations(&root)?;
    if path.try_exists()? {
        let previous = ClientConfig::load(&path)?;
        ensure!(
            previous.service.endpoint == config.service.endpoint
                && previous.service.repository == config.service.repository,
            "workspace is already configured for a different service or repository"
        );
        config = previous;
    }
    let remote = remote::Remote::new(config.clone(), path.clone())?;
    let capabilities = remote.capabilities()?;
    config.service.service_uuid = Some(capabilities.service_uuid);
    remote.repository()?;
    config.sources = remote.get("/api/naxus/v1/proxies")?;
    config.validate()?;
    config.save(&path)?;
    println!(
        "Configured Naxus repository {} at {}",
        config.service.repository, config.service.endpoint
    );
    Ok(0)
}

/// 业务作用：从工作目录向上查找项目配置，不要求业务维护额外 Cargo 地址文件。
/// 参数说明：
/// - `start`：当前目录或显式 manifest 所在目录。
///
/// 返回：最近的项目配置绝对路径；未配置时给出操作指引。
fn find_config(start: &Path) -> Result<PathBuf> {
    existing_config(start)?
        .context("project has no Naxus configuration; run cargo naxus configure first")
}

/// 业务作用：在首次配置和重新配置间区分缺失状态，保留既有身份及相对凭据文件的实际基准。
/// 参数说明：
/// - `start`：当前目录或显式 manifest 所在目录。
///
/// 返回：最近配置的绝对路径；没有配置为 None，路径或读取权限异常不伪装成首次配置。
fn existing_config(start: &Path) -> Result<Option<PathBuf>> {
    for directory in start.canonicalize()?.ancestors() {
        let path = directory.join(".naxus/config.toml");
        if path.try_exists()? {
            ensure!(
                path.is_file(),
                "project configuration is not a regular file"
            );
            return Ok(Some(path));
        }
    }
    Ok(None)
}
