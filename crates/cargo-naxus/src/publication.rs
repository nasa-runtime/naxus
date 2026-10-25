use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{ensure, Context, Result};
use naxus_client::ClientConfig;
use naxus_core::{
    ArtifactDigest, IndexEntry, RepositoryPolicy, Snapshot, MAX_BATCH_ITEMS,
    MAX_PUBLICATION_METADATA_BYTES,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::remote::{Remote, Repository};

/// 原生打包完成且已检查归档的固定输入，路径只在准备阶段使用。
pub(crate) struct PreparedArtifact {
    pub path: PathBuf,
    pub entry: IndexEntry,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    file: String,
    size: u64,
    entry: IndexEntry,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    format: u32,
    id: String,
    service_id: String,
    endpoint: String,
    repository_id: String,
    repository_name: String,
    policy: RepositoryPolicy,
    actor_id: ArtifactDigest,
    expected_head: String,
    key: String,
    request_sha256: ArtifactDigest,
    batch_id: Option<String>,
    phase: String,
    artifacts: Vec<Artifact>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    digest: ArtifactDigest,
    pending: Pending,
}

#[derive(Deserialize)]
struct Caller {
    subject_id: ArtifactDigest,
}

#[derive(Deserialize)]
struct Batch {
    id: String,
    repository_id: String,
    actor: String,
    expected_head: String,
    state: String,
    snapshot_id: Option<String>,
    entries: Vec<IndexEntry>,
}

/// 业务作用：在首个远端写请求前保存固定制品及发布意图，使进程退出后仍可重放同一组字节。
/// 参数说明：
/// - `root`：已取得客户端独占权的工作区。
/// - `remote`：当前发布连接及配置。
/// - `repository`：已确认的目标仓库。
/// - `service_id`：已协商的稳定服务 UUID。
/// - `expected`：打包前固定的 HEAD。
/// - `artifacts`：原生打包并完成静态检查的精确归档。
///
/// 返回：原批次提交后返回零；远端阶段失败保留已建立的日志和固定制品，后续命令先恢复。
pub fn publish(
    root: &Path,
    remote: &Remote,
    repository: &Repository,
    service_id: &str,
    expected: &Snapshot,
    artifacts: &[PreparedArtifact],
) -> Result<u8> {
    ensure!(
        !root.join(".naxus/publication.json").try_exists()?,
        "pending publication must be recovered first"
    );
    ensure!(
        !artifacts.is_empty() && artifacts.len() <= MAX_BATCH_ITEMS,
        "invalid publication artifact count"
    );
    let caller: Caller = remote.get("/api/naxus/v1/identity")?;
    let id = Uuid::new_v4().to_string();
    let base = root.join(".naxus/publications");
    naxus_core::create_private_directory(&base)?;
    let directory = base.join(&id);
    naxus_core::create_private_directory(&directory)?;
    let mut retained = Vec::new();
    for (index, artifact) in artifacts.iter().enumerate() {
        artifact.entry.validate()?;
        let name = format!("{index}.crate");
        let destination = directory.join(&name);
        fs::copy(&artifact.path, &destination)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&destination, fs::Permissions::from_mode(0o600))?;
        }
        File::open(&destination)?.sync_all()?;
        ensure!(
            file_digest(&destination)? == artifact.entry.cksum,
            "publication artifact changed during preparation"
        );
        retained.push(Artifact {
            file: name,
            size: fs::metadata(&destination)?.len(),
            entry: artifact.entry.clone(),
        });
    }
    File::open(&directory)?.sync_all()?;
    let mut pending = Pending {
        format: 1,
        id,
        service_id: service_id.to_owned(),
        endpoint: remote
            .config
            .service
            .endpoint
            .as_str()
            .trim_end_matches('/')
            .to_owned(),
        repository_id: repository.id.clone(),
        repository_name: repository.name.clone(),
        policy: repository.policy,
        actor_id: caller.subject_id,
        expected_head: expected.id.clone(),
        key: Uuid::new_v4().to_string(),
        request_sha256: ArtifactDigest::of(b""),
        batch_id: None,
        phase: "prepared".to_owned(),
        artifacts: retained,
    };
    pending.request_sha256 = ArtifactDigest::of(&serde_json::to_vec(&request(&pending))?);
    ensure!(
        remote.capabilities()?.service_uuid == pending.service_id,
        "publication service identity changed during packaging"
    );
    // 固定归档和目录链先同步，发布日志最后公开；只有日志存在后才允许向服务端创建批次。
    save(root, &pending)?;
    resume(root, remote, pending, false)?;
    Ok(0)
}

