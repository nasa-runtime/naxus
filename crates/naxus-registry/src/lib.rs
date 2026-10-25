#![doc = include_str!("../README.md")]

mod archive;
mod manifest;
mod publish;

pub use archive::{inspect_archive, ArchiveLimits};
pub use manifest::CRATES_IO_INDEX;
pub use publish::PublishMetadata;

use anyhow::Result;
use naxus_core::validate_name;

/// 业务作用：按 Cargo sparse 规则计算包名的唯一索引路径。
/// 参数说明：
/// - `name`：实际 package 名称。
///
/// 返回：小写 sparse 相对路径；非法名称不进入路由寻址。
pub fn index_path(name: &str) -> Result<String> {
    validate_name(name)?;
    let name = name.to_ascii_lowercase();
    Ok(match name.len() {
        1 => format!("1/{name}"),
        2 => format!("2/{name}"),
        3 => format!("3/{}/{name}", &name[..1]),
        _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
    })
}
