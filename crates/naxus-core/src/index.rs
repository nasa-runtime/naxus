use std::collections::BTreeMap;

use anyhow::{ensure, Result};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

use crate::{validate_name, ArtifactDigest};

/// Cargo 已知的实验依赖字段；保留类型信息，防止未支持的语义被当作普通依赖。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DependencyExtensions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bindep_target: Option<String>,
    #[serde(default, skip_serializing_if = "disabled")]
    pub lib: bool,
}

impl DependencyExtensions {
    /// 业务作用：在索引、归档及发布帧边界拒绝当前不能完整表达的实验依赖语义。
    /// 参数说明: 无。
    ///
    /// 返回：仅普通依赖的中性字段通过；artifact、artifact target、库混合构建或公开依赖声明明确拒绝。
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.public != Some(true)
                && self.artifact.is_none()
                && self.bindep_target.is_none()
                && !self.lib,
            "artifact and public dependencies are not supported"
        );
        Ok(())
    }
}

/// 业务作用：省略普通依赖的默认 lib 标记，保持已有索引元数据的序列化身份。
/// 参数说明：
/// - `value`：是否要求同时构建 artifact 对应的库。
///
/// 返回：未启用该语义时不写入索引字段。
fn disabled(value: &bool) -> bool {
    !value
}

/// Cargo sparse index 中一个依赖声明，保留别名、条件和逻辑来源。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IndexDependency {
    pub name: String,
    pub req: String,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub optional: bool,
    #[serde(default = "enabled")]
    pub default_features: bool,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub registry: Option<String>,
    #[serde(default)]
    pub package: Option<String>,
    #[serde(flatten)]
    pub extensions: DependencyExtensions,
}

/// 业务作用：保持旧式 Cargo 索引省略 default_features 时的默认启用语义。
/// 参数说明: 无。
///
/// 返回：默认启用依赖自身的默认 feature。
fn enabled() -> bool {
    true
}

/// 一个名称和 SemVer 对应的 Cargo 索引记录，实验依赖语义受显式支持边界约束。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub name: String,
    pub vers: String,
    pub deps: Vec<IndexDependency>,
    pub cksum: ArtifactDigest,
    #[serde(default)]
    pub features: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub features2: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub yanked: bool,
    #[serde(default)]
    pub links: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rust_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v: Option<u32>,
}

impl IndexEntry {
    /// 业务作用：在制品进入仓库快照前校验坐标和 Cargo 依赖声明的基础合同。
    /// 参数说明: 无。
    ///
    /// 返回：坐标及依赖结构可解析时成功；不执行归档中的任意代码。
    pub fn validate(&self) -> Result<()> {
        self.validate_upstream()?;
        let version = Version::parse(&self.vers)?;
        ensure!(
            version.build.is_empty(),
            "build metadata cannot distinguish crate versions"
        );
        Ok(())
    }

    /// 业务作用：校验公开索引的 Cargo 语义，保留上游合法的 SemVer build metadata。
    /// 参数说明: 无。
    ///
    /// 返回：坐标、MSRV、schema、普通依赖声明、target 语法及本 crate 的 feature 图合法才成功；拒绝 artifact 和公开依赖语义，不裁决目标匹配和依赖包的实际 feature 集合。
    pub fn validate_upstream(&self) -> Result<()> {
        validate_name(&self.name)?;
        Version::parse(&self.vers)?;
        if let Some(msrv) = &self.rust_version {
            let parts = msrv.trim().split('.').collect::<Vec<_>>();
            ensure!(
                (1..=3).contains(&parts.len())
                    && parts
                        .iter()
                        .all(|part| !part.is_empty()
                            && part.bytes().all(|byte| byte.is_ascii_digit())),
                "invalid rust-version declaration"
            );
            let mut normalized = parts.join(".");
            for _ in parts.len()..3 {
                normalized.push_str(".0");
            }
            Version::parse(&normalized)
                .map_err(|_| anyhow::anyhow!("invalid rust-version declaration"))?;
        }
        ensure!(
            self.v.is_none_or(|value| value == 1 || value == 2),
            "unsupported index schema"
        );
        ensure!(
            self.features2.is_empty() || self.v == Some(2),
            "features2 requires index schema 2"
        );
        for dependency in &self.deps {
            dependency.extensions.validate()?;
            validate_name(&dependency.name)?;
            if let Some(package) = &dependency.package {
                validate_name(package)?;
            }
            VersionReq::parse(&dependency.req)?;
            if let Some(target) = &dependency.target {
                // 平台条件的语法在入库前即可确定；只解析，不按仓库主机筛掉其他业务目标的依赖。
                crate::platform::validate(target)?;
            }
            ensure!(
                dependency
                    .kind
                    .as_deref()
                    .is_none_or(|kind| matches!(kind, "normal" | "build" | "dev")),
                "unsupported dependency kind"
            );
            if let Some(registry) = &dependency.registry {
                crate::registry_identity(registry)?;
            }
        }
        crate::features::validate(self)
    }

    /// 业务作用：计算索引元数据指纹，使同字节包体的 feature 或来源变化仍可被识别。
    /// 参数说明: 无。
    ///
    /// 返回：确定性序列化后的摘要；序列化失败时不产生可提交指纹。
    pub fn metadata_digest(&self) -> Result<ArtifactDigest> {
        Ok(ArtifactDigest::of(&serde_json::to_vec(self)?))
    }
}

/// 指向不可变对象和索引元数据的发布记录。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Revision {
    pub id: String,
    pub repository_id: String,
    pub entry: IndexEntry,
    pub metadata_sha256: ArtifactDigest,
    pub actor: String,
}

/// 某次仓库提交的稳定读取入口；父快照与变更集在存储层保持不可变。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub repository_id: String,
    pub parent_id: Option<String>,
    pub manifest_sha256: ArtifactDigest,
}
