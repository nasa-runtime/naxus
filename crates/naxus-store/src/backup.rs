use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{ensure, Context, Result};
use naxus_core::ArtifactDigest;
use rusqlite::{backup::Backup, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{objects, Store, StoreOptions};

/// 一致备份的精确对象清单，包含受保护的账号元数据但不包含外部部署配置。
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupManifest {
    pub format: u32,
    pub service_uuid: String,
    pub database_sha256: ArtifactDigest,
    pub objects: Vec<crate::StoredObject>,
    pub bootstrap_sha256: Option<ArtifactDigest>,
}

impl Store {
    /// 业务作用：使用 SQLite 一致备份和独立对象副本生成可离线恢复的数据集。
    /// 参数说明：
    /// - `destination`：尚不存在的备份目录，必须位于仓库根以外。
    ///
    /// 返回：数据库、现存对象、可选引导口令及清单全部同步后原子公开备份；中断只留下未完成暂存目录。
    pub fn backup(&self, destination: &Path) -> Result<BackupManifest> {
        let (destination, staging) = prepare(destination, Some(&self.root))?;
        let result = (|| {
            let _objects = self
                .object_gate
                .lock()
                .map_err(|_| anyhow::anyhow!("object lifecycle unavailable"))?;
            let connection = self.connection()?;
            let mut statement = connection.prepare(
                "SELECT sha256, size FROM objects WHERE state = 'present' ORDER BY sha256",
            )?;
            let objects = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .map(|(sha256, size)| {
                    Ok(crate::StoredObject {
                        sha256: sha256.parse()?,
                        size,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let target_database = staging.join("naxus.sqlite3");
            let mut target = Connection::open(&target_database)?;
            // 数据库连接和对象生命周期同时保持独占，清单中的引用与复制的字节属于同一可恢复状态。
            Backup::new(&connection, &mut target)?.run_to_completion(
                128,
                Duration::from_millis(1),
                None,
            )?;
            target.close().map_err(|(_, error)| error)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&target_database, fs::Permissions::from_mode(0o600))?;
            }
            File::open(&target_database)?.sync_all()?;
            let target_objects = staging.join("objects/sha256");
            fs::create_dir_all(&target_objects)?;
            for object in &objects {
                let source = objects::object_path(&self.root, &object.sha256);
                objects::verify_path(&source, &object.sha256, Some(object.size))?;
                copy_verified(
                    &source,
                    &target_objects.join(object.sha256.as_str()),
                    &object.sha256,
                )?;
            }
            let bootstrap = self.root.join("bootstrap/admin.password");
            let bootstrap_sha256 = if bootstrap.try_exists()? {
                let digest = file_digest(&bootstrap)?;
                fs::create_dir(staging.join("bootstrap"))?;
                copy_verified(
                    &bootstrap,
                    &staging.join("bootstrap/admin.password"),
                    &digest,
                )?;
                objects::sync_directory(&staging.join("bootstrap"))?;
                Some(digest)
            } else {
                None
            };
            let manifest = BackupManifest {
                format: 1,
                service_uuid: self.service_id().to_owned(),
                database_sha256: file_digest(&target_database)?,
                objects,
                bootstrap_sha256,
            };
            let description = serde_json::to_vec_pretty(&manifest)?;
            ensure!(
                description.len() <= 64 * 1024 * 1024,
                crate::ResourceLimitExceeded("backup manifest exceeds restore budget")
            );
            write_private(&staging.join("backup.json"), &description)?;
            objects::sync_directory(&target_objects)?;
            objects::sync_directory(&staging.join("objects"))?;
            publish(&staging, &destination)?;
            Ok(manifest)
        })();
        remove_incomplete(&staging, &result)?;
        result
    }

    /// 业务作用：在全新数据目录恢复备份并复验稳定身份及完整引用，不覆盖任何现有仓库。
    /// 参数说明：
    /// - `backup`：已经完成的备份目录。
    /// - `destination`：尚不存在的新数据根。
    /// - `options`：恢复校验使用的存储容量和读取预算。
    ///
    /// 返回：全部引用通过 Store 初始化检查后原子公开新数据根；损坏备份拒绝恢复。
    pub fn restore(backup: &Path, destination: &Path, options: StoreOptions) -> Result<()> {
        let backup = backup.canonicalize()?;
        let manifest_path = backup.join("backup.json");
        ensure!(
            fs::symlink_metadata(&manifest_path)?.file_type().is_file(),
            "backup manifest is not a regular file"
        );
        ensure!(
            fs::metadata(&manifest_path)?.len() <= 64 * 1024 * 1024,
            "backup manifest exceeds limit"
        );
        let manifest: BackupManifest = serde_json::from_slice(&fs::read(manifest_path)?)?;
        ensure!(manifest.format == 1, "unsupported backup format");
        Uuid::parse_str(&manifest.service_uuid)?;
        let (destination, staging) = prepare(destination, Some(&backup))?;
        let result = (|| {
            copy_verified(
                &backup.join("naxus.sqlite3"),
                &staging.join("naxus.sqlite3"),
                &manifest.database_sha256,
            )?;
            fs::create_dir_all(staging.join("objects/sha256"))?;
            let mut unique = std::collections::BTreeSet::new();
            for object in &manifest.objects {
                ensure!(
                    unique.insert(object.sha256.clone()),
                    "duplicate backup object"
                );
                let source = backup.join("objects/sha256").join(object.sha256.as_str());
                objects::verify_path(&source, &object.sha256, Some(object.size))?;
                copy_verified(
                    &source,
                    &staging.join("objects/sha256").join(object.sha256.as_str()),
                    &object.sha256,
                )?;
            }
            if let Some(digest) = &manifest.bootstrap_sha256 {
                fs::create_dir(staging.join("bootstrap"))?;
                copy_verified(
                    &backup.join("bootstrap/admin.password"),
                    &staging.join("bootstrap/admin.password"),
                    digest,
                )?;
                objects::sync_directory(&staging.join("bootstrap"))?;
            }
            // 先使用正式启动合同验证所有数据库引用和对象，再公开目标目录；身份不能在恢复时重建。
            let restored = Store::open(&staging, options)?;
            ensure!(
                restored.service_id() == manifest.service_uuid,
                "backup service identity mismatch"
            );
            drop(restored);
            objects::sync_directory(&staging.join("objects/sha256"))?;
            objects::sync_directory(&staging.join("objects"))?;
            publish(&staging, &destination)
        })();
        remove_incomplete(&staging, &result)?;
        result
    }
}

/// 业务作用：为备份或恢复建立受限暂存根，防止目标嵌入源目录或覆盖已有数据。
/// 参数说明：
/// - `destination`：调用者指定的新目录。
/// - `source`：禁止嵌入的源根。
///
/// 返回：规范化目标和精确 UUID 暂存目录；父目录必须事先存在。
fn prepare(destination: &Path, source: Option<&Path>) -> Result<(PathBuf, PathBuf)> {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    naxus_core::validate_local_filesystem(&parent)?;
    let name = destination
        .file_name()
        .context("backup destination has no name")?;
    let destination = parent.join(name);
    ensure!(
        !destination.try_exists()?,
        "backup destination already exists"
    );
    ensure!(
        source.is_none_or(|source| !destination.starts_with(source)),
        "backup destination must be outside the source directory"
    );
    let staging = parent.join(format!(".naxus-backup-{}", Uuid::new_v4()));
    fs::create_dir(&staging)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o700))?;
    }
    objects::sync_directory(&parent)?;
    Ok((destination, staging))
}

