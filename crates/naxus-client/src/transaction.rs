use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use anyhow::{ensure, Context, Result};
use naxus_core::ArtifactDigest;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 客户端在恢复与更新锁文件期间持有的工作区独占权。
pub struct ClientGuard {
    file: File,
    root: PathBuf,
}

static OPEN_WORKSPACES: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();

#[derive(Serialize, Deserialize)]
struct Journal {
    id: String,
    new_cargo: ArtifactDigest,
    new_naxus: ArtifactDigest,
    old_cargo: Option<ArtifactDigest>,
    old_naxus: Option<ArtifactDigest>,
}

impl ClientGuard {
    /// 业务作用：在恢复或更新双锁前取得工作区独占权。
    /// 参数说明：
    /// - `root`：业务工作区根目录。
    ///
    /// 返回：持续到命令结束的守卫；并发命令拒绝，不建立无界等待。
    pub fn acquire(root: &Path) -> Result<Self> {
        let root = root.canonicalize()?;
        naxus_core::validate_local_filesystem(&root)?;
        let mut workspaces = OPEN_WORKSPACES
            .get_or_init(|| Mutex::new(BTreeSet::new()))
            .lock()
            .map_err(|_| anyhow::anyhow!("workspace ownership unavailable"))?;
        // 文件锁可能按进程合并所有者，必须同时拒绝本进程内的第二个客户端实例。
        ensure!(
            !workspaces.contains(&root),
            "another Naxus command owns this workspace"
        );
        let directory = root.join(".naxus");
        naxus_core::create_private_directory(&directory)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(directory.join("client.lock"))?;
        fs2::FileExt::try_lock_exclusive(&file)
            .context("another Naxus command owns this workspace")?;
        workspaces.insert(root.clone());
        Ok(Self {
            file,
            root,
        })
    }
}

impl Drop for ClientGuard {
    /// 业务作用：命令结束时释放更新权，锁文件本身供后续命令复用。
    /// 参数说明: 无。
    ///
    /// 返回：释放操作系统锁，文件句柄随后关闭。
    fn drop(&mut self) {
        if let Some(workspaces) = OPEN_WORKSPACES.get() {
            if let Ok(mut workspaces) = workspaces.lock() {
                // 跨进程权威先释放，随后才允许本进程中的下一位更新者进入。
                let _ = fs2::FileExt::unlock(&self.file);
                workspaces.remove(&self.root);
            }
        }
    }
}

/// 业务作用：按持久日志恢复双锁，拒绝猜测覆盖日志之外的工作区内容。
/// 参数说明：
/// - `root`：已取得客户端锁的工作区根。
///
/// 返回：完整新状态或完整旧状态；未知改动、缺失恢复材料时明确失败。
pub fn recover_locks(root: &Path) -> Result<()> {
    let path = root.join(".naxus/lock-transaction.json");
    if !path.try_exists()? {
        return Ok(());
    }
    let journal: Journal = serde_json::from_slice(&fs::read(&path)?)?;
    ensure!(
        Uuid::parse_str(&journal.id).is_ok(),
        "invalid lock transaction identity"
    );
    let directory = root.join(".naxus/transactions").join(&journal.id);
    let cargo = digest(&root.join("Cargo.lock"))?;
    let naxus = digest(&root.join("naxus.lock"))?;
    ensure!(
        (cargo.as_ref() == Some(&journal.new_cargo) || cargo == journal.old_cargo)
            && (naxus.as_ref() == Some(&journal.new_naxus) || naxus == journal.old_naxus),
        "workspace locks changed outside the pending transaction"
    );
    let both_new =
        cargo.as_ref() == Some(&journal.new_cargo) && naxus.as_ref() == Some(&journal.new_naxus);
    let both_old = cargo == journal.old_cargo && naxus == journal.old_naxus;
    if !both_new && !both_old {
        let cargo_available = cargo.as_ref() == Some(&journal.new_cargo)
            || digest(&directory.join("Cargo.lock.new"))?.as_ref() == Some(&journal.new_cargo);
        let naxus_available = naxus.as_ref() == Some(&journal.new_naxus)
            || digest(&directory.join("naxus.lock.new"))?.as_ref() == Some(&journal.new_naxus);
        if cargo_available && naxus_available {
            // 已发布一个文件时优先完成同一新状态，未发布材料必须符合日志摘要。
            if cargo.as_ref() != Some(&journal.new_cargo) {
                replace(&directory.join("Cargo.lock.new"), &root.join("Cargo.lock"))?;
            }
            if naxus.as_ref() != Some(&journal.new_naxus) {
                replace(&directory.join("naxus.lock.new"), &root.join("naxus.lock"))?;
            }
        } else {
            restore_old(root, &directory, &journal)?;
        }
    }
    ensure!(
        (digest(&root.join("Cargo.lock"))?.as_ref() == Some(&journal.new_cargo)
            && digest(&root.join("naxus.lock"))?.as_ref() == Some(&journal.new_naxus))
            || (digest(&root.join("Cargo.lock"))? == journal.old_cargo
                && digest(&root.join("naxus.lock"))? == journal.old_naxus),
        "lock recovery did not reach a complete state"
    );
    finish(root, &directory, &path)
}

