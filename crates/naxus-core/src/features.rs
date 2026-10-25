use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Result};

use crate::IndexEntry;

/// 业务作用：在索引可见前核对本 crate 的 feature 引用与可选依赖，避免无效上游替换有效缓存。
/// 参数说明：
/// - `entry`：已经核对 schema 和依赖声明的完整索引条目。
///
/// 返回：普通引用、显式依赖、强弱依赖 feature 引用及隐式 feature 一致时成功；依赖包的实际 feature 由 Cargo 解析。
pub(crate) fn validate(entry: &IndexEntry) -> Result<()> {
    let mut dependencies = BTreeMap::new();
    for dependency in &entry.deps {
        ensure!(
            !dependency.optional || dependency.kind.as_deref() != Some("dev"),
            "dev dependencies cannot be optional"
        );
        *dependencies
            .entry(dependency.name.as_str())
            .or_insert(false) |= dependency.optional;
    }
    let mut features: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (name, members) in entry.features.iter().chain(&entry.features2) {
        validate_name(name)?;
        // schema 2 的同名 feature 合并成员，不能由后一个字段覆盖前一个字段的约束。
        features
            .entry(name)
            .or_default()
            .extend(members.iter().map(String::as_str));
    }
    let hidden = features
        .values()
        .flatten()
        .filter(|member| !member.contains('/'))
        .filter_map(|member| member.strip_prefix("dep:"))
        .collect::<BTreeSet<_>>();
    let implicit = dependencies
        .iter()
        .filter(|(name, optional)| {
            **optional && !features.contains_key(**name) && !hidden.contains(**name)
        })
        .map(|(name, _)| *name)
        .collect::<BTreeSet<_>>();
    let mut used = implicit.clone();
    for members in features.values() {
        for member in members {
            if let Some((dependency, feature)) = member.split_once('/') {
                let weak = dependency.ends_with('?');
                let dependency = dependency.strip_suffix('?').unwrap_or(dependency);
                validate_name(feature)?;
                ensure!(
                    !dependency.starts_with("dep:")
                        && dependencies.contains_key(dependency)
                        && (!weak || dependencies[dependency]),
                    "invalid dependency feature reference"
                );
                used.insert(dependency);
            } else if let Some(dependency) = member.strip_prefix("dep:") {
                ensure!(
                    dependencies.get(dependency) == Some(&true),
                    "explicit feature dependency must be optional"
                );
                used.insert(dependency);
            } else {
                ensure!(
                    features.contains_key(member) || implicit.contains(member),
                    "feature references an undefined feature or unavailable implicit dependency"
                );
            }
        }
    }
    // 显式同名 feature 会隐藏隐式依赖开关；没有任何实际依赖引用时不能宣称可选依赖可被启用。
    ensure!(
        dependencies
            .iter()
            .all(|(name, optional)| !optional || used.contains(name)),
        "optional dependency is not included in any feature"
    );
    Ok(())
}

/// 业务作用：按 Cargo 的 Unicode feature 名称规则检查图节点及依赖 feature 的语法。
/// 参数说明：
/// - `name`：feature 名称，不包含依赖语法前缀或斜线。
///
/// 返回：非空、首字符及后续字符均符合 Cargo 语法时成功；不额外套用 crates.io 的 ASCII 发布策略。
fn validate_name(name: &str) -> Result<()> {
    let mut characters = name.chars();
    ensure!(
        characters
            .next()
            .is_some_and(|character| unicode_ident::is_xid_start(character)
                || character == '_'
                || character.is_ascii_digit())
            && characters.all(|character| unicode_ident::is_xid_continue(character)
                || matches!(character, '-' | '+' | '.')),
        "invalid Cargo feature name"
    );
    Ok(())
}