/// 业务作用：在新命令或无主目录清理前恢复未完成发布，不重新打包变化后的源码。
/// 参数说明：
/// - `root`：已取得独占权的工作区。
/// - `configuration`：当前凭据文件解析的可信配置路径。
/// - `config`：当前服务与仓库配置。
/// - `abort`：显式终止未提交发布；已提交内容只确认，不撤销。
///
/// 返回：存在并完成原操作时为 true；无待恢复意图为 false；无法确认时保留现场并失败。
pub fn recover(
    root: &Path,
    configuration: &Path,
    config: &ClientConfig,
    abort: bool,
) -> Result<bool> {
    discard_prepared(root)?;
    let path = root.join(".naxus/publication.json");
    if !path.try_exists()? {
        cleanup(root, None)?;
        ensure!(!abort, "there is no pending publication");
        return Ok(false);
    }
    ensure!(
        fs::symlink_metadata(&path)?.file_type().is_file()
            && fs::metadata(&path)?.len() <= MAX_PUBLICATION_METADATA_BYTES as u64,
        "invalid publication journal file"
    );
    let envelope: Envelope = serde_json::from_slice(&fs::read(&path)?)
        .map_err(|_| anyhow::anyhow!("invalid publication journal"))?;
    let pending = envelope.pending;
    ensure!(
        envelope.digest == ArtifactDigest::of(&serde_json::to_vec(&pending)?),
        "publication journal checksum mismatch"
    );
    ensure!(
        pending.format == 1
            && matches!(
                pending.phase.as_str(),
                "prepared" | "uploading" | "committing"
            )
            && pending.endpoint == config.service.endpoint.as_str().trim_end_matches('/')
            && Uuid::parse_str(&pending.id)?.to_string() == pending.id
            && !pending.artifacts.is_empty()
            && pending.artifacts.len() <= MAX_BATCH_ITEMS,
        "publication journal identity mismatch"
    );
    for id in [
        &pending.service_id,
        &pending.repository_id,
        &pending.expected_head,
        &pending.key,
    ] {
        Uuid::parse_str(id)?;
    }
    ensure!(
        pending.request_sha256 == ArtifactDigest::of(&serde_json::to_vec(&request(&pending))?),
        "publication request changed"
    );
    let mut config = config.clone();
    // 目标来自已校验日志，连接与凭据仍来自当前可信配置；不能让日志把凭据改送其他服务。
    config.service.repository = pending.repository_id.clone();
    let remote = Remote::new(config, configuration.to_path_buf())?;
    let capabilities = remote.capabilities()?;
    let repository = remote.repository()?;
    let caller: Caller = remote.get("/api/naxus/v1/identity")?;
    ensure!(
        capabilities.service_uuid == pending.service_id
            && repository.id == pending.repository_id
            && repository.policy == pending.policy
            && caller.subject_id == pending.actor_id,
        "pending publication belongs to a different service, repository or principal"
    );
    cleanup(root, Some(&pending.id))?;
    resume(root, &remote, pending, abort)?;
    if !abort {
        eprintln!("Recovered the previous publication from retained artifacts; run publish again to publish any new source changes.");
    }
    Ok(true)
}

