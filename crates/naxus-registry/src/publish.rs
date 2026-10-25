use std::collections::BTreeMap;

use anyhow::{ensure, Context, Result};
use naxus_core::{DependencyExtensions, IndexDependency, IndexEntry};
use serde::Deserialize;

/// Cargo 发布 API 的元数据，与归档规范化 manifest 分别验证后才允许提交。
#[derive(Deserialize)]
pub struct PublishMetadata {
    pub name: String,
    pub vers: String,
    #[serde(default)]
    deps: Vec<PublishDependency>,
    #[serde(default)]
    features: BTreeMap<String, Vec<String>>,
    links: Option<String>,
    rust_version: Option<String>,
}

#[derive(Deserialize)]
struct PublishDependency {
    name: String,
    version_req: String,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    optional: bool,
    default_features: bool,
    target: Option<String>,
    kind: Option<String>,
    registry: Option<String>,
    explicit_name_in_toml: Option<String>,
    #[serde(flatten)]
    extensions: DependencyExtensions,
}

impl PublishMetadata {
    /// 业务作用：核对原生发布 JSON 与归档语义，禁止外部元数据替换归档中的依赖来源或 feature。
    /// 参数说明：
    /// - `entry`：静态读取归档得到的权威索引。
    /// - `logical_indexes`：当前仓库名称与 UUID 的等价来源。
    ///
    /// 返回：名称、版本、依赖、feature、links 与 MSRV 全部一致才成功；不采纳未经归档证明的字段。
    pub fn validate_archive(&self, entry: &IndexEntry, logical_indexes: &[String]) -> Result<()> {
        ensure!(
            self.name == entry.name && self.vers == entry.vers,
            "publish metadata coordinate differs from archive"
        );
        ensure!(
            self.links == entry.links && self.rust_version == entry.rust_version,
            "publish metadata links or rust-version differs from archive"
        );
        let mut features = entry.features.clone();
        features.extend(entry.features2.clone());
        ensure!(
            normalized_features(features) == normalized_features(self.features.clone()),
            "publish metadata features differ from archive"
        );
        let mut declared = Vec::new();
        for dependency in &self.deps {
            dependency.extensions.validate()?;
            let alias = dependency
                .explicit_name_in_toml
                .as_ref()
                .filter(|alias| **alias != dependency.name);
            declared.push(IndexDependency {
                name: alias.unwrap_or(&dependency.name).clone(),
                req: dependency.version_req.clone(),
                features: dependency.features.clone(),
                optional: dependency.optional,
                default_features: dependency.default_features,
                target: dependency.target.clone(),
                kind: dependency.kind.clone(),
                registry: dependency.registry.clone(),
                package: alias.map(|_| dependency.name.clone()),
                extensions: dependency.extensions.clone(),
            });
        }
        ensure!(
            normalized_dependencies(declared, logical_indexes)?
                == normalized_dependencies(entry.deps.clone(), logical_indexes)?,
            "publish metadata dependencies differ from archive"
        );
        Ok(())
    }
}

/// 业务作用：按集合语义比较 feature 成员，保持别名及依赖语法原文。
/// 参数说明：
/// - `features`：发布或归档中的完整 feature 表。
///
/// 返回：每项成员已排序的表，不增删业务成员。
fn normalized_features(
    mut features: BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, Vec<String>> {
    for values in features.values_mut() {
        values.sort();
    }
    features
}

/// 业务作用：比较发布帧与归档的依赖语义，允许等价的 SemVer 表示和当前仓库别名。
/// 参数说明：
/// - `dependencies`：需要比较的独立依赖声明。
/// - `logical_indexes`：同一 hosted 仓库的等价入口。
///
/// 返回：排序后的确定性依赖描述，非法版本要求拒绝。
fn normalized_dependencies(
    mut dependencies: Vec<IndexDependency>,
    logical_indexes: &[String],
) -> Result<Vec<String>> {
    let logical = logical_indexes
        .iter()
        .map(|index| naxus_core::registry_identity(index))
        .collect::<Result<std::collections::BTreeSet<_>>>()?;
    let mut normalized = Vec::new();
    for dependency in &mut dependencies {
        dependency.extensions.validate()?;
        // public=false 和省略字段具有同一普通依赖语义，不因 Cargo 是否输出默认值而拒绝匹配。
        dependency.extensions = DependencyExtensions::default();
        dependency.req = semver::VersionReq::parse(&dependency.req)?.to_string();
        dependency.features.sort();
        if dependency.kind.is_none() {
            dependency.kind = Some("normal".to_owned());
        }
        if let Some(registry) = &dependency.registry {
            let identity = naxus_core::registry_identity(registry)?;
            if logical.contains(&identity) {
                dependency.registry = None;
            } else {
                dependency.registry = Some(identity);
            }
        }
        normalized
            .push(serde_json::to_string(dependency).context("cannot compare publish dependency")?);
    }
    normalized.sort();
    Ok(normalized)
}