/// 业务作用：复制独立文件并核对身份，不通过硬链接共享后来可能删除或损坏的原文件。
/// 参数说明：
/// - `source`：可信数据集中的实体文件。
/// - `destination`：本次暂存根中尚不存在的精确文件。
/// - `expected`：清单声明的完整摘要。
///
/// 返回：完整副本及其同步完成；摘要不一致或空间不足时拒绝。
fn copy_verified(source: &Path, destination: &Path, expected: &ArtifactDigest) -> Result<()> {
    ensure!(
        fs::symlink_metadata(source)?.file_type().is_file(),
        "backup source is not a regular file"
    );
    let parent = destination.parent().context("backup file has no parent")?;
    let mut input = File::open(source)?;
    let mut output = private_file(destination)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        ensure!(
            fs2::available_space(parent)? > count as u64 + 16 * 1024 * 1024,
            crate::RepositoryFailure::InsufficientStorage
        );
        output.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
    }
    ensure!(
        hex::encode(hasher.finalize()) == expected.as_str(),
        "backup checksum mismatch"
    );
    output.sync_all()?;
    Ok(())
}

/// 业务作用：核对备份元数据文件的完整字节身份。
/// 参数说明：
/// - `path`：需要计算摘要的文件。
///
/// 返回：按固定缓冲读取的 SHA-256，不把大型数据库载入内存。
fn file_digest(path: &Path) -> Result<ArtifactDigest> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file(),
        "backup file is not regular"
    );
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    hex::encode(hasher.finalize()).parse()
}

/// 业务作用：创建只允许当前用户读取的新备份文件，避免账号散列和引导材料公开。
/// 参数说明：
/// - `path`：尚不存在的备份文件。
///
/// 返回：独占创建的写入句柄，不覆盖已有内容。
fn private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

/// 业务作用：同步写入完整的备份清单，清单存在本身不代表目录已经正式公开。
/// 参数说明：
/// - `path`：暂存根中的清单文件。
/// - `bytes`：完整序列化清单。
///
/// 返回：文件内容同步完成。
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = private_file(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// 业务作用：完整数据集落盘后公开目录，失败数据集不冒充可恢复备份。
/// 参数说明：
/// - `staging`：本次精确暂存根。
/// - `destination`：尚不存在的正式目标。
///
/// 返回：目录改名及父目录同步完成。
fn publish(staging: &Path, destination: &Path) -> Result<()> {
    objects::sync_directory(staging)?;
    ensure!(
        !destination.try_exists()?,
        "backup destination appeared during operation"
    );
    fs::rename(staging, destination)?;
    objects::sync_directory(destination.parent().context("backup has no parent")?)
}

/// 业务作用：普通失败时清理本操作持有的精确暂存根，保留已经公开的成功目录。
/// 参数说明：
/// - `staging`：由 prepare 创建的精确路径。
/// - `result`：当前操作结果。
///
/// 返回：只清理未完成的暂存根，不扫描其他备份或数据目录。
fn remove_incomplete<T>(staging: &Path, result: &Result<T>) -> Result<()> {
    if result.is_err() && staging.try_exists()? {
        fs::remove_dir_all(staging)?;
        objects::sync_directory(staging.parent().context("staging has no parent")?)?;
    }
    Ok(())
}
