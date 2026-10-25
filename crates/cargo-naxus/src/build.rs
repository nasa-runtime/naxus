use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    path::Path,
    process::Command,
};

use anyhow::{ensure, Context, Result};
use naxus_client::{commit_locks, ClientConfig, ProxySource};
use naxus_core::{ArtifactDigest, Coordinate, Resolution, Revision, Snapshot};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{cargo, lease::LeaseGuard, remote::Remote};

#[derive(Clone, Serialize, Deserialize)]
struct ManagedPackage {
    name: String,
    version: String,
    revision_id: String,
    artifact_sha256: ArtifactDigest,
    index_metadata_sha256: ArtifactDigest,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NaxusLock {
    format: u32,
    service_uuid: String,
    repository_uuid: String,
    cargo_lock_sha256: ArtifactDigest,
    workspace_manifest_sha256: ArtifactDigest,
    snapshot_set_id: String,
    snapshot: Snapshot,
    managed_packages: Vec<ManagedPackage>,
    #[serde(default)]
    public_materializations: Vec<PublicMaterialization>,
}

#[derive(Serialize, Deserialize)]
struct PublicMaterialization {
    id: String,
    proxy_id: String,
    fingerprint: ArtifactDigest,
}

struct PublicSessions {
    remote: Remote,
    selected: BTreeMap<String, String>,
}

impl PublicSessions {
    /// 业务作用：为解析中各个独立来源建立短期固定视图，显式 update 必须重新确认索引。
    /// 参数说明：
    /// - `remote`：已核对身份的单入口。
    /// - `sources`：已绑定的独立公开来源。
    /// - `update`：是否要求 revalidate。
    /// - `views`：每个独立来源的镜像起始位置。
    ///
    /// 返回：全部 session；部分创建失败时已建视图由守卫有界释放。
    fn begin(
        remote: &Remote,
        sources: &[ProxySource],
        update: bool,
        views: &BTreeMap<String, usize>,
    ) -> Result<Self> {
        let mut sessions = Self {
            remote: remote.clone(),
            selected: BTreeMap::new(),
        };
        for source in sources {
            let response: Value = remote.post(
                &format!("/proxies/{}/sessions", source.name),
                &serde_json::json!({
                    "mode": if update {
                        "revalidate"
                    } else {
                        "cache-first"
                    },
                    "upstream_start": views.get(&source.name).copied().unwrap_or_default(),
                }),
            )?;
            let id = response["id"]
                .as_str()
                .context("proxy session identity missing")?;
            uuid::Uuid::parse_str(id)?;
            ensure!(
                response["proxy_id"].as_str() == Some(source.id.as_str()),
                "proxy source identity changed"
            );
            sessions.selected.insert(source.name.clone(), id.to_owned());
        }
        Ok(sessions)
    }

