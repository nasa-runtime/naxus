use std::{fs, path::Path};

use anyhow::{ensure, Context, Result};

/// 业务作用：创建仅服务账号可访问的持久目录，并保证新目录链在发布业务状态前可恢复寻址。
/// 参数说明：
/// - `path`：可信配置或工作区中明确指定的状态目录，最终目录不能是符号链接。
///
/// 返回：目录权限及新建目录和其父目录同步完成才成功；不支持的文件系统或异常路径拒绝创建。
pub fn create_private_directory(path: &Path) -> Result<()> {
    let path = std::path::absolute(path)?;
    let mut missing = Vec::new();
    let mut current = path.as_path();
    while !current.try_exists()? {
        missing.push(current.to_path_buf());
        current = current
            .parent()
            .context("state directory has no existing ancestor")?;
    }
    validate_local_filesystem(&current.canonicalize()?)?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&path)?;
    ensure!(
        fs::symlink_metadata(&path)?.file_type().is_dir(),
        "state directory must not be a symlink"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    }
    fs::File::open(&path)?.sync_all()?;
    // 只同步叶目录不能持久化父目录中的新名称；先落盘整条目录链，再允许写入恢复日志或身份。
    for directory in missing {
        fs::File::open(&directory)?.sync_all()?;
        fs::File::open(
            directory
                .parent()
                .context("state directory has no parent")?,
        )?
        .sync_all()?;
    }
    Ok(())
}

/// 业务作用：在持久写入前限制本地文件系统类型，避免在不满足独占与持久化合同的挂载上受理写入。
/// 参数说明：
/// - `root`：已经存在且规范化的绝对数据根。
///
/// 返回：支持的本地存储类型返回成功；未知、网络、内存及宿主共享文件系统拒绝打开。
pub fn validate_local_filesystem(root: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::{ffi::CString, mem::MaybeUninit, os::unix::ffi::OsStrExt};

        let path = CString::new(root.as_os_str().as_bytes())?;
        let mut information = MaybeUninit::<libc::statfs>::zeroed();
        let status = unsafe {
            // C 字符串与独占输出缓冲在整个系统调用期间保持有效。
            libc::statfs(path.as_ptr(), information.as_mut_ptr())
        };
        if status != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let information = unsafe {
            // 内核已确认完整写入，错误返回路径不会读取未初始化字段。
            information.assume_init()
        };
        let magic = information.f_type as u64;
        // 明确的本地类型才能承担 WAL、跨进程锁和目录同步；未知类型不能按“调用成功”推断安全。
        anyhow::ensure!(
            matches!(magic, 0xef53 | 0x5846_5342 | 0x9123_683e | 0xf2f5_2010 | 0x794c_7630),
            "unsupported repository filesystem {magic:#x}; use a local ext4, XFS, Btrfs, F2FS, overlayfs or Docker named volume"
        );
    }
    #[cfg(target_os = "macos")]
    {
        use std::{ffi::CString, mem::MaybeUninit, os::unix::ffi::OsStrExt};

        let path = CString::new(root.as_os_str().as_bytes())?;
        let mut information = MaybeUninit::<libc::statfs>::zeroed();
        let status = unsafe {
            // 路径与输出缓冲属于当前栈帧，调用期间没有别名写入或提前释放。
            libc::statfs(path.as_ptr(), information.as_mut_ptr())
        };
        if status != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let information = unsafe {
            // 只在系统调用成功后读取完整结构体，类型名称读取另受固定数组边界限制。
            information.assume_init()
        };
        let name = information
            .f_fstypename
            .iter()
            .take_while(|byte| **byte != 0)
            .map(|byte| *byte as u8)
            .collect::<Vec<_>>();
        anyhow::ensure!(
            matches!(name.as_slice(), b"apfs" | b"hfs"),
            "unsupported repository filesystem; use local APFS or HFS+"
        );
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = root;
        anyhow::bail!("unsupported repository storage platform")
    }
}