/// 业务作用：用原创建键读取批次终态或补传固定制品，提交响应丢失也不创建另一批次。
/// 参数说明：
/// - `root`：持有独占权的工作区。
/// - `remote`：已经核对服务和主体的连接。
/// - `pending`：持久发布意图及固定归档集合。
/// - `abort`：调用方是否明确要求终止未提交批次。
///
/// 返回：提交或明确终止后持久保存收据并清理本次目录；不确定结果保留日志。
fn resume(root: &Path, remote: &Remote, mut pending: Pending, abort: bool) -> Result<()> {
    let base = format!(
        "/api/naxus/v1/repositories/{}/batches",
        pending.repository_id
    );
    let mut batch: Batch = if let Some(id) = &pending.batch_id {
        Uuid::parse_str(id)?;
        remote.batch(&format!("{base}/{id}"), None)?
    } else {
        remote.batch(&base, Some(&request(&pending)))?
    };
    validate_batch(&pending, &batch)?;
    if pending.batch_id.is_none() {
        pending.batch_id = Some(batch.id.clone());
        pending.phase = "uploading".to_owned();
        save(root, &pending)?;
    }
    let path = format!("{base}/{}", batch.id);
    if abort && batch.state == "open" {
        remote.close(&path)?;
        batch = remote.batch(&path, None)?;
        validate_batch(&pending, &batch)?;
    }
    if abort && matches!(batch.state.as_str(), "aborted" | "expired") {
        return finish(root, &pending, &batch.state, None);
    }
    // 终止请求未被服务端确认时保留原意图，不能转入补传和提交而产生相反的业务副作用。
    ensure!(
        !abort || batch.state == "committed",
        "publication abort was not confirmed; retry to recover the original batch"
    );
    ensure!(matches!(batch.state.as_str(), "open" | "committed"), "pending publication is {}; use publish --abort-pending to acknowledge a terminal batch, or retry while validating", batch.state);
    if batch.state == "open" {
        let directory = root.join(".naxus/publications").join(&pending.id);
        ensure!(
            fs::symlink_metadata(&directory)?.file_type().is_dir(),
            "publication directory was replaced"
        );
        // 在任何续传前核对全部归档，不能上传一部分旧字节后才发现另一文件已经损坏。
        for (index, artifact) in pending.artifacts.iter().enumerate() {
            artifact.entry.validate()?;
            ensure!(
                artifact.file == format!("{index}.crate"),
                "invalid publication artifact path"
            );
            let file = directory.join(&artifact.file);
            ensure!(
                fs::symlink_metadata(&file)?.file_type().is_file()
                    && fs::metadata(&file)?.len() == artifact.size
                    && file_digest(&file)? == artifact.entry.cksum,
                "retained publication artifact is missing or changed"
            );
        }
        for artifact in &pending.artifacts {
            if !batch.entries.iter().any(|entry| entry == &artifact.entry) {
                let uploaded: IndexEntry = serde_json::from_value(remote.upload(
                    &format!(
                        "{path}/crates/{}/{}",
                        artifact.entry.name, artifact.entry.vers
                    ),
                    &directory.join(&artifact.file),
                    &artifact.entry.cksum,
                )?)?;
                ensure!(
                    uploaded == artifact.entry,
                    "uploaded artifact metadata changed"
                );
            }
        }
        pending.phase = "committing".to_owned();
        save(root, &pending)?;
        let committed: Result<Snapshot> =
            remote.post(&format!("{path}/commit"), &serde_json::json!({}));
        if committed.is_err() {
            // 请求已送达后不能把连接失败解释为未提交；只读取原批次的持久终态。
            batch = remote.batch(&path, None)?;
            validate_batch(&pending, &batch)?;
            ensure!(
                batch.state == "committed",
                "publication result is not committed; retry to recover the original batch"
            );
        } else {
            batch = remote.batch(&path, None)?;
            validate_batch(&pending, &batch)?;
            let snapshot = committed?;
            ensure!(
                batch.snapshot_id.as_deref() == Some(snapshot.id.as_str())
                    && snapshot.repository_id == pending.repository_id,
                "publication commit receipt differs from batch"
            );
        }
    }
    ensure!(
        batch.state == "committed" && batch.entries.len() == pending.artifacts.len(),
        "committed publication differs from the fixed artifact set"
    );
    let id = batch
        .snapshot_id
        .as_deref()
        .context("committed batch omitted snapshot")?;
    Uuid::parse_str(id)?;
    let snapshot: Snapshot = remote.get(&format!(
        "/api/naxus/v1/repositories/{}/snapshots/{id}",
        pending.repository_id
    ))?;
    ensure!(
        snapshot.id == id && snapshot.repository_id == pending.repository_id,
        "publication snapshot identity mismatch"
    );
    finish(root, &pending, "committed", Some(&snapshot))
}

