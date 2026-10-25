use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{ensure, Context, Result};
use naxus_core::ArtifactDigest;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// 已完成校验和磁盘持久化的制品对象，尚不等于仓库提交成功。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct StoredObject {
    pub sha256: ArtifactDigest,
    pub size: u64,
}

/// 对象安装的内容身份、字节与空闲空间边界。
pub(crate) struct InstallationPolicy {
    pub max_bytes: u64,
    pub reserve_free_bytes: u64,
    pub expected: Option<ArtifactDigest>,
}

/// 业务作用：把严格摘要转换为对象存储中的确定性路径。
/// 参数说明：
/// - `root`：由仓库实例独占的可信数据根。
/// - `digest`：经过校验的 SHA-256。
///
/// 返回：数据根中的精确对象路径，不使用用户提供的任意路径片段。
pub(crate) fn object_path(root: &Path, digest: &ArtifactDigest) -> PathBuf {
    root.join("objects/sha256").join(digest.as_str())
}

/// 业务作用：将目录项变更持久化，防止文件已刷盘而重启后名称不可见。
/// 参数说明：
/// - `directory`：已经创建的本地目录。
///
/// 返回：目录同步完成；不支持所需持久化语义的文件系统返回错误。
pub(crate) fn sync_directory(directory: &Path) -> Result<()> {
    File::open(directory)?.sync_all()?;
    Ok(())
}

/// 业务作用：独占数据卷启动时清理上一个进程未完成的上传暂存，不能让反复中断耗尽空间。
/// 参数说明：
/// - `root`：已经取得实例独占权的数据根。
///
/// 返回：只删除 staging 内符合本服务 UUID.upload 命名的普通文件；未知内容拒绝自动处理。
pub(crate) fn recover_staging(root: &Path) -> Result<()> {
    let directory = root.join("staging");
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().context("invalid staging filename")?;
        let id = name
            .strip_suffix(".upload")
            .context("unknown staging entry")?;
        Uuid::parse_str(id)?;
        ensure!(
            entry.file_type()?.is_file(),
            "staging entry is not a regular file"
        );
        // 当前实例尚未开放上传，数据根独占证明这些精确路径不属于活动写入者。
        fs::remove_file(entry.path())?;
    }
    sync_directory(&directory)
}

/// 业务作用：流式核验磁盘对象，识别截断、损坏或摘要路径被替换的内容。
/// 参数说明：
/// - `root`：可信数据根。
/// - `digest`：文件必须满足的内容身份。
/// - `expected_size`：数据库记录的对象大小；未知时仍验证实际摘要。
///
/// 返回：校验后的对象信息；任何内容或类型不符都拒绝使用。
pub(crate) fn verify_object(
    root: &Path,
    digest: &ArtifactDigest,
    expected_size: Option<u64>,
) -> Result<StoredObject> {
    let path = object_path(root, digest);
    verify_path(&path, digest, expected_size)
}

/// 业务作用：核验正式对象或已隔离对象的实际字节，清除恢复不得只信任文件名。
/// 参数说明：
/// - `path`：由存储层构造的精确正式或隔离路径。
/// - `digest`：预期内容身份。
/// - `expected_size`：已持久化大小。
///
/// 返回：文件类型、大小和 SHA-256 均匹配的对象描述。
pub(crate) fn verify_path(
    path: &Path,
    digest: &ArtifactDigest,
    expected_size: Option<u64>,
) -> Result<StoredObject> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file(),
        "object is not a regular file"
    );
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut size = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .context("object size overflow")?;
        hasher.update(&buffer[..count]);
    }
    ensure!(
        hex::encode(hasher.finalize()) == digest.as_str(),
        "object checksum mismatch"
    );
    ensure!(
        expected_size.is_none_or(|value| value == size),
        "object size mismatch"
    );
    Ok(StoredObject {
        sha256: digest.clone(),
        size,
    })
}

/// 业务作用：在同一文件系统完成流式暂存、摘要核验和不可变对象安装。
/// 参数说明：
/// - `root`：可信数据根。
/// - `reader`：原始制品字节流；调用者负责入站超时和并发预算。
/// - `policy`：单制品字节、摘要和必须保留的空闲空间边界。
/// - `gate`：仅用于正式对象安装的生命周期门禁。
/// - `validate`：在私有暂存文件上执行归档及协议语义校验，不得建立正式引用。
/// - `before`：安装前在生命周期门禁内复验删除状态。
/// - `installed`：正式字节持久化后登记当前存储代次。
///
/// 返回：文件及目录持久化后返回对象；失败清理本次精确暂存路径，不产生仓库引用。
pub(crate) fn install_object<T>(
    root: &Path,
    reader: &mut dyn Read,
    policy: InstallationPolicy,
    gate: &Mutex<()>,
    validate: impl FnOnce(&mut File, &StoredObject) -> Result<T>,
    before: impl FnOnce(&ArtifactDigest) -> Result<()>,
    installed: impl FnOnce(&StoredObject) -> Result<()>,
) -> Result<(StoredObject, T)> {
    let limit = policy.max_bytes;
    let reserve = policy.reserve_free_bytes;
    ensure!(
        fs2::available_space(root)? > reserve,
        crate::RepositoryFailure::InsufficientStorage
    );
    let staging = root
        .join("staging")
        .join(format!("{}.upload", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&staging)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut size = 0_u64;
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            size = size
                .checked_add(count as u64)
                .context("artifact size overflow")?;
            // 每次写入前复验容量，容量不足只能留下不可见暂存，不能生成缺字节的正式对象。
            ensure!(size <= limit, "artifact exceeds size limit");
            ensure!(
                fs2::available_space(root)? >= reserve.saturating_add(count as u64),
                crate::RepositoryFailure::InsufficientStorage
            );
            hasher.update(&buffer[..count]);
            file.write_all(&buffer[..count])?;
        }
        let digest: ArtifactDigest = hex::encode(hasher.finalize()).parse()?;
        ensure!(
            policy
                .expected
                .as_ref()
                .is_none_or(|expected| expected == &digest),
            "artifact checksum mismatch"
        );
        let expected = &digest;
        file.sync_all()?;
        drop(file);
        let object = StoredObject {
            sha256: digest.clone(),
            size,
        };
        // 归档、长度和来源检查必须在提升正式对象前完成；失败只清理当前精确暂存路径。
        let validated = validate(&mut File::open(&staging)?, &object)?;
        // 网络暂存不占用安装门禁，只有校验完成的对象才串行进入正式路径。
        let _guard = gate
            .lock()
            .map_err(|_| anyhow::anyhow!("object lifecycle lock is unavailable"))?;
        before(expected)?;
        let destination = object_path(root, expected);
        if destination.try_exists()? {
            verify_object(root, expected, Some(size))?;
            fs::remove_file(&staging)?;
        } else {
            // 正式对象先落盘并同步目录，后续 SQLite 提交才能安全引用该摘要。
            fs::rename(&staging, &destination)?;
            sync_directory(&root.join("objects/sha256"))?;
        }
        sync_directory(&root.join("staging"))?;
        installed(&object)?;
        Ok((object, validated))
    })();
    if result.is_err() && staging.try_exists()? {
        fs::remove_file(&staging)?;
        sync_directory(&root.join("staging"))?;
    }
    result
}