/// 业务作用：在恢复完成且工作区独占期间清理上次中断留下的无主操作目录。
/// 参数说明：
/// - `root`：已持有 ClientGuard 且已完成 recover_locks 的工作区根。
///
/// 返回：仅清理本地事务根内规范 UUID 的实体目录；活动日志、非预期名称或符号链接均拒绝处理。
pub fn cleanup_operations(root: &Path) -> Result<()> {
    ensure!(
        !root.join(".naxus/lock-transaction.json").try_exists()?,
        "pending lock transaction must be recovered first"
    );
    let directory = root.join(".naxus/transactions");
    if !directory.try_exists()? {
        return Ok(());
    }
    let mut operations = Vec::new();
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let name = entry.file_name();
        ensure!(
            name.to_str()
                .and_then(|name| Uuid::parse_str(name).ok())
                .is_some_and(|id| id.to_string() == name.to_string_lossy()),
            "unexpected operation directory"
        );
        ensure!(
            entry.file_type()?.is_dir(),
            "operation directory was replaced"
        );
        ensure!(operations.len() < 1024, "operation cleanup budget exceeded");
        operations.push(entry.path());
    }
    // 先核对全部路径再删除，不能在遇到未知条目之前部分清理现场。
    for operation in operations {
        fs::remove_dir_all(operation)?;
    }
    sync_directory(&directory)
}

/// 业务作用：在解析和下载校验完成后，以可恢复事务发布两个关联锁文件。
/// 参数说明：
/// - `root`：已经持有客户端锁的工作区根。
/// - `cargo`：完整 Cargo.lock 内容。
/// - `naxus`：绑定同一 Cargo.lock 摘要的 Naxus 锁内容。
///
/// 返回：两个目标和目录项都持久化才成功，中断由下次命令先恢复。
pub fn commit_locks(root: &Path, cargo: &[u8], naxus: &[u8]) -> Result<()> {
    recover_locks(root)?;
    let id = Uuid::new_v4().to_string();
    let directory = root.join(".naxus/transactions").join(&id);
    fs::create_dir_all(&directory)?;
    let journal = Journal {
        id,
        new_cargo: ArtifactDigest::of(cargo),
        new_naxus: ArtifactDigest::of(naxus),
        old_cargo: digest(&root.join("Cargo.lock"))?,
        old_naxus: digest(&root.join("naxus.lock"))?,
    };
    for (name, old) in [
        ("Cargo.lock", &journal.old_cargo),
        ("naxus.lock", &journal.old_naxus),
    ] {
        if old.is_some() {
            write_sync(
                &directory.join(format!("{name}.old")),
                &fs::read(root.join(name))?,
            )?;
        }
    }
    write_sync(&directory.join("Cargo.lock.new"), cargo)?;
    write_sync(&directory.join("naxus.lock.new"), naxus)?;
    sync_directory(&directory)?;
    sync_directory(directory.parent().context("transaction has no parent")?)?;
    let prepared = directory.join("journal.prepared");
    write_sync(&prepared, &serde_json::to_vec(&journal)?)?;
    let path = root.join(".naxus/lock-transaction.json");
    replace(&prepared, &path)?;
    // 恢复日志先持久化，随后才按固定顺序发布两个文件，不能把两次 rename 当作原子操作。
    replace(&directory.join("Cargo.lock.new"), &root.join("Cargo.lock"))?;
    replace(&directory.join("naxus.lock.new"), &root.join("naxus.lock"))?;
    ensure!(
        digest(&root.join("Cargo.lock"))?.as_ref() == Some(&journal.new_cargo)
            && digest(&root.join("naxus.lock"))?.as_ref() == Some(&journal.new_naxus),
        "lock transaction verification failed"
    );
    finish(root, &directory, &path)
}

