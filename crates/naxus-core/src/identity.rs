use std::{fmt, str::FromStr};

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 同一名称和版本再次发布时的内容策略。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepositoryPolicy {
    Mutable,
    Immutable,
}

/// 已校验的二进制对象 SHA-256；表示字节身份，不表示发布主体可信。
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ArtifactDigest(String);

impl ArtifactDigest {
    /// 业务作用：对已取得的字节计算稳定的内容身份。
    /// 参数说明：
    /// - `bytes`：未经文本转换的对象或规范化元数据。
    ///
    /// 返回：小写十六进制 SHA-256，不对输入来源作信任背书。
    pub fn of(bytes: &[u8]) -> Self {
        Self(hex::encode(Sha256::digest(bytes)))
    }

    /// 业务作用：向存储与协议层提供经过校验的摘要文本。
    /// 参数说明: 无。
    ///
    /// 返回：恰好 64 个小写十六进制字符。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ArtifactDigest {
    type Err = anyhow::Error;

    /// 业务作用：在进入对象寻址前拒绝非摘要输入，避免路径字符越过存储边界。
    /// 参数说明：
    /// - `value`：索引或管理请求携带的摘要。
    ///
    /// 返回：仅接受 64 个小写十六进制字符，其余输入返回错误。
    fn from_str(value: &str) -> Result<Self> {
        ensure!(
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "invalid SHA-256 identity"
        );
        Ok(Self(value.to_owned()))
    }
}

impl TryFrom<String> for ArtifactDigest {
    type Error = anyhow::Error;

    /// 业务作用：让反序列化与显式解析遵守同一个摘要校验合同。
    /// 参数说明：
    /// - `value`：待校验的摘要文本。
    ///
    /// 返回：合法对象身份；不接受路径、大小写混用或截断摘要。
    fn try_from(value: String) -> Result<Self> {
        value.parse()
    }
}

impl From<ArtifactDigest> for String {
    /// 业务作用：在持久化或协议输出时保留规范化摘要。
    /// 参数说明：
    /// - `value`：已经校验的摘要。
    ///
    /// 返回：原有小写十六进制文本。
    fn from(value: ArtifactDigest) -> Self {
        value.0
    }
}

impl fmt::Display for ArtifactDigest {
    /// 业务作用：在索引和对象地址中输出确定性的内容身份。
    /// 参数说明：
    /// - `formatter`：接收规范化摘要的格式化目标。
    ///
    /// 返回：格式化写入结果，不产生额外标记。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// 业务作用：拒绝不能安全作为 Cargo 坐标和索引路径使用的包名。
/// 参数说明：
/// - `name`：保留原始大小写的 Cargo package 名称。
///
/// 返回：名称合法时成功；拒绝非 ASCII、路径字符和保留文件名。
pub fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.len() <= 64,
        "invalid crate name length"
    );
    ensure!(
        name.as_bytes()[0].is_ascii_alphabetic(),
        "crate name must start with a letter"
    );
    ensure!(
        name.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
        "invalid crate name character"
    );
    let lower = name.to_ascii_lowercase();
    let reserved = ["con", "prn", "aux", "nul", "clock$"];
    ensure!(!reserved.contains(&lower.as_str()), "reserved crate name");
    for prefix in ["com", "lpt"] {
        ensure!(
            !(lower.len() == 4
                && lower.starts_with(prefix)
                && matches!(lower.as_bytes()[3], b'1'..=b'9')),
            "reserved crate name"
        );
    }
    Ok(())
}

/// 业务作用：为包名所有权和大小写、连接符冲突提供唯一比较键。
/// 参数说明：
/// - `name`：已经通过包名校验的 package 名称。
///
/// 返回：小写并将下划线折叠为连接符的名称；不改写公开索引中的原名。
pub fn canonical_name(name: &str) -> String {
    name.to_ascii_lowercase().replace('_', "-")
}
