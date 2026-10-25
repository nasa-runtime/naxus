# naxus-core

Naxus 的制品身份与持久化平台合同，支持同版本新修订与不可变快照，不依赖 Web、SQLite 或 Cargo 子进程。

- `ArtifactDigest` 只接受 64 位小写十六进制 SHA-256；二进制摘要直接作用于原始字节。
- Cargo package 名称限制为 ASCII 字母、数字、连接符和下划线，首字符为字母，最大 64 字节，拒绝保留文件名。
- `canonical_name` 折叠大小写及连接符差异，只用于所有权比较，不改写公开索引中的原名。
- `IndexEntry::validate` 校验 SemVer、依赖版本要求和索引基础结构；不允许借 SemVer build metadata 区分同版本发布。
- `IndexEntry::validate_upstream` 校验公开索引的 schema、MSRV、依赖声明、target 条件语法及本 crate 的 feature 图，保留上游合法的 build metadata；失败时不得替换已有缓存。target 使用最低支持 Cargo 同源的 `cargo-platform` 解析命名平台及 `cfg(...)`，保留原文，不按仓库主机决定是否生效。feature 校验覆盖 Unicode 名称、普通引用、`dep:`、`name/feature`、`name?/feature`、可选依赖的隐式开关，以及 `features`/`features2` 合并；不额外施加 crates.io 的 ASCII feature 名称策略。
- `metadata_digest` 覆盖完整索引记录，区分相同包体但不同依赖、feature 或来源的声明。
- `DependencyExtensions` 对 Cargo 已知的 `public`、`artifact`、`bindep_target`、`lib` 字段执行类型检查。当前仅支持普通库依赖：`public = true`、非 null 的 artifact/构建目标及 `lib = true` 均拒绝；`public = false`、null 的可选字段和 `lib = false` 保持普通语义。未知未来字段仍可忽略，不代表支持其新增语义。
- 单个 target 声明最多 16 KiB，字符串外括号最多嵌套 64 层；进入 Cargo 解析前以无递归扫描执行预算，保护不可信索引解析的调用栈。字符串值内的括号不计入嵌套。
- `RepositoryPolicy` 区分 mutable 和 immutable；`Revision`、`Snapshot` 表达不可变读取身份。
- `validate_local_filesystem` 核对 Linux/macOS 本地存储类型，拒绝不满足独占锁和目录同步合同的挂载。
- `create_private_directory` 以 Unix 0700 创建并持久化私有目录链，拒绝末级符号链接；已有目录需要归运行账号所有。
- `validate_service_url` 统一远程 HTTPS 与回环 HTTP 边界；`registry_identity` 要求 sparse 声明以 `/` 结尾、归一标准 crates.io 别名并保留其他来源的 Git/sparse 差异。其返回值仅供身份比较，不替代 Cargo 配置中的原始 URL。
- `MAX_BATCH_ITEMS`、`MAX_PUBLICATION_METADATA_BYTES` 和 `MAX_BATCH_RESPONSE_BYTES` 约束发布条目数量、完整条目数组/本地日志及批次响应，服务端接受的批次不能超出客户端终态读取预算。
- `Resolution::validate` 绑定请求仓库、坐标多重集合、完整固定快照和修订摘要；`LeaseProof` 绑定同一持久租约及快照集合。

`registry = null` 的解释依赖索引所属来源。本组件保留该字段，不把公开依赖自动重写为私有依赖，也不自行发起上游网络请求。

结构、target 语法和本 crate 的 feature 图校验不能代替 Cargo 对完整依赖闭包、平台条件实际匹配、依赖包目标支持、实际 feature 和工具链兼容性的解析，也不能代替归档检查、身份认证与仓库授权。调用者必须在相应边界执行这些操作。

许可证可选择 [MIT](https://github.com/nasa-runtime/naxus/blob/release/crates/naxus-core/LICENSE-MIT) 或 [Apache-2.0](https://github.com/nasa-runtime/naxus/blob/release/crates/naxus-core/LICENSE-APACHE)。
