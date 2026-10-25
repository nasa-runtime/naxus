use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use anyhow::{ensure, Context, Result};

static OPEN_ROOTS: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();

/// 同时持有进程内路径门禁与操作系统文件锁的数据根所有者。
pub(crate) struct InstanceLock {
    root: PathBuf,
    file: File,
}

impl InstanceLock {
    /// 业务作用：在数据库迁移和对象目录变更前拒绝已有所有者的数据根。
    /// 参数说明：
    /// - `root`：已经规范化的绝对数据根。
    ///
    /// 返回：同时取得进程内与跨进程独占权的守卫；任一门禁冲突时不开放数据卷。
    pub(crate) fn acquire(root: &Path) -> Result<Self> {
        let mut roots = OPEN_ROOTS
            .get_or_init(|| Mutex::new(BTreeSet::new()))
            .lock()
            .map_err(|_| anyhow::anyhow!("repository ownership registry is unavailable"))?;
        // 部分文件系统按进程合并锁所有者，文件锁不能代替同进程的独立实例门禁。
        ensure!(
            !roots.contains(root),
            "repository directory is already in use"
        );
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("instance.lock"))?;
        fs2::FileExt::try_lock_exclusive(&file)
            .context("repository directory is already in use")?;
        roots.insert(root.to_owned());
        Ok(Self {
            root: root.to_owned(),
            file,
        })
    }
}

impl Drop for InstanceLock {
    /// 业务作用：在正常关闭或初始化失败时释放数据根所有权，允许后续实例重新打开。
    /// 参数说明: 无。
    ///
    /// 返回：关闭文件锁并移除进程内登记；登记锁不可用时保持保守拒绝状态。
    fn drop(&mut self) {
        if let Some(roots) = OPEN_ROOTS.get() {
            if let Ok(mut roots) = roots.lock() {
                // 先解除文件锁再放开本进程入口，避免新实例遇到本守卫尚未释放的锁。
                let _ = fs2::FileExt::unlock(&self.file);
                roots.remove(&self.root);
            }
        }
    }
}