/// 业务作用：核对批次归属和已上传条目，禁止日志或响应把原意图换成另一组内容。
/// 参数说明：
/// - `pending`：已校验的本地发布意图。
/// - `batch`：服务端返回的原批次。
///
/// 返回：所有条目均是固定集合的唯一子集且身份相符才成功。
fn validate_batch(pending: &Pending, batch: &Batch) -> Result<()> {
    Uuid::parse_str(&batch.id)?;
    ensure!(
        pending.batch_id.as_ref().is_none_or(|id| id == &batch.id)
            && batch.repository_id == pending.repository_id
            && batch.expected_head == pending.expected_head
            && ArtifactDigest::of(batch.actor.as_bytes()) == pending.actor_id,
        "publication batch identity mismatch"
    );
    let mut coordinates = std::collections::BTreeSet::new();
    for entry in &batch.entries {
        ensure!(
            coordinates.insert((&entry.name, &entry.vers))
                && pending
                    .artifacts
                    .iter()
                    .any(|artifact| &artifact.entry == entry),
            "publication batch contains unexpected content"
        );
    }
    Ok(())
}

/// 业务作用：重建与日志摘要绑定的批次创建请求，重试不生成新的幂等键。
/// 参数说明：
/// - `pending`：固定预期 HEAD 与幂等身份。
///
/// 返回：不含凭据的原创建请求。
fn request(pending: &Pending) -> serde_json::Value {
    serde_json::json!({"expected_head": pending.expected_head, "idempotency_key": pending.key})
}

/// 业务作用：原子保存发布状态并同步父目录，远端副作用只能发生在对应日志公开之后。
/// 参数说明：
/// - `root`：持有独占权的工作区。
/// - `pending`：完整且有界的发布意图。
///
/// 返回：日志正文和目录项均持久化才成功。
fn save(root: &Path, pending: &Pending) -> Result<()> {
    let bytes = serde_json::to_vec(&Envelope {
        digest: ArtifactDigest::of(&serde_json::to_vec(pending)?),
        pending: pending.clone(),
    })?;
    ensure!(
        bytes.len() <= MAX_PUBLICATION_METADATA_BYTES,
        "publication journal exceeds limit"
    );
    atomic_write(root, "publication", &bytes)
}

/// 业务作用：先保存终态收据再解除本地恢复权威，避免清理中断导致重新发布。
/// 参数说明：
/// - `root`：持有独占权的工作区。
/// - `pending`：原发布意图。
/// - `state`：服务端确认的持久终态。
/// - `snapshot`：提交成功时的不可变快照。
///
/// 返回：收据持久化并完成本次目录清理后报告终态；清理失败可由下次命令继续。
fn finish(root: &Path, pending: &Pending, state: &str, snapshot: Option<&Snapshot>) -> Result<()> {
    atomic_write(
        root,
        "publication-receipt",
        &serde_json::to_vec(
            &serde_json::json!({"publication": pending, "state": state, "snapshot": snapshot}),
        )?,
    )?;
    fs::remove_file(root.join(".naxus/publication.json"))?;
    File::open(root.join(".naxus"))?.sync_all()?;
    cleanup(root, None)?;
    if let Some(snapshot) = snapshot {
        println!(
            "Published {} package(s) to {}; snapshot {} (original batch {})",
            pending.artifacts.len(),
            pending.repository_name,
            snapshot.id,
            pending.batch_id.as_deref().unwrap_or_default()
        );
    } else {
        println!("Publication ended: {state}; committed artifacts were not removed");
    }
    Ok(())
}

