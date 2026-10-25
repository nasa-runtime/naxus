use std::path::Path;

use anyhow::Result;

/// 业务作用：在打开数据库前限制数据卷类型，避免在不满足独占与持久化合同的挂载上受理写入。
/// 参数说明：
/// - `root`：已经存在且规范化的绝对数据根。
///
/// 返回：支持的本地存储类型返回成功；未知、网络、内存及宿主共享文件系统拒绝打开。
pub(crate) fn validate_filesystem(root: &Path) -> Result<()> {
    naxus_core::validate_local_filesystem(root)
}