/// 业务作用：新状态材料不足时仅使用已核验的完整旧副本回滚。
/// 参数说明：
/// - `root`：工作区根。
/// - `directory`：精确事务目录。
/// - `journal`：持久恢复记录。
///
/// 返回：完整旧状态；旧副本缺失或摘要不符时保留现场并拒绝。
fn restore_old(root: &Path, directory: &Path, journal: &Journal) -> Result<()> {
    for (name, expected) in [
        ("Cargo.lock", &journal.old_cargo),
        ("naxus.lock", &journal.old_naxus),
    ] {
        if let Some(expected) = expected {
            ensure!(
                digest(&directory.join(format!("{name}.old")))?.as_ref() == Some(expected),
                "old lock backup is missing or corrupt"
            );
        }
    }
    for (name, expected) in [
        ("Cargo.lock", &journal.old_cargo),
        ("naxus.lock", &journal.old_naxus),
    ] {
        let destination = root.join(name);
        if expected.is_some() {
            let replacement = directory.join(format!("{}.restore", Uuid::new_v4()));
            write_sync(
                &replacement,
                &fs::read(directory.join(format!("{name}.old")))?,
            )?;
            replace(&replacement, &destination)?;
        } else if destination.try_exists()? {
            fs::remove_file(&destination)?;
            sync_directory(root)?;
        }
    }
    Ok(())
}

/// 业务作用：区分不存在与不可读取的锁文件，避免把 I/O 失败当作允许覆盖的空状态。
/// 参数说明：
/// - `path`：精确锁文件路径。
///
/// 返回：存在时的内容摘要，缺失为 None，其他错误向上传播。
fn digest(path: &Path) -> Result<Option<ArtifactDigest>> {
    if !path.try_exists()? {
        return Ok(None);
    }
    Ok(Some(ArtifactDigest::of(&fs::read(path)?)))
}

/// 业务作用：写入并同步事务材料，保证日志中声明的内容已经落盘。
/// 参数说明：
/// - `path`：本事务中的精确文件。
/// - `bytes`：完整目标内容。
///
/// 返回：完整写入并同步；已有同名材料不被覆盖。
fn write_sync(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// 业务作用：原子替换单个文件并同步源、目标目录。
/// 参数说明：
/// - `source`：同文件系统的完整暂存内容。
/// - `destination`：目标锁文件或恢复日志。
///
/// 返回：替换与目录同步全部完成才成功。
fn replace(source: &Path, destination: &Path) -> Result<()> {
    fs::rename(source, destination)?;
    sync_directory(destination.parent().context("destination has no parent")?)?;
    sync_directory(source.parent().context("source has no parent")?)
}

/// 业务作用：完整状态确认后删除活动日志及本事务的精确备份目录。
/// 参数说明：
/// - `root`：工作区根。
/// - `directory`：本日志声明的 UUID 事务目录。
/// - `journal`：已完成日志。
///
/// 返回：活动恢复记录撤销并完成同步；不扫描或删除其他任务目录。
fn finish(root: &Path, directory: &Path, journal: &Path) -> Result<()> {
    ensure!(
        directory.parent() == Some(root.join(".naxus/transactions").as_path())
            && directory
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| Uuid::parse_str(name).is_ok()),
        "invalid transaction cleanup scope"
    );
    fs::remove_file(journal)?;
    sync_directory(&root.join(".naxus"))?;
    if directory.try_exists()? {
        ensure!(
            fs::symlink_metadata(directory)?.file_type().is_dir(),
            "transaction directory was replaced"
        );
        fs::remove_dir_all(directory)?;
        sync_directory(directory.parent().context("transaction has no parent")?)?;
    }
    Ok(())
}

/// 业务作用：持久化目录项，使恢复材料和文件替换在重启后仍可寻址。
/// 参数说明：
/// - `directory`：已有的本地目录。
///
/// 返回：目录同步完成，不支持相应语义时拒绝提交。
fn sync_directory(directory: &Path) -> Result<()> {
    File::open(directory)?.sync_all()?;
    Ok(())
}
