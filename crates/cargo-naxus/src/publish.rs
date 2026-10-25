use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{ensure, Context, Result};
use naxus_client::ClientConfig;
use naxus_core::ArtifactDigest;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{cargo, publication, remote::Remote};

#[derive(Clone)]
struct Package {
    name: String,
    version: semver::Version,
    directory: PathBuf,
}

/// 业务作用：在独立源码快照完成整仓原生打包，再把全部固定制品原子发布到 Naxus。
/// 参数说明：
/// - `root`：已经持有客户端锁的原工作区。
/// - `configuration`：业务项目服务配置。
/// - `config`：已校验的单入口映射。
/// - `arguments`：包选择及原子发布选项。
///
/// 返回：全部选定成员已提交才成功；打包和上传失败不主动提交，提交结果未知时保留原发布日志。
pub fn execute(
    root: &Path,
    configuration: &Path,
    mut config: ClientConfig,
    arguments: &[String],
) -> Result<u8> {
    if let Some(repository) = cargo::option(arguments, "--repository") {
        config.service.repository = repository.to_owned();
        config.validate()?;
    }
    let operation = cargo::OperationDirectory::create(root)?;
    let remote = Remote::new(config.clone(), configuration.to_path_buf())?;
    let capabilities = remote.capabilities()?;
    remote.validate_sources()?;
    let repository = remote.repository()?;
    let expected = remote.head()?;
    ensure!(
        expected.repository_id == repository.id,
        "repository identity changed"
    );
    let snapshot = operation.0.join("workspace");
    fs::create_dir_all(&snapshot)?;
    let provider_config = operation.0.join("project.toml");
    if let Some(path) = &config.service.token_file {
        if path.is_relative() {
            config.service.token_file = Some(
                configuration
                    .parent()
                    .context("configuration has no parent")?
                    .join(path),
            );
        }
    }
    let credential_path = config
        .service
        .token_file
        .as_ref()
        .map(|path| path.canonicalize())
        .transpose()?;
    fs::write(&provider_config, toml::to_string(&config)?)?;
    let logical = format!(
        "sparse+{}/repositories/{}/index/",
        config.service.endpoint.as_str().trim_end_matches('/'),
        config.service.repository
    );
    let generated = operation.0.join("cargo.toml");
    let initial_lock = operation.0.join("Cargo.lock");
    let source_lock = if root.join("Cargo.lock").try_exists()? {
        Some(hash_file(&root.join("Cargo.lock"))?)
    } else {
        None
    };
    if root.join("Cargo.lock").try_exists()? {
        fs::copy(root.join("Cargo.lock"), &initial_lock)?;
        ensure!(
            Some(hash_file(&initial_lock)?) == source_lock,
            "source Cargo.lock changed while preparing publication"
        );
    }
    fs::write(
        &generated,
        cargo::configuration(&config, &provider_config, &logical, Some(&initial_lock))?,
    )?;
    let metadata: Value = serde_json::from_slice(
        &cargo::checked(
            Command::new(cargo::executable())
                .current_dir(root)
                .args([
                    "metadata",
                    "--no-deps",
                    "--offline",
                    "--format-version",
                    "1",
                    "--config",
                ])
                .arg(&generated),
        )?
        .stdout,
    )?;
    let packages = select_packages(root, &metadata, arguments)?;
    let inventory = workspace_inventory(root, &metadata)?;
    let owned = packages
        .iter()
        .map(|package| (package.name.clone(), package.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut files = BTreeMap::new();
    files.insert(
        root.join("Cargo.toml"),
        hash_file(&root.join("Cargo.toml"))?,
    );
    for package in &packages {
        let output = cargo::checked(
            Command::new(cargo::executable())
                .current_dir(root)
                .args([
                    "package",
                    "--list",
                    "--allow-dirty",
                    "--registry",
                    "naxus",
                    "--config",
                ])
                .arg(&generated)
                .arg("-p")
                .arg(&package.name),
        )?;
        for name in String::from_utf8(output.stdout)?.lines() {
            let path = Path::new(name);
            ensure!(
                path.components()
                    .all(|part| matches!(part, std::path::Component::Normal(_))),
                "Cargo returned a non-relative package file"
            );
            if matches!(
                name,
                "Cargo.lock" | "Cargo.toml.orig" | ".cargo_vcs_info.json" | "naxus.lock"
            ) || name.starts_with(".naxus/")
            {
                continue;
            }
            let original = package.directory.join(path);
            if original.is_file() {
                files.insert(original.clone(), hash_file(&original)?);
            }
        }
    }
    // 工作区继承材料可能位于成员目录之外，按 Cargo 的真实元数据位置保留同一相对布局。
    for package in metadata["packages"]
        .as_array()
        .context("invalid Cargo package metadata")?
    {
        if owned.contains_key(package["name"].as_str().unwrap_or_default()) {
            for key in ["license_file", "readme"] {
                if let Some(path) = package[key].as_str() {
                    let declared = PathBuf::from(path);
                    let manifest_directory = Path::new(
                        package["manifest_path"]
                            .as_str()
                            .context("package has no manifest")?,
                    )
                    .parent()
                    .context("manifest has no directory")?;
                    let path = if declared.is_absolute() {
                        declared
                    } else {
                        manifest_directory.join(declared)
                    };
                    let path = path.canonicalize()?;
                    if path.is_file() && path.starts_with(root) {
                        files.insert(path.clone(), hash_file(&path)?);
                    }
                }
            }
        }
    }
    for (original, expected) in &files {
        if let Some(credential) = &credential_path {
            ensure!(
                !same_file(original, credential)?,
                "configured Naxus credential file must not enter a package archive"
            );
        }
        let relative = original
            .strip_prefix(root)
            .context("workspace member is outside the workspace root")?;
        let destination = snapshot.join(relative);
        fs::create_dir_all(destination.parent().context("package file has no parent")?)?;
        fs::copy(original, &destination)?;
        ensure!(
            hash_file(&destination)? == *expected,
            "source changed while preparing publication"
        );
    }
    for package in &packages {
        let original = package.directory.join("Cargo.toml");
        let destination = snapshot.join(original.strip_prefix(root)?);
        let mut manifest: toml::Value = toml::from_str(&fs::read_to_string(&destination)?)?;
        rewrite_manifest(&mut manifest, &package.directory, root, &inventory, &owned)?;
        make_writable(&destination)?;
        fs::write(destination, toml::to_string_pretty(&manifest)?)?;
    }
    let mut manifest: toml::Value =
        toml::from_str(&fs::read_to_string(snapshot.join("Cargo.toml"))?)?;
    rewrite_manifest(&mut manifest, root, root, &inventory, &owned)?;
    if manifest.get("package").is_some()
        && !packages.iter().any(|package| package.directory == root)
    {
        let table = manifest.as_table_mut().context("invalid root manifest")?;
        for key in [
            "package",
            "dependencies",
            "dev-dependencies",
            "build-dependencies",
            "target",
            "lib",
            "bin",
            "example",
            "bench",
            "test",
            "features",
            "badges",
            "lints",
        ] {
            table.remove(key);
        }
    }
    if manifest.get("workspace").is_none() {
        manifest
            .as_table_mut()
            .context("invalid workspace manifest")?
            .insert(
                "workspace".to_owned(),
                toml::Value::Table(toml::Table::new()),
            );
    }
    if let Some(workspace) = manifest
        .get_mut("workspace")
        .and_then(toml::Value::as_table_mut)
    {
        workspace.insert(
            "members".to_owned(),
            toml::Value::Array(
                packages
                    .iter()
                    .filter(|package| package.directory != root)
                    .map(|package| {
                        toml::Value::String(
                            package
                                .directory
                                .strip_prefix(root)
                                .expect("checked workspace path")
                                .to_string_lossy()
                                .into_owned(),
                        )
                    })
                    .collect(),
            ),
        );
        workspace.remove("default-members");
        workspace.remove("exclude");
    }
    make_writable(&snapshot.join("Cargo.toml"))?;
    fs::write(
        snapshot.join("Cargo.toml"),
        toml::to_string_pretty(&manifest)?,
    )?;
    let lock_path = snapshot.join("Cargo.lock");
    if source_lock.is_some() {
        let mut lock: toml::Value = toml::from_str(&fs::read_to_string(&initial_lock)?)?;
        prepare_publish_lock(&mut lock, &owned, &config)?;
        fs::write(&lock_path, toml::to_string(&lock)?)?;
    }
    fs::write(
        &generated,
        cargo::configuration(&config, &provider_config, &logical, Some(&lock_path))?,
    )?;
    let target = operation.0.join("target");
    let mut package_command = Command::new(cargo::executable());
    package_command
        .current_dir(&snapshot)
        .args(["package", "--target-dir"])
        .arg(&target)
        .args(["--registry", "naxus", "--allow-dirty", "--config"])
        .arg(&generated)
        .env("CARGO_HOME", operation.0.join("cargo-home"));
    for package in &packages {
        package_command.arg("-p").arg(&package.name);
    }
    if arguments.iter().any(|argument| argument == "--no-verify") {
        package_command.arg("--no-verify");
    }
    cargo::checked(&mut package_command)?;
    let mut artifacts = Vec::new();
    for package in &packages {
        let path = target
            .join("package")
            .join(format!("{}-{}.crate", package.name, package.version));
        let digest = hash_file(&path)?;
        let entry = naxus_registry::inspect_archive(
            &mut File::open(&path)?,
            &package.name,
            &package.version.to_string(),
            digest.clone(),
            &logical,
            &naxus_registry::ArchiveLimits::default(),
        )?;
        artifacts.push(publication::PreparedArtifact {
            path,
            entry,
        });
    }
    for (original, expected) in files {
        ensure!(
            hash_file(&original)? == expected,
            "source changed during packaging; publication was not started"
        );
    }
    let current_lock = if root.join("Cargo.lock").try_exists()? {
        Some(hash_file(&root.join("Cargo.lock"))?)
    } else {
        None
    };
    // 外部 Cargo 不遵守本客户端锁时，也不能把两个时刻的锁选择混成一个发布输入。
    ensure!(
        source_lock == current_lock,
        "source Cargo.lock changed during packaging; publication was not started"
    );
    publication::publish(
        root,
        &remote,
        &repository,
        &capabilities.service_uuid,
        &expected,
        &artifacts,
    )
}

/// 业务作用：识别配置凭据的真实文件身份，归档不能通过别名、符号链接或硬链接包含该材料。
/// 参数说明：
/// - `path`：Cargo 原生文件清单中的候选文件。
/// - `credential`：已规范化且受保护的凭据文件。
///
/// 返回：指向同一实际文件时为 true，文件状态不可读取时拒绝继续打包。
fn same_file(path: &Path, credential: &Path) -> Result<bool> {
    if path.canonicalize()? == credential {
        return Ok(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let candidate = fs::metadata(path)?;
        let material = fs::metadata(credential)?;
        Ok(candidate.dev() == material.dev() && candidate.ino() == material.ino())
    }
    #[cfg(not(unix))]
    Ok(false)
}

/// 业务作用：从真实 Cargo 成员描述确定本次明确选择的发布集合。
/// 参数说明：
/// - `root`：工作区根。
/// - `metadata`：Cargo --no-deps 的工作区描述。
/// - `arguments`：workspace 或包选择参数。
///
/// 返回：允许发布到 naxus 的成员，不按目录名猜测包名。
fn select_packages(root: &Path, metadata: &Value, arguments: &[String]) -> Result<Vec<Package>> {
    let names = arguments
        .windows(2)
        .filter(|values| matches!(values[0].as_str(), "-p" | "--package"))
        .map(|values| values[1].as_str())
        .collect::<Vec<_>>();
    let all = arguments.iter().any(|value| value == "--workspace");
    let defaults = metadata["workspace_default_members"]
        .as_array()
        .context("metadata omitted default members")?;
    let mut selected = Vec::new();
    for package in metadata["packages"]
        .as_array()
        .context("metadata omitted packages")?
    {
        let name = package["name"].as_str().context("invalid package name")?;
        let include =
            all || names.contains(&name) || (names.is_empty() && defaults.contains(&package["id"]));
        if !include {
            continue;
        }
        if let Some(registries) = package["publish"].as_array() {
            ensure!(
                registries.iter().any(|value| value == "naxus"),
                "package {name} is not publishable to registry naxus"
            );
        }
        let directory = PathBuf::from(
            package["manifest_path"]
                .as_str()
                .context("invalid manifest path")?,
        )
        .parent()
        .context("manifest has no parent")?
        .to_path_buf();
        ensure!(
            directory.starts_with(root),
            "workspace member is outside source root"
        );
        selected.push(Package {
            name: name.to_owned(),
            version: package["version"]
                .as_str()
                .context("invalid package version")?
                .parse()?,
            directory,
        });
    }
    ensure!(
        !selected.is_empty()
            && names
                .iter()
                .all(|name| selected.iter().any(|package| package.name == *name)),
        "no matching publishable packages selected"
    );
    Ok(selected)
}

/// 业务作用：从 Cargo 明确声明的完整工作区建立坐标所有权，部分发布仍保留其他内部成员的私服来源。
/// 参数说明：
/// - `root`：原工作区根。
/// - `metadata`：Cargo 的完整成员描述。
///
/// 返回：只包含真实工作区成员的版本和路径，不按临时发布选择缩小来源所有权。
fn workspace_inventory(root: &Path, metadata: &Value) -> Result<BTreeMap<String, Package>> {
    let members = metadata["workspace_members"]
        .as_array()
        .context("Cargo omitted workspace members")?;
    let mut inventory = BTreeMap::new();
    for package in metadata["packages"]
        .as_array()
        .context("Cargo omitted package metadata")?
    {
        if !members.contains(&package["id"]) {
            continue;
        }
        let directory = Path::new(
            package["manifest_path"]
                .as_str()
                .context("member manifest missing")?,
        )
        .parent()
        .context("member manifest has no parent")?
        .canonicalize()?;
        ensure!(
            directory.starts_with(root),
            "workspace member is outside source root"
        );
        let name = package["name"]
            .as_str()
            .context("member name missing")?
            .to_owned();
        inventory.insert(
            name.clone(),
            Package {
                name,
                version: package["version"]
                    .as_str()
                    .context("member version missing")?
                    .parse()?,
                directory,
            },
        );
    }
    Ok(inventory)
}

/// 业务作用：仅在发布副本中把明确拥有的同仓依赖绑定到本批成员，保留外部来源。
/// 参数说明：
/// - `manifest`：待改写的发布副本 manifest。
/// - `directory`：原 manifest 所在目录。
/// - `root`：原工作区根。
/// - `owned`：完整工作区明确拥有的内部成员清单。
/// - `selected`：本次需要打包并原子发布的成员集合。
///
/// 返回：来源和版本关系合法时完成转换，不修改原工程文件。
fn rewrite_manifest(
    manifest: &mut toml::Value,
    directory: &Path,
    root: &Path,
    owned: &BTreeMap<String, Package>,
    selected: &BTreeMap<String, Package>,
) -> Result<()> {
    rewrite_dependencies(manifest, directory, root, owned, selected)?;
    if let Some(workspace) = manifest.get_mut("workspace") {
        rewrite_dependencies(workspace, root, root, owned, selected)?;
    }
    if let Some(targets) = manifest
        .get_mut("target")
        .and_then(toml::Value::as_table_mut)
    {
        for (_, table) in targets.iter_mut() {
            rewrite_dependencies(table, directory, root, owned, selected)?;
        }
    }
    Ok(())
}

/// 业务作用：转换各类依赖与别名，在版本不属于本批成员时保持精确私服约束。
/// 参数说明：
/// - `manifest`：根、workspace 或 target 依赖表。
/// - `directory`：声明相对路径的基准。
/// - `root`：工作区路径边界。
/// - `owned`：明确拥有的包清单。
/// - `selected`：本次本地打包的成员清单。
///
/// 返回：本批且版本匹配时增加相对 path + version + registry，其他内部成员只使用私服 registry。
fn rewrite_dependencies(
    manifest: &mut toml::Value,
    directory: &Path,
    root: &Path,
    owned: &BTreeMap<String, Package>,
    selected: &BTreeMap<String, Package>,
) -> Result<()> {
    for section in ["dependencies", "build-dependencies", "dev-dependencies"] {
        let Some(dependencies) = manifest
            .get_mut(section)
            .and_then(toml::Value::as_table_mut)
        else {
            continue;
        };
        for (alias, value) in dependencies {
            if value.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
                continue;
            }
            let name = value
                .get("package")
                .and_then(toml::Value::as_str)
                .unwrap_or(alias);
            let Some(package) = owned.get(name) else {
                continue;
            };
            let mut dependency = if let Some(version) = value.as_str() {
                let mut table = toml::Table::new();
                table.insert(
                    "version".to_owned(),
                    toml::Value::String(version.to_owned()),
                );
                table
            } else {
                value
                    .as_table()
                    .context("invalid dependency declaration")?
                    .clone()
            };
            ensure!(
                !dependency.contains_key("git")
                    && dependency
                        .get("registry")
                        .and_then(toml::Value::as_str)
                        .is_none_or(|registry| matches!(registry, "naxus" | "crates-io")),
                "owned package has a conflicting declared source"
            );
            if let Some(path) = dependency.get("path").and_then(toml::Value::as_str) {
                ensure!(
                    directory.join(path).canonicalize()? == package.directory.canonicalize()?,
                    "owned dependency path points to another package"
                );
            }
            let requirement: semver::VersionReq = dependency
                .get("version")
                .and_then(toml::Value::as_str)
                .context("internal dependency requires a version")?
                .parse()?;
            dependency.insert(
                "registry".to_owned(),
                toml::Value::String("naxus".to_owned()),
            );
            if requirement.matches(&package.version) && selected.contains_key(&package.name) {
                dependency.insert(
                    "path".to_owned(),
                    toml::Value::String(
                        relative(
                            directory.strip_prefix(root)?,
                            package.directory.strip_prefix(root)?,
                        )
                        .to_string_lossy()
                        .into_owned(),
                    ),
                );
            } else {
                dependency.remove("path");
            }
            *value = toml::Value::Table(dependency);
        }
    }
    Ok(())
}

/// 业务作用：把旧发布锁中的本批内部成员转为本地身份，保留未变公开依赖的版本选择。
/// 参数说明：
/// - `lock`：原工作区锁的发布副本。
/// - `owned`：本批成员及精确版本。
/// - `config`：本次私服的精确来源范围。
///
/// 返回：本批旧 checksum 不再参与新打包，其他来源锁定保持不变。
fn prepare_publish_lock(
    lock: &mut toml::Value,
    owned: &BTreeMap<String, Package>,
    config: &ClientConfig,
) -> Result<()> {
    let Some(packages) = lock.get_mut("package").and_then(toml::Value::as_array_mut) else {
        return Ok(());
    };
    let mut replacements = Vec::new();
    let mut local = packages
        .iter()
        .filter(|package| package.get("source").is_none())
        .map(|package| {
            (
                package["name"].as_str().unwrap_or_default().to_owned(),
                package["version"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    let mut removed = std::collections::BTreeSet::new();
    for (index, package) in packages.iter_mut().enumerate() {
        let name = package
            .get("name")
            .and_then(toml::Value::as_str)
            .context("invalid locked package name")?
            .to_owned();
        let version = package
            .get("version")
            .and_then(toml::Value::as_str)
            .context("invalid locked package version")?
            .to_owned();
        let source = package
            .get("source")
            .and_then(toml::Value::as_str)
            .map(str::to_owned);
        let managed = source.as_deref().is_some_and(|source| {
            config.permits_index(source.strip_prefix("registry+").unwrap_or(source))
        });
        if managed
            && owned
                .get(&name)
                .is_some_and(|package| package.version.to_string() == version)
        {
            if let Some(source) = source {
                replacements.push((
                    format!("{name} {version} ({source})"),
                    format!("{name} {version}"),
                ));
            }
            // 工作区路径成员是本次发布的权威；独立公开来源保留原身份，不能被同名匹配吸收。
            if !local.insert((name, version)) {
                removed.insert(index);
                continue;
            }
            let package = package.as_table_mut().context("invalid lock package")?;
            package.remove("source");
            package.remove("checksum");
        }
    }
    let mut index = 0;
    packages.retain(|_| {
        let retain = !removed.contains(&index);
        index += 1;
        retain
    });
    for package in packages {
        if let Some(dependencies) = package
            .get_mut("dependencies")
            .and_then(toml::Value::as_array_mut)
        {
            for dependency in dependencies {
                if let Some((_, replacement)) = replacements
                    .iter()
                    .find(|(old, _)| dependency.as_str() == Some(old.as_str()))
                {
                    *dependency = toml::Value::String(replacement.clone());
                }
            }
        }
    }
    Ok(())
}

/// 业务作用：生成工作区内两个成员之间的相对路径，避免把本机绝对位置写入归档。
/// 参数说明：
/// - `from`：工作区内声明目录。
/// - `to`：工作区内目标目录。
///
/// 返回：只含父级与正常组件的相对路径。
fn relative(from: &Path, to: &Path) -> PathBuf {
    let left = from.components().collect::<Vec<_>>();
    let right = to.components().collect::<Vec<_>>();
    let shared = left
        .iter()
        .zip(&right)
        .take_while(|(left, right)| left == right)
        .count();
    let mut path = PathBuf::new();
    for _ in shared..left.len() {
        path.push("..");
    }
    for component in &right[shared..] {
        path.push(component.as_os_str());
    }
    if path.as_os_str().is_empty() {
        path.push(".");
    }
    path
}

/// 业务作用：流式计算源码或归档身份，避免整包载入内存。
/// 参数说明：
/// - `path`：本次固定清单中的精确文件。
///
/// 返回：原始字节 SHA-256；读取失败不允许发布。
fn hash_file(path: &Path) -> Result<ArtifactDigest> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let text = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    text.parse()
}

/// 业务作用：只对发布副本的 manifest 开放所有者写入权限，原工程权限保持原样。
/// 参数说明：
/// - `path`：独立发布目录中的 manifest。
///
/// 返回：副本可写，之后才能进行来源转换。
fn make_writable(path: &Path) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(permissions.mode() | 0o200);
    }
    #[cfg(not(unix))]
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions)?;
    Ok(())
}