/// 业务作用：通过受限临时文件和同目录改名公开发布日志或收据。
/// 参数说明：
/// - `root`：可信工作区根。
/// - `name`：模块内部固定的日志用途。
/// - `bytes`：完整序列化内容。
///
/// 返回：正文及父目录同步后成功，不原地截断已经公开的日志。
fn atomic_write(root: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let directory = root.join(".naxus");
    let path = directory.join(format!("{name}.pending"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&path, directory.join(format!("{name}.json")))?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

/// 业务作用：独占恢复时丢弃尚未公开的精确日志临时文件，不把它们当成远端操作权威。
/// 参数说明：
/// - `root`：已经持有客户端锁的工作区。
///
/// 返回：仅处理两个固定名称的普通文件，异常类型拒绝自动清理。
fn discard_prepared(root: &Path) -> Result<()> {
    for name in ["publication.pending", "publication-receipt.pending"] {
        let path = root.join(".naxus").join(name);
        if path.try_exists()? {
            ensure!(
                fs::symlink_metadata(&path)?.file_type().is_file(),
                "publication temporary file was replaced"
            );
            fs::remove_file(path)?;
        }
    }
    File::open(root.join(".naxus"))?.sync_all()?;
    Ok(())
}

/// 业务作用：在没有活动日志引用时回收固定制品副本，不删除未知文件或另一发布的恢复材料。
/// 参数说明：
/// - `root`：已经持有独占权的工作区。
/// - `retained`：当前恢复日志保护的唯一目录 UUID。
///
/// 返回：仅清理完整验证的 UUID 目录及数字命名归档；异常现场保留并拒绝继续。
fn cleanup(root: &Path, retained: Option<&str>) -> Result<()> {
    let base = root.join(".naxus/publications");
    if !base.try_exists()? {
        return Ok(());
    }
    ensure!(
        fs::symlink_metadata(&base)?.file_type().is_dir(),
        "publication root was replaced"
    );
    let mut unused = Vec::new();
    for item in fs::read_dir(&base)? {
        let item = item?;
        let name = item
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("invalid publication directory"))?;
        ensure!(
            Uuid::parse_str(&name)?.to_string() == name && item.file_type()?.is_dir(),
            "publication directory was replaced"
        );
        if retained == Some(name.as_str()) {
            continue;
        }
        ensure!(unused.len() < 1024, "publication cleanup budget exceeded");
        for artifact in fs::read_dir(item.path())? {
            let artifact = artifact?;
            let name = artifact.file_name();
            ensure!(
                artifact.file_type()?.is_file()
                    && name
                        .to_str()
                        .and_then(|name| name.strip_suffix(".crate"))
                        .and_then(|index| index.parse::<usize>().ok())
                        .is_some_and(|index| index < MAX_BATCH_ITEMS),
                "unexpected publication artifact"
            );
        }
        unused.push(item.path());
    }
    // 先验证所有候选路径，再删除无主副本；持有日志的目录始终被排除。
    for path in unused {
        fs::remove_dir_all(path)?;
    }
    File::open(base)?.sync_all()?;
    Ok(())
}

/// 业务作用：流式核对本地固定归档，恢复不依赖原项目源码仍然存在。
/// 参数说明：
/// - `path`：当前发布的精确归档文件。
///
/// 返回：原始字节 SHA-256；读取失败停止恢复。
fn file_digest(path: &Path) -> Result<ArtifactDigest> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    format!("{:x}", digest.finalize()).parse()
}
