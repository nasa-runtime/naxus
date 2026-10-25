# 贡献说明

## 环境与构建

使用 Rust/Cargo 1.97 或更新版本，保留并提交 `Cargo.lock`。依赖来自 crates.io；工作区组件使用 path 与 version 并存的声明，源码归档仍由 Cargo 原生多包打包处理。

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --locked
cargo doc --workspace --locked --no-deps
cargo package --workspace --registry crates-io --locked
```

库组件使用 Cargo 标准可发布声明，使原生多包打包能验证尚未发布的成员依赖；上述命令仅构建和生成归档，不上传 registry。公开发布由维护者通过相应仓库权限和凭据执行。GitHub CI 在 Linux、macOS 和最低支持工具链上执行格式、lint、构建、文档与源码归档检查。

反复打包相同开发版本时，为每次源码变化使用全新的 `cargo package --target-dir <新目录>`，避免 Cargo 临时 registry 的同版本解包缓存复用旧内容；不要改写或删除 Cargo 全局缓存来模拟开发覆盖。二进制和 Docker Hub 交付配置见[交付说明](docs/delivery.md)。

## 实现合同

- `naxus-core` 提供身份及本地文件系统合同，不依赖网络、SQLite 或 Cargo 子进程。
- 正式对象先完成文件与目录同步，数据库事务最后切换 HEAD。
- 修订、快照、幂等结果和发布审计属于同一提交。
- 名称所有权与仓库策略不能在重复创建或同版本重发时被静默放宽。
- 读取固定快照不能回到当前 HEAD，也不能用其他摘要替代缺失对象。

新增或实质修改的业务函数使用中文说明业务作用、参数和返回语义。关键门禁解释设计原因及失败影响，保持注释与实际执行顺序一致。SQL 迁移采用描述业务含义的文件名。

公开能力、配置和失败行为变化时，同步维护中英文 README、组件说明与归档内容。源码归档采用显式 include 清单，不包含本地目录、凭据或构建缓存。

crate rustdoc 首页复用组件 README；修改能力时同时核对 description、keywords/categories、命令帮助与相关配置。说明当前行为、初始化顺序、安全不变量和失败结果，不将执行记录或开发批次写入公开说明。

## 提交变更

先通过 Issue 说明使用场景或预期行为，再提交范围明确的 Pull Request。描述触发条件、最终行为、兼容性、配置和持久格式影响。持久化变化应说明旧数据打开、升级和回退的边界，不能只改默认配置而静默改变既有身份。

公开说明中的示例地址、令牌和路径不得替代实际部署凭据；涉及敏感资料的讨论使用私有渠道。使用支持与诊断信息见[常见问题](docs/troubleshooting.md)。

## 贡献与安全

项目采用 MIT 或 Apache-2.0 双许可证。贡献者应确认有权提交所提供内容。安全问题通过 [SECURITY.md](SECURITY.md) 指定的私有渠道报告。