    /// 业务作用：在提交双锁前要求服务端持久化 Cargo 实际选择的所有公开对象。
    /// 参数说明：
    /// - `lock`：已经由 Cargo 生成的临时锁。
    /// - `sources`：保留独立身份的公开来源。
    ///
    /// 返回：每个非空公开依赖集合的完整物化证据。
    fn materialize(
        &self,
        lock: &[u8],
        sources: &[ProxySource],
    ) -> Result<Vec<PublicMaterialization>> {
        let lock: toml::Value = toml::from_str(std::str::from_utf8(lock)?)?;
        let packages = lock["package"]
            .as_array()
            .context("Cargo.lock has no packages")?;
        let mut materializations = Vec::new();
        for source in sources {
            let mut items = Vec::new();
            for package in packages {
                if package
                    .get("source")
                    .and_then(toml::Value::as_str)
                    .and_then(registry_index)
                    .is_some_and(|index| same_index(index, &source.logical_index))
                {
                    items.push(serde_json::json!({
                        "name": package["name"].as_str().context("locked name missing")?,
                        "version": package["version"].as_str().context("locked version missing")?,
                        "digest": package["checksum"].as_str().context("locked checksum missing")?
                    }));
                }
            }
            if !items.is_empty() {
                let id = self
                    .selected
                    .get(&source.name)
                    .context("proxy session missing")?;
                let value: PublicMaterialization = self.remote.post(
                    &format!("/proxies/{}/sessions/{id}/materializations", source.name),
                    &serde_json::json!({
                        "items": items
                    }),
                )?;
                ensure!(
                    value.proxy_id == source.id,
                    "materialization source changed"
                );
                uuid::Uuid::parse_str(&value.id)?;
                materializations.push(value);
            }
        }
        Ok(materializations)
    }
}

impl Drop for PublicSessions {
    /// 业务作用：释放本命令创建的短期解析视图，异常退出留下的记录按服务期限回收。
    /// 参数说明: 无。
    ///
    /// 返回：每个视图只尝试一次有界关闭，不改变业务命令结果。
    fn drop(&mut self) {
        for (proxy, id) in &self.selected {
            let _ = self
                .remote
                .close(&format!("/proxies/{proxy}/sessions/{id}"));
        }
    }
}

/// 业务作用：自动核对已选修订，在临时锁文件完成解析与下载后发布双锁，再执行用户命令。
/// 参数说明：
/// - `root`：已经持有客户端锁的工作区。
/// - `configuration`：业务项目单入口配置文件。
/// - `config`：校验后的服务映射。
/// - `command`：build/check/test/run/update。
/// - `arguments`：原始 Cargo 构建参数。
///
/// 返回：真实构建退出码；解析或物化失败不改写业务锁文件。
pub fn execute(
    root: &Path,
    configuration: &Path,
    config: ClientConfig,
    command: &str,
    arguments: &[String],
) -> Result<u8> {
    let prefix = arguments
        .iter()
        .position(|value| value == "--")
        .unwrap_or(arguments.len());
    let locked = arguments[..prefix]
        .iter()
        .any(|value| matches!(value.as_str(), "--locked" | "--frozen"));
    let offline = arguments[..prefix]
        .iter()
        .any(|value| matches!(value.as_str(), "--offline" | "--frozen"));
    ensure!(!offline || locked, "offline builds require --locked");
    ensure!(
        command != "update" || !locked,
        "update cannot be combined with --locked"
    );
    let arguments = arguments
        .iter()
        .enumerate()
        .filter(|(index, value)| *index >= prefix || value.as_str() != "--locked")
        .map(|(_, value)| value.clone())
        .collect::<Vec<_>>();
    let operation = cargo::OperationDirectory::create(root)?;
    let generated = operation.0.join("cargo.toml");
    let logical = format!(
        "sparse+{}/repositories/{}/index/",
        config.service.endpoint.as_str().trim_end_matches('/'),
        config.service.repository
    );
    let description_lock = operation.0.join("description/Cargo.lock");
    fs::create_dir_all(
        description_lock
            .parent()
            .context("description lock has no parent")?,
    )?;
    if root.join("Cargo.lock").try_exists()? {
        fs::copy(root.join("Cargo.lock"), &description_lock)?;
    }
    fs::write(
        &generated,
        cargo::configuration(&config, configuration, &logical, Some(&description_lock))?,
    )?;
    let no_deps = metadata(root, &generated, &arguments, true, false)?;
    validate_manifest_sources(&no_deps, &logical, &config)?;
    let manifest_digest = fingerprint(root, &no_deps, &config, &arguments)?;
    let cargo_path = root.join("Cargo.lock");
    let naxus_path = root.join("naxus.lock");
    let old = if naxus_path.try_exists()? {
        let lock: NaxusLock = toml::from_str(&fs::read_to_string(&naxus_path)?)?;
        ensure!(
            lock.format == 1
                && lock.cargo_lock_sha256 == ArtifactDigest::of(&fs::read(&cargo_path)?),
            "Cargo.lock and naxus.lock do not describe the same transaction"
        );
        Some(lock)
    } else {
        None
    };
    let remote = Remote::new(config.clone(), configuration.to_path_buf())?;
    let mut leases = Vec::new();
    if !offline {
        remote.validate_sources()?;
    }
    if locked {
        let lock = old
            .as_ref()
            .context("locked Naxus builds require Cargo.lock and naxus.lock")?;
        ensure!(
            lock.workspace_manifest_sha256 == manifest_digest,
            "manifest or resolution inputs changed in locked mode"
        );
        if !offline {
            let capabilities = remote.capabilities()?;
            ensure!(
                capabilities.service_uuid == lock.service_uuid,
                "locked service identity changed"
            );
            ensure!(materializations_valid(&remote, &lock.public_materializations)?, "locked public materialization was revoked; run an unlocked Naxus build to materialize it again");
            let resolved = resolve(
                &remote,
                &mut leases,
                &coordinates(&lock.managed_packages),
                Some(&lock.snapshot),
            )?;
            ensure!(
                resolved.snapshot.manifest_sha256 == lock.snapshot.manifest_sha256
                    && unchanged(&lock.managed_packages, &resolved)?,
                "locked snapshot content is no longer available"
            );
        }
        let index = snapshot_index(&config, &lock.snapshot);
        fs::write(
            &generated,
            cargo::configuration(&config, configuration, &index, None)?,
        )?;
        let result = cargo::execute(root, command, &generated, &arguments)?;
        for lease in &leases {
            lease.validate()?;
        }
        return Ok(result);
    }
    let capabilities = remote.capabilities()?;
    if let Some(old) = &old {
        ensure!(
            old.service_uuid == capabilities.service_uuid,
            "workspace was locked against a different Naxus service"
        );
    }
    let selected = old
        .as_ref()
        .map(|old| coordinates(&old.managed_packages))
        .unwrap_or_default();
    let resolution = resolve(&remote, &mut leases, &selected, None)?;
    if let Some(old) = &old {
        ensure!(
            old.repository_uuid == resolution.snapshot.repository_id,
            "repository identity changed"
        );
        if command != "update"
            && old.workspace_manifest_sha256 == manifest_digest
            && unchanged(&old.managed_packages, &resolution)?
            && materializations_valid(&remote, &old.public_materializations)?
        {
            // 只有无关坐标变化时继续旧快照；重新取得旧快照租约，但不改写业务双锁。
            resolve(&remote, &mut leases, &selected, Some(&old.snapshot))?;
            let index = snapshot_index(&config, &old.snapshot);
            fs::write(
                &generated,
                cargo::configuration(&config, configuration, &index, None)?,
            )?;
            let result = cargo::execute(root, command, &generated, &arguments)?;
            for lease in &leases {
                lease.validate()?;
            }
            return Ok(result);
        }
    }
    let snapshot = resolution.snapshot;
    let index = snapshot_index(&config, &snapshot);
    let temporary_lock = operation.0.join("Cargo.lock");
    if cargo_path.try_exists()? {
        fs::copy(&cargo_path, &temporary_lock)?;
        let mut lock: toml::Value = toml::from_str(&fs::read_to_string(&temporary_lock)?)?;
        let existing = lock_coordinates(&lock, &config)?;
        let selected = resolve(&remote, &mut leases, &existing, Some(&snapshot))?;
        transfer(&mut lock, &config, &index, &selected)?;
        fs::write(&temporary_lock, toml::to_string(&lock)?)?;
    }
    let (public_sessions, graph) = resolve_graph(
        root,
        configuration,
        &remote,
        &index,
        &operation,
        &arguments,
        command == "update",
    )?;
    let chosen = validate_graph(&graph, &index, &config)?;
    let resolved = resolve(&remote, &mut leases, &chosen, Some(&snapshot))?;
    ensure!(
        resolved.snapshot.id == snapshot.id
            && resolved.snapshot.manifest_sha256 == snapshot.manifest_sha256,
        "fixed snapshot changed during resolution"
    );
    let managed_packages = resolved
        .packages
        .into_iter()
        .map(|package| {
            let revision = package
                .revision
                .context("resolved private coordinate is missing")?;
            validate_revision(&revision)?;
            Ok(ManagedPackage {
                name: package.coordinate.name,
                version: package.coordinate.version,
                revision_id: revision.id,
                artifact_sha256: revision.entry.cksum,
                index_metadata_sha256: revision.metadata_sha256,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let before = fs::read(&temporary_lock)?;
    let public_materializations = public_sessions.materialize(&before, &config.sources)?;
    cargo::checked(
        Command::new(cargo::executable())
            .current_dir(root)
            .args(["fetch", "--locked", "--config"])
            .arg(&generated),
    )?;
    let graph_after = metadata(root, &generated, &arguments, false, true)?;
    validate_graph(&graph_after, &index, &config)?;
    ensure!(
        fs::read(&temporary_lock)? == before,
        "temporary lock changed during locked fetch"
    );
    let lock = NaxusLock {
        format: 1,
        service_uuid: capabilities.service_uuid,
        repository_uuid: snapshot.repository_id.clone(),
        cargo_lock_sha256: ArtifactDigest::of(&before),
        workspace_manifest_sha256: manifest_digest,
        snapshot_set_id: resolved.snapshot_set_id,
        snapshot,
        managed_packages,
        public_materializations,
    };
    // 所有对象已由 Cargo 下载并校验，才允许提交对业务可见的关联锁文件。
    for lease in &leases {
        lease.validate()?;
    }
    commit_locks(root, &before, toml::to_string_pretty(&lock)?.as_bytes())?;
    if command == "update" {
        println!("Updated Naxus dependency selection");
        return Ok(0);
    }
    fs::write(
        &generated,
        cargo::configuration_with_sessions(
            &config,
            configuration,
            &index,
            None,
            &public_sessions.selected,
        )?,
    )?;
    let result = cargo::execute(root, command, &generated, &arguments)?;
    for lease in &leases {
        lease.validate()?;
    }
    Ok(result)
}

/// 业务作用：在新的固定公开视图中重试完整依赖图，处理前序镜像尚未同步所需版本的情况。
/// 参数说明：
/// - `root`：业务工作区。
/// - `configuration`：凭据配置路径。
/// - `remote`：已固定来源身份的服务。
/// - `index`：始终不变的 hosted 快照索引。
/// - `operation`：本命令的独立解析目录。
/// - `arguments`：影响 Cargo 解析的真实选项。
/// - `update`：是否显式更新版本选择。
///
/// 返回：成功图与仍有效的视图守卫；最多尝试六十四个镜像组合，不依赖 Cargo 错误文本。
fn resolve_graph(
    root: &Path,
    configuration: &Path,
    remote: &Remote,
    index: &str,
    operation: &cargo::OperationDirectory,
    arguments: &[String],
    update: bool,
) -> Result<(PublicSessions, Value)> {
    let temporary_lock = operation.0.join("Cargo.lock");
    let generated = operation.0.join("cargo.toml");
    let seed = if temporary_lock.try_exists()? {
        Some(fs::read(&temporary_lock)?)
    } else {
        None
    };
    let mut counts = Vec::new();
    for source in &remote.config.sources {
        let response: Value = remote.get(&format!("/proxies/{}/views", source.name))?;
        let count = response["count"]
            .as_u64()
            .context("proxy view count missing")?;
        ensure!((1..=16).contains(&count), "invalid proxy view count");
        counts.push(count as usize);
    }
    let initial = vec![0; counts.len()];
    let mut visited = BTreeSet::from([initial.clone()]);
    let mut pending = VecDeque::from([initial]);
    let mut attempts = 0;
    let mut failure = None;
    while let Some(view) = pending.pop_front() {
        if attempts >= 64 {
            break;
        }
        attempts += 1;
        let views = remote
            .config
            .sources
            .iter()
            .zip(&view)
            .map(|(source, start)| (source.name.clone(), *start))
            .collect();
        let sessions = PublicSessions::begin(remote, &remote.config.sources, update, &views)?;
        // 失败图的部分锁选择不能流入下一视图；每次均从相同的业务选择种子重新解析。
        if let Some(seed) = &seed {
            fs::write(&temporary_lock, seed)?;
        } else if temporary_lock.try_exists()? {
            fs::remove_file(&temporary_lock)?;
        }
        fs::write(
            &generated,
            cargo::configuration_with_sessions(
                &remote.config,
                configuration,
                index,
                Some(&temporary_lock),
                &sessions.selected,
            )?,
        )?;
        let result = (|| {
            if update {
                cargo::checked(
                    Command::new(cargo::executable())
                        .current_dir(root)
                        .args(["update", "--config"])
                        .arg(&generated)
                        .args(arguments),
                )?;
            }
            metadata(root, &generated, arguments, false, false)
        })();
        match result {
            Ok(graph) => return Ok((sessions, graph)),
            Err(error) => {
                eprintln!("Naxus mirror view could not resolve the Cargo graph: {error:#}");
                failure = Some(error);
            }
        }
        // 不在同一 session 中改变已返回索引，也不把不同镜像的版本行拼成新索引。
        drop(sessions);
        for dimension in 0..counts.len() {
            if view[dimension] + 1 < counts[dimension] {
                let mut next = view.clone();
                next[dimension] += 1;
                if visited.insert(next.clone()) {
                    pending.push_back(next);
                }
            }
        }
        if !pending.is_empty() && attempts < 64 {
            eprintln!("Retrying Cargo resolution with the next fixed Naxus mirror view");
        }
    }
    Err(failure.unwrap_or_else(|| anyhow::anyhow!("no public resolution view available"))).context(
        "Cargo could not resolve the dependency graph within the configured Naxus mirror views",
    )
}

/// 业务作用：批量查询 HEAD 或固定历史快照，不逐坐标读取变化中的当前入口。
/// 参数说明：
/// - `remote`：唯一 Naxus 服务。
/// - `leases`：命令期间持续续期的保留权集合。
/// - `packages`：需要查询的精确坐标。
/// - `snapshot`：已有快照；None 时固定当前 HEAD。
///
/// 返回：带租约的一致依赖选择。
fn resolve(
    remote: &Remote,
    leases: &mut Vec<LeaseGuard>,
    packages: &[Coordinate],
    snapshot: Option<&Snapshot>,
) -> Result<Resolution> {
    let repository = remote.repository()?;
    let value: Resolution = remote.post(
        "/api/naxus/v1/resolutions",
        &serde_json::json!({
            "repository": remote.config.service.repository,
            "client_protocol": 1,
            "packages": packages,
            "fixed_snapshot": snapshot.map(|snapshot| &snapshot.id),
        }),
    )?;
    // 先验证完整响应再续期，错误快照或等长坐标替换不能获得本地可信租约守卫。
    value.validate(&repository.id, packages, snapshot)?;
    leases.push(LeaseGuard::begin(remote, &value)?);
    Ok(value)
}

/// 业务作用：核对工作区已经持有的公开物化证据，不因 Cargo 本机缓存命中而跳过服务端撤销。
/// 参数说明：
/// - `remote`：已核对身份的服务。
/// - `materializations`：工作区记录的公开集合。
///
/// 返回：全部仍有效时为 true；网络失败明确失败，不解释为可忽略的失效。
fn materializations_valid(
    remote: &Remote,
    materializations: &[PublicMaterialization],
) -> Result<bool> {
    for materialization in materializations {
        let value: Value = remote.get(&format!(
            "/api/naxus/v1/materializations/{}",
            materialization.id
        ))?;
        if value["valid"].as_bool() != Some(true) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// 业务作用：从已锁定私有包形成服务器核对集合。
/// 参数说明：
/// - `packages`：当前已选修订。
///
/// 返回：确定性顺序的精确坐标。
fn coordinates(packages: &[ManagedPackage]) -> Vec<Coordinate> {
    packages
        .iter()
        .map(|package| Coordinate {
            name: package.name.clone(),
            version: package.version.clone(),
        })
        .collect()
}

/// 业务作用：比较原始制品与索引元数据指纹，避免无关发布引发锁文件抖动。
/// 参数说明：
/// - `old`：业务已选私有修订。
/// - `current`：同一快照下的服务器核对结果。
///
/// 返回：所有坐标仍存在且语义指纹一致时为 true。
fn unchanged(old: &[ManagedPackage], current: &Resolution) -> Result<bool> {
    for package in old {
        let Some(revision) = current
            .packages
            .iter()
            .find(|item| {
                item.coordinate.name == package.name && item.coordinate.version == package.version
            })
            .and_then(|item| item.revision.as_ref())
        else {
            return Ok(false);
        };
        validate_revision(revision)?;
        if revision.entry.cksum != package.artifact_sha256
            || revision.metadata_sha256 != package.index_metadata_sha256
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// 业务作用：在使用服务端指纹迁移临时锁之前复验元数据摘要与修订身份。
/// 参数说明：
/// - `revision`：服务器提供的不可变修订。
///
/// 返回：结构、UUID 和元数据摘要匹配时成功。
fn validate_revision(revision: &Revision) -> Result<()> {
    revision.entry.validate()?;
    uuid::Uuid::parse_str(&revision.id)?;
    ensure!(
        revision.entry.metadata_digest()? == revision.metadata_sha256,
        "revision metadata digest mismatch"
    );
    Ok(())
}

/// 业务作用：生成固定仓库快照的来源 URL，不把本地事务目录写入 package 身份。
/// 参数说明：
/// - `config`：业务服务映射。
/// - `snapshot`：已选择快照。
///
/// 返回：不可变 sparse index URL。
fn snapshot_index(config: &ClientConfig, snapshot: &Snapshot) -> String {
    format!(
        "sparse+{}/repositories/{}/snapshots/{}/index/",
        config.service.endpoint.as_str().trim_end_matches('/'),
        config.service.repository,
        snapshot.id
    )
}

/// 业务作用：判定一个 Cargo source 是否属于当前项目受管的 hosted 仓库。
/// 参数说明：
/// - `config`：项目服务映射。
/// - `source`：Cargo source identity。
///
/// 返回：仅逻辑或固定私有来源匹配时为 true。
fn managed(config: &ClientConfig, source: &str) -> bool {
    registry_index(source).is_some_and(|index| config.permits_index(index))
}

/// 业务作用：识别 Cargo 的 sparse 与 Git registry 来源，保持真实 source identity。
/// 参数说明：
/// - `source`：Cargo.lock 或 metadata 中的来源标识。
///
/// 返回：registry 的索引标识；本地路径和 Git 依赖不属于 registry。
fn registry_index(source: &str) -> Option<&str> {
    source
        .strip_prefix("registry+")
        .or_else(|| source.starts_with("sparse+").then_some(source))
}

/// 业务作用：按标准 crates.io 别名及规范 URL 比较来源，其他 registry 保留 Git/sparse 差异。
/// 参数说明：
/// - `left`：Cargo 返回的索引身份。
/// - `right`：服务声明的逻辑身份。
///
/// 返回：身份一致时为 true，不以包名或下载地址判断等价性。
fn same_index(left: &str, right: &str) -> bool {
    match (
        naxus_core::registry_identity(left),
        naxus_core::registry_identity(right),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// 业务作用：从原锁中提取受管坐标，用于同版本内容迁移前的精确核对。
/// 参数说明：
/// - `lock`：原 Cargo.lock 模型。
/// - `config`：当前受管来源。
///
/// 返回：去重排序后的私有坐标，不吸收同名公开 crate。
fn lock_coordinates(lock: &toml::Value, config: &ClientConfig) -> Result<Vec<Coordinate>> {
    let mut selected = BTreeSet::new();
    if let Some(packages) = lock.get("package").and_then(toml::Value::as_array) {
        for package in packages {
            if package
                .get("source")
                .and_then(toml::Value::as_str)
                .is_some_and(|source| managed(config, source))
            {
                selected.insert((
                    package
                        .get("name")
                        .and_then(toml::Value::as_str)
                        .context("invalid locked name")?
                        .to_owned(),
                    package
                        .get("version")
                        .and_then(toml::Value::as_str)
                        .context("invalid locked version")?
                        .to_owned(),
                ));
            }
        }
    }
    Ok(selected
        .into_iter()
        .map(|(name, version)| Coordinate {
            name,
            version,
        })
        .collect())
}

/// 业务作用：只在临时锁中迁移受管来源和已验证摘要，保留原有 SemVer 选择。
/// 参数说明：
/// - `lock`：工作区锁的临时副本。
/// - `config`：受管来源范围。
/// - `index`：本次固定的新快照。
/// - `resolution`：精确坐标的新修订指纹。
///
/// 返回：完成临时迁移；公开来源和未受管包不修改。
fn transfer(
    lock: &mut toml::Value,
    config: &ClientConfig,
    index: &str,
    resolution: &Resolution,
) -> Result<()> {
    let mut sources = BTreeSet::new();
    if let Some(packages) = lock.get_mut("package").and_then(toml::Value::as_array_mut) {
        for package in packages.iter_mut() {
            let Some(source) = package
                .get("source")
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
            else {
                continue;
            };
            if !managed(config, &source) {
                continue;
            }
            let name = package
                .get("name")
                .and_then(toml::Value::as_str)
                .context("invalid locked name")?;
            let version = package
                .get("version")
                .and_then(toml::Value::as_str)
                .context("invalid locked version")?;
            let Some(revision) = resolution
                .packages
                .iter()
                .find(|item| item.coordinate.name == name && item.coordinate.version == version)
                .and_then(|item| item.revision.as_ref())
            else {
                continue;
            };
            validate_revision(revision)?;
            sources.insert(source);
            let package = package.as_table_mut().context("invalid lock package")?;
            package.insert("source".to_owned(), toml::Value::String(index.to_owned()));
            package.insert(
                "checksum".to_owned(),
                toml::Value::String(revision.entry.cksum.to_string()),
            );
        }
        for package in packages {
            if let Some(dependencies) = package
                .get_mut("dependencies")
                .and_then(toml::Value::as_array_mut)
            {
                for dependency in dependencies {
                    if let Some(value) = dependency.as_str() {
                        let mut value = value.to_owned();
                        for source in &sources {
                            value = value.replace(source, index);
                        }
                        *dependency = toml::Value::String(value);
                    }
                }
            }
        }
    }
    Ok(())
}

/// 业务作用：调用 Cargo 完成工作区描述或真实解析，所有可写解析使用调用者固定的配置。
/// 参数说明：
/// - `root`：工作区根。
/// - `configuration`：独立 Cargo 配置。
/// - `arguments`：用户解析参数。
/// - `no_deps`：只读 manifest 描述模式。
/// - `locked`：要求 Cargo 不更改临时锁。
///
/// 返回：真实 Cargo metadata 图，不从服务端自行模拟 Cargo 解析。
fn metadata(
    root: &Path,
    configuration: &Path,
    arguments: &[String],
    no_deps: bool,
    locked: bool,
) -> Result<Value> {
    let mut command = Command::new(cargo::executable());
    command
        .current_dir(root)
        .args(["metadata", "--format-version", "1", "--config"])
        .arg(configuration)
        .args(resolution_arguments(arguments));
    if no_deps {
        command.args(["--no-deps", "--offline"]);
    }
    if locked {
        command.arg("--locked");
    }
    Ok(serde_json::from_slice(
        &cargo::checked(&mut command)?.stdout,
    )?)
}

/// 业务作用：只向 metadata 传递影响依赖选择的选项，不误传运行程序参数。
/// 参数说明：
/// - `arguments`：用户 Cargo 命令参数。
///
/// 返回：规范化的 feature 与目标平台解析选项。
fn resolution_arguments(arguments: &[String]) -> Vec<String> {
    let mut output = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--" {
            break;
        }
        if matches!(argument.as_str(), "--features" | "-F" | "--target") {
            output.push(if argument == "--target" {
                "--filter-platform".to_owned()
            } else {
                argument.clone()
            });
            if let Some(value) = arguments.get(index + 1) {
                output.push(value.clone());
                index += 1;
            }
        } else if matches!(
            argument.as_str(),
            "--all-features" | "--no-default-features"
        ) || argument.starts_with("--features=")
        {
            output.push(argument.clone());
        } else if let Some(target) = argument.strip_prefix("--target=") {
            output.extend(["--filter-platform".to_owned(), target.to_owned()]);
        } else if let Some(features) = argument.strip_prefix("-F") {
            let features = features.strip_prefix('=').unwrap_or(features);
            output.push(format!("--features={features}"));
        }
        index += 1;
    }
    output
}

/// 业务作用：计算会影响依赖选择的工作区输入，不把源码改动误当作依赖变化。
/// 参数说明：
/// - `root`：工作区根。
/// - `metadata`：Cargo 真实工作区成员描述。
/// - `config`：服务与仓库映射。
/// - `arguments`：解析选项。
///
/// 返回：manifest、项目 Cargo 配置与解析输入的确定性摘要。
fn fingerprint(
    root: &Path,
    metadata: &Value,
    config: &ClientConfig,
    arguments: &[String],
) -> Result<ArtifactDigest> {
    let mut inputs = BTreeMap::new();
    inputs.insert("Cargo.toml".to_owned(), fs::read(root.join("Cargo.toml"))?);
    for package in metadata["packages"]
        .as_array()
        .context("Cargo metadata has no packages")?
    {
        let path = Path::new(
            package["manifest_path"]
                .as_str()
                .context("package has no manifest path")?,
        );
        inputs.insert(
            path.strip_prefix(root)?.to_string_lossy().into_owned(),
            fs::read(path)?,
        );
    }
    for name in [".cargo/config", ".cargo/config.toml"] {
        if root.join(name).try_exists()? {
            inputs.insert(name.to_owned(), fs::read(root.join(name))?);
        }
    }
    Ok(ArtifactDigest::of(&serde_json::to_vec(&(
        inputs,
        config.service.endpoint.as_str(),
        &config.service.repository,
        &config.service.service_uuid,
        &config.sources,
        resolution_arguments(arguments),
    ))?))
}

/// 业务作用：在 Cargo 开始网络解析前拒绝未受管的独立 registry 声明。
/// 参数说明：
/// - `metadata`：只读 manifest 描述。
/// - `logical`：受管 hosted 逻辑来源。
/// - `config`：已授权的公开来源映射。
///
/// 返回：所有 registry 依赖均有受管映射且 path 依赖属于本工作区成员才成功。
fn validate_manifest_sources(metadata: &Value, logical: &str, config: &ClientConfig) -> Result<()> {
    let packages = metadata["packages"]
        .as_array()
        .context("invalid Cargo metadata")?;
    let members = packages
        .iter()
        .map(|package| {
            Path::new(
                package["manifest_path"]
                    .as_str()
                    .context("package has no manifest path")?,
            )
            .parent()
            .context("package manifest has no parent")?
            .canonicalize()
            .map_err(anyhow::Error::from)
        })
        .collect::<Result<BTreeSet<_>>>()?;
    for package in packages {
        for dependency in package["dependencies"]
            .as_array()
            .context("invalid Cargo dependency list")?
        {
            if let Some(path) = dependency["path"].as_str() {
                // 双锁指纹覆盖工作区成员的 manifest；外部 path 未被固定，不能借用旧图跳过来源校验。
                ensure!(
                    members.contains(&Path::new(path).canonicalize()?),
                    "path dependencies must be members of the same workspace"
                );
            }
            if let Some(source) = dependency["source"].as_str() {
                ensure!(
                    registry_index(source).is_some(),
                    "git dependencies are not supported by the Naxus single-endpoint policy"
                );
                if let Some(index) = registry_index(source) {
                    ensure!(
                        index == logical
                            || config
                                .sources
                                .iter()
                                .any(|source| same_index(index, &source.logical_index)),
                        "dependency uses a registry without a Naxus source mapping"
                    );
                }
            }
        }
    }
    Ok(())
}

/// 业务作用：核对 Cargo 实际解析图的来源，私有坐标只能来自本次固定快照。
/// 参数说明：
/// - `graph`：真实 Cargo metadata 图。
/// - `index`：本次不可变私有索引。
/// - `config`：已授权的独立公开来源。
///
/// 返回：所有受管私有坐标；不接受未知 registry 或错误快照。
fn validate_graph(graph: &Value, index: &str, config: &ClientConfig) -> Result<Vec<Coordinate>> {
    let mut coordinates = BTreeSet::new();
    for package in graph["packages"]
        .as_array()
        .context("invalid Cargo resolution graph")?
    {
        if let Some(source) = package["source"].as_str() {
            ensure!(
                registry_index(source).is_some(),
                "resolved dependency bypasses the Naxus registry source policy"
            );
            if registry_index(source) == Some(index) {
                coordinates.insert((
                    package["name"]
                        .as_str()
                        .context("package has no name")?
                        .to_owned(),
                    package["version"]
                        .as_str()
                        .context("package has no version")?
                        .to_owned(),
                ));
            } else if let Some(index) = registry_index(source) {
                ensure!(
                    config
                        .sources
                        .iter()
                        .any(|source| same_index(index, &source.logical_index)),
                    "Cargo resolved a package from an unauthorized registry source"
                );
            }
        }
    }
    Ok(coordinates
        .into_iter()
        .map(|(name, version)| Coordinate {
            name,
            version,
        })
        .collect())
}
