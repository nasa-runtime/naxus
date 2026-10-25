#![doc = include_str!("../README.md")]

mod features;
mod filesystem;
mod identity;
mod index;
mod platform;
mod resolution;
mod source;

/// 单个发布批次的完整索引条目数组及客户端发布日志共用的字节预算。
pub const MAX_PUBLICATION_METADATA_BYTES: usize = 32 * 1024 * 1024;
/// 批次响应为条目数组额外保留有界的身份、状态和期限字段空间。
pub const MAX_BATCH_RESPONSE_BYTES: usize = MAX_PUBLICATION_METADATA_BYTES + 64 * 1024;
/// 单个发布意图允许原子提交的最大制品数量。
pub const MAX_BATCH_ITEMS: usize = 4096;

pub use filesystem::{create_private_directory, validate_local_filesystem};
pub use identity::{canonical_name, validate_name, ArtifactDigest, RepositoryPolicy};
pub use index::{DependencyExtensions, IndexDependency, IndexEntry, Revision, Snapshot};
pub use resolution::{Coordinate, LeaseProof, Resolution, ResolvedPackage};
pub use source::{registry_identity, validate_service_url};
