use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader, Read},
};

use anyhow::{ensure, Context, Result};
use flate2::bufread::GzDecoder;
use naxus_core::{ArtifactDigest, IndexEntry};

/// 制品静态检查预算，涵盖解压总量、文件数和长路径扩展。
#[derive(Clone, Debug)]
pub struct ArchiveLimits {
    pub expanded_bytes: u64,
    pub files: usize,
    pub manifest_bytes: u64,
    pub path_bytes: u64,
}

impl Default for ArchiveLimits {
    /// 业务作用：为上传静态检查提供有界的默认解压与元数据预算。
    /// 参数说明: 无。
    ///
    /// 返回：512 MiB 解压预算、十万条目、1 MiB manifest 和 4096 字节路径上限。
    fn default() -> Self {
        Self {
            expanded_bytes: 512 * 1024 * 1024,
            files: 100_000,
            manifest_bytes: 1024 * 1024,
            path_bytes: 4096,
        }
    }
}

/// 业务作用：检查上传归档的结构和规范化 manifest，生成该精确对象的 Cargo 索引记录。
/// 参数说明：
/// - `reader`：已经验证原始 SHA-256 的完整 gzip 归档。
/// - `name`：上传地址声明的实际包名。
/// - `version`：上传地址声明的精确版本。
/// - `digest`：原始 `.crate` 的内容摘要。
/// - `logical_index`：本仓库稳定逻辑 registry URL，用于识别同仓依赖。
/// - `limits`：展开、条目和元数据预算。
///
/// 返回：所有结构与来源检查成功后的索引；不落盘展开内容，不执行构建脚本或宏。
pub fn inspect_archive(
    reader: &mut dyn Read,
    name: &str,
    version: &str,
    digest: ArtifactDigest,
    logical_index: &str,
    limits: &ArchiveLimits,
) -> Result<IndexEntry> {
    naxus_core::validate_name(name)?;
    ensure!(
        limits.expanded_bytes > 0 && limits.path_bytes > 0 && limits.files > 0,
        "invalid archive limits"
    );
    let decoder = GzDecoder::new(BufReader::new(reader));
    let mut archive = tar::Archive::new(decoder.take(limits.expanded_bytes));
    let prefix = format!("{name}-{version}");
    let mut paths = BTreeSet::new();
    let mut files = BTreeSet::new();
    let mut pending_name = None;
    let mut manifest = None;
    for (index, item) in archive.entries()?.raw(true).enumerate() {
        ensure!(index < limits.files, "archive contains too many entries");
        let mut item = item?;
        let kind = item.header().entry_type();
        if kind.is_gnu_longname() {
            // 先限制扩展长度再分配，避免长名称头绕过整体流式读取预算。
            ensure!(
                pending_name.is_none() && item.size() <= limits.path_bytes + 1,
                "archive long-name budget exceeded"
            );
            let mut path = Vec::new();
            item.read_to_end(&mut path)?;
            ensure!(
                path.pop() == Some(0) && !path.contains(&0),
                "invalid long-name extension"
            );
            pending_name = Some(path);
            continue;
        }
        ensure!(
            kind.is_file() || kind.is_dir(),
            "archive links and special entries are not allowed"
        );
        let raw = pending_name
            .take()
            .unwrap_or_else(|| item.path_bytes().into_owned());
        ensure!(
            raw.len() as u64 <= limits.path_bytes,
            "archive path budget exceeded"
        );
        let path = std::str::from_utf8(&raw)?;
        ensure!(
            !path.contains('\\') && !path.contains('\0'),
            "archive path uses ambiguous separators"
        );
        let path = if kind.is_dir() {
            path.trim_end_matches('/')
        } else {
            path
        };
        let parts = path.split('/').collect::<Vec<_>>();
        ensure!(
            parts.first().copied() == Some(prefix.as_str()),
            "archive root differs from package coordinate"
        );
        ensure!(
            parts.iter().all(|part| !part.is_empty()
                && !matches!(*part, "." | "..")
                && !part.contains(':')),
            "archive path escapes package root"
        );
        let key = path.to_lowercase();
        ensure!(
            paths.insert(key.clone()),
            "archive contains duplicate or case-conflicting paths"
        );
        let mut parent = String::new();
        for part in &parts[..parts.len().saturating_sub(1)] {
            if !parent.is_empty() {
                parent.push('/');
            }
            parent.push_str(&part.to_lowercase());
            ensure!(
                !files.contains(&parent),
                "archive file is used as a directory"
            );
        }
        if kind.is_file() {
            let child_prefix = format!("{key}/");
            ensure!(
                !paths
                    .range(child_prefix.clone()..)
                    .next()
                    .is_some_and(|candidate| candidate.starts_with(&child_prefix)),
                "archive file shadows an existing directory"
            );
            files.insert(key);
        }
        if path == format!("{prefix}/Cargo.toml") {
            ensure!(
                kind.is_file() && item.size() <= limits.manifest_bytes,
                "invalid package manifest entry"
            );
            let mut bytes = Vec::new();
            item.read_to_end(&mut bytes)?;
            manifest = Some(String::from_utf8(bytes)?);
        } else {
            std::io::copy(&mut item, &mut std::io::sink())?;
        }
    }
    ensure!(pending_name.is_none(), "orphan archive long-name extension");
    let mut remaining = archive.into_inner();
    let mut tail = [0_u8; 8192];
    loop {
        let count = remaining.read(&mut tail)?;
        if count == 0 {
            break;
        }
        ensure!(
            tail[..count].iter().all(|byte| *byte == 0),
            "archive contains trailing non-padding data"
        );
    }
    ensure!(
        remaining.limit() > 0,
        "archive expanded size budget exceeded"
    );
    let mut compressed = remaining.into_inner().into_inner();
    ensure!(
        compressed.fill_buf()?.is_empty(),
        "archive contains trailing gzip members or data"
    );
    crate::manifest::index_from_manifest(
        &manifest.context("archive has no Cargo.toml")?,
        name,
        version,
        digest,
        logical_index,
    )
}
