use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{Revision, Snapshot};

/// 业务锁中已选择或请求核对的精确坐标。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Coordinate {
    pub name: String,
    pub version: String,
}

/// 固定快照下的坐标核对结果；缺失必须显式表达，不能回退来源。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedPackage {
    pub coordinate: Coordinate,
    pub revision: Option<Revision>,
}

/// 服务器在同一事务观察到的快照和有界保留租约。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Resolution {
    pub snapshot_set_id: String,
    pub snapshot: Snapshot,
    pub packages: Vec<ResolvedPackage>,
    pub lease_id: String,
    pub lease_expires_at: u64,
}

/// 服务端续期时从同一持久租约读回的归属与保护对象。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LeaseProof {
    pub id: String,
    pub repository_id: String,
    pub snapshot_id: String,
    pub snapshot_set_id: String,
    pub expires_at: u64,
}

impl Resolution {
    /// 业务作用：把解析响应绑定到请求仓库、固定快照和完整坐标多重集合，拒绝等长替换与伪造元数据。
    /// 参数说明：
    /// - `repository`：已经通过服务目录固定的仓库 UUID。
    /// - `requested`：实际提交的精确坐标，可包含重复项。
    /// - `fixed`：已知不可变快照；首次读取 HEAD 时为 None。
    ///
    /// 返回：所有响应身份及摘要一致才成功，不创建或续期任何租约。
    pub fn validate(
        &self,
        repository: &str,
        requested: &[Coordinate],
        fixed: Option<&Snapshot>,
    ) -> Result<()> {
        for id in [
            repository,
            &self.snapshot.id,
            &self.snapshot.repository_id,
            &self.snapshot_set_id,
            &self.lease_id,
        ] {
            uuid::Uuid::parse_str(id)?;
        }
        if let Some(parent) = &self.snapshot.parent_id {
            uuid::Uuid::parse_str(parent)?;
            ensure!(parent != &self.snapshot.id, "snapshot is its own parent");
        }
        ensure!(
            self.snapshot.repository_id == repository && self.lease_expires_at > 0,
            "resolution repository or lease is invalid"
        );
        ensure!(
            fixed.is_none_or(|expected| expected == &self.snapshot),
            "fixed snapshot changed during resolution"
        );
        ensure!(
            requested.len() <= 4096 && self.packages.len() == requested.len(),
            "resolution coordinate count mismatch"
        );
        let mut remaining = BTreeMap::new();
        for coordinate in requested {
            crate::validate_name(&coordinate.name)?;
            semver::Version::parse(&coordinate.version)?;
            *remaining
                .entry((&coordinate.name, &coordinate.version))
                .or_insert(0usize) += 1;
        }
        for package in &self.packages {
            let count = remaining
                .get_mut(&(&package.coordinate.name, &package.coordinate.version))
                .ok_or_else(|| anyhow::anyhow!("resolution returned an unrequested coordinate"))?;
            ensure!(*count > 0, "resolution duplicated a coordinate");
            *count -= 1;
            if let Some(revision) = &package.revision {
                uuid::Uuid::parse_str(&revision.id)?;
                revision.entry.validate()?;
                ensure!(
                    revision.repository_id == repository
                        && revision.entry.name == package.coordinate.name
                        && revision.entry.vers == package.coordinate.version
                        && revision.entry.metadata_digest()? == revision.metadata_sha256,
                    "resolution revision identity or metadata mismatch"
                );
            }
        }
        ensure!(
            remaining.values().all(|count| *count == 0),
            "resolution omitted a requested coordinate"
        );
        Ok(())
    }
}

impl LeaseProof {
    /// 业务作用：复验续期确认仍保护最初解析的快照集合，防止把另一租约误认为当前构建证据。
    /// 参数说明：
    /// - `resolution`：客户端已经核验的原解析响应。
    ///
    /// 返回：租约、仓库、快照及集合均一致且期限有效时成功。
    pub fn validate(&self, resolution: &Resolution) -> Result<()> {
        ensure!(
            self.id == resolution.lease_id
                && self.repository_id == resolution.snapshot.repository_id
                && self.snapshot_id == resolution.snapshot.id
                && self.snapshot_set_id == resolution.snapshot_set_id
                && self.expires_at > 0,
            "retention lease binding mismatch"
        );
        Ok(())
    }
}
