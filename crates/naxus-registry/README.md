# naxus-registry

以有界流式读取检查标准 Cargo `.crate`，不解包到服务器文件系统，也不执行包内代码。检查坐标、唯一路径、目录穿越、链接与特殊文件、解压预算、gzip 完整性及规范化 manifest。

从归档生成 sparse index 元数据，保留依赖别名、kind、target、feature、links、rust-version 和来源。同仓稳定 `registry-index` 转换为索引内同仓引用，默认依赖保留 crates.io 身份，不静默借用同名私有包。

当前不支持实验性的 artifact 和 public dependency。归档与发布 JSON 都检查已知扩展字段类型，拒绝 `public = true`、artifact、artifact target 和 `lib = true`，不会把它们转换为普通库依赖。外层 `[target.'cfg(...)'.dependencies]` 继续作为普通平台条件保留。普通依赖的中性默认值可以省略或显式声明。

默认展开预算为 512 MiB、十万条目、1 MiB manifest 和 4096 字节路径。接受普通文件、目录及有界 GNU 长名称；其他归档扩展拒绝。原始包体摘要必须由调用者在此前校验。

`PublishMetadata::validate_archive` 核对 Cargo 原生发布 JSON 与归档中的名称、版本、依赖别名及来源、feature、links 和 MSRV，不让自报元数据替代归档语义。

许可证可选择 [MIT](https://github.com/nasa-runtime/naxus/blob/release/crates/naxus-registry/LICENSE-MIT) 或 [Apache-2.0](https://github.com/nasa-runtime/naxus/blob/release/crates/naxus-registry/LICENSE-APACHE)。
