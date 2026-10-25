use std::collections::BTreeMap;

use anyhow::{ensure, Context, Result};
use naxus_core::{ArtifactDigest, IndexDependency, IndexEntry};
use toml::Value;

pub const CRATES_IO_INDEX: &str = "https://github.com/rust-lang/crates.io-index";

/// 业务作用：把 Cargo 规范化 manifest 映射为索引，保留私有与公开来源边界。
/// 参数说明：
/// - `text`：归档内唯一规范化 manifest。
/// - `name`：上传坐标中的真实包名。
/// - `version`：上传坐标中的精确版本。
/// - `digest`：已验证的原始包体摘要。
/// - `logical_index`：目标仓库稳定逻辑索引。
///
/// 返回：与归档一致的完整索引；工作区继承、路径、Git 或来源歧义均拒绝。
pub(crate) fn index_from_manifest(
    text: &str,
    name: &str,
    version: &str,
    digest: ArtifactDigest,
    logical_index: &str,
) -> Result<IndexEntry> {
    let manifest: Value = toml::from_str(text)?;
    let package = manifest
        .get("package")
        .and_then(Value::as_table)
        .context("normalized manifest has no package table")?;
    ensure!(
        package.get("name").and_then(Value::as_str) == Some(name)
            && package.get("version").and_then(Value::as_str) == Some(version),
        "manifest coordinate differs from upload coordinate"
    );
    ensure!(
        manifest.get("workspace").is_none()
            && manifest.get("patch").is_none()
            && manifest.get("replace").is_none(),
        "manifest is not normalized for publication"
    );
    let mut dependencies = Vec::new();
    read_dependencies(&manifest, None, logical_index, &mut dependencies)?;
    if let Some(targets) = manifest.get("target") {
        for (target, table) in targets
            .as_table()
            .context("invalid target dependency table")?
        {
            read_dependencies(table, Some(target), logical_index, &mut dependencies)?;
        }
    }
    dependencies.sort_by(|left, right| {
        (&left.name, &left.kind, &left.target).cmp(&(&right.name, &right.kind, &right.target))
    });
    let features: BTreeMap<String, Vec<String>> = match manifest.get("features") {
        Some(features) => features.clone().try_into()?,
        None => BTreeMap::new(),
    };
    let extended = features
        .values()
        .flatten()
        .any(|value| value.starts_with("dep:") || value.contains("?/"));
    let (features, features2) = if extended {
        (BTreeMap::new(), features)
    } else {
        (features, BTreeMap::new())
    };
    let entry = IndexEntry {
        name: name.to_owned(),
        vers: version.to_owned(),
        deps: dependencies,
        cksum: digest,
        features,
        features2,
        yanked: false,
        links: package
            .get("links")
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .context("invalid links declaration")
            })
            .transpose()?,
        rust_version: package
            .get("rust-version")
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .context("invalid rust-version declaration")
            })
            .transpose()?,
        v: Some(2),
    };
    entry.validate()?;
    Ok(entry)
}

/// 业务作用：逐项转换普通、构建、开发和 target 依赖，不把默认公开依赖误判为同仓依赖。
/// 参数说明：
/// - `table`：manifest 根或某个 target 表。
/// - `target`：需要保留的 target 条件。
/// - `logical_index`：当前 hosted 仓库稳定来源。
/// - `output`：本制品完整依赖列表。
///
/// 返回：全部声明规范且来源明确时追加索引依赖；任一歧义拒绝整个制品。
fn read_dependencies(
    table: &Value,
    target: Option<&str>,
    logical_index: &str,
    output: &mut Vec<IndexDependency>,
) -> Result<()> {
    for (section, kind) in [
        ("dependencies", "normal"),
        ("build-dependencies", "build"),
        ("dev-dependencies", "dev"),
    ] {
        let Some(values) = table.get(section) else {
            continue;
        };
        for (alias, value) in values.as_table().context("invalid dependency table")? {
            let mut dependency = IndexDependency {
                name: alias.to_owned(),
                req: String::new(),
                features: Vec::new(),
                optional: false,
                default_features: true,
                target: target.map(str::to_owned),
                kind: Some(kind.to_owned()),
                registry: Some(CRATES_IO_INDEX.to_owned()),
                package: None,
                extensions: Default::default(),
            };
            if let Some(requirement) = value.as_str() {
                dependency.req = requirement.to_owned();
            } else {
                let value = value.as_table().context("invalid dependency declaration")?;
                dependency.extensions = Value::Table(value.clone()).try_into()?;
                dependency.extensions.validate()?;
                // 此处 target 属于 artifact 构建目标，不是外层 target 表的平台条件，不能丢弃后降为普通依赖。
                ensure!(
                    !value.contains_key("target"),
                    "artifact dependency target is not supported"
                );
                for key in [
                    "path",
                    "git",
                    "branch",
                    "tag",
                    "rev",
                    "workspace",
                    "registry",
                ] {
                    ensure!(
                        !value.contains_key(key),
                        "dependency contains non-normalized source field: {key}"
                    );
                }
                dependency.req = value
                    .get("version")
                    .and_then(Value::as_str)
                    .context("dependency has no version requirement")?
                    .to_owned();
                dependency.package = value
                    .get("package")
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .context("invalid dependency package name")
                    })
                    .transpose()?;
                dependency.features = value
                    .get("features")
                    .map(|value| value.clone().try_into())
                    .transpose()?
                    .unwrap_or_default();
                dependency.optional = value
                    .get("optional")
                    .map(|value| value.as_bool().context("invalid optional dependency flag"))
                    .transpose()?
                    .unwrap_or(false);
                dependency.default_features = value
                    .get("default-features")
                    .map(|value| value.as_bool().context("invalid default-features flag"))
                    .transpose()?
                    .unwrap_or(true);
                if let Some(registry) = value.get("registry-index") {
                    let registry = registry.as_str().context("invalid registry-index")?;
                    let identity = naxus_core::registry_identity(registry)?;
                    dependency.registry =
                        if identity == naxus_core::registry_identity(logical_index)? {
                            None
                        } else if identity == CRATES_IO_INDEX {
                            Some(identity)
                        } else {
                            Some(registry.to_owned())
                        };
                }
            }
            output.push(dependency);
        }
    }
    Ok(())
}
