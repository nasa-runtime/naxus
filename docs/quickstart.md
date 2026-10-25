# 首次接入

本教程使用本机 managed 服务、一个生产者组件和一个消费者，演示不改变版本号的发布与调用。团队部署沿用相同客户端流程，将回环地址换为 HTTPS 私服地址。

## 安装与启动服务

从源码仓库根目录安装三个程序：

```sh
cargo install --path . --locked
cargo install --path crates/cargo-naxus --locked
cargo install --path crates/cargo-credential-naxus --locked
mkdir -p "$HOME/.local/state/naxus/logs"
export NAXUS_LOG_PATH="$HOME/.local/state/naxus/logs"
export NAXUS_AUTH_MODE=managed
naxus serve --config zcf/application.yml
```

需要 Rust/Cargo 1.97 或更新版本。两个客户端程序须位于 PATH 中，服务保持前台运行。在另一个终端继续以下操作；本机数据根默认为 `~/.naxus/repository`。

打开 `http://127.0.0.1:7443/admin/`，以 `admin` 登录。首次口令位于数据根的 `bootstrap/admin.password`；设置新口令后该临时文件被移除。已有数据根不会重新生成账号。

在 Admin 创建开发用户，授予 `team-dev` 仓库权限。生产者需要 publish token；同版本新内容的发布还需要显式覆盖权限。消费者可以使用只读 token。通过进程环境注入 `NAXUS_TOKEN`，或在客户端生成的配置中设置 `service.token_file` 指向仅当前用户可读的文件。不要将实际令牌写入命令历史、Git 或公开报告。

## 发布生产者组件

在业务目录创建独立组件：

```sh
cargo new --lib company-sdk --edition 2021
cd company-sdk
cargo naxus configure --endpoint http://127.0.0.1:7443 --repository team-dev
```

把 `src/lib.rs` 内容设为：

```rust
pub const VALUE: u8 = 1;
```

保持 `Cargo.toml` 中的 `version = "0.1.0"`，发布：

```sh
cargo naxus publish -p company-sdk
```

发布使用独立源码副本和 Cargo 原生打包；原业务 manifest 与 `Cargo.lock` 不会被发布来源改写。服务在预期 HEAD 上原子提交修订和快照。

## 消费组件与公开依赖

在相邻目录创建消费者：

```sh
cargo new business-service --edition 2021
cd business-service
cargo naxus configure --endpoint http://127.0.0.1:7443 --repository team-dev
```

在 `Cargo.toml` 的 `[dependencies]` 中添加：

```toml
company-sdk = { version = "=0.1.0", registry = "naxus" }
serde = { version = "1", features = ["derive"] }
```

将 `src/main.rs` 设为：

```rust
/// 业务作用：读取依赖组件提供的业务常量。
/// 参数说明: 无。
///
/// 返回：向标准输出写出当前构建所固定的值。
fn main() {
    println!("{}", company_sdk::VALUE);
}
```

执行：

```sh
cargo naxus run
```

输出为 `1`。私有组件来自 hosted 快照，serde 等公开依赖通过 Naxus 代理；消费者不需要配置 crates.io 的物理下载地址。服务第一次取得公开包体后按摘要持久保存，后续相同内容直接从本地缓存提供。

## 同版本发布新内容

把生产者的 `VALUE` 改为 `2`，版本仍为 `0.1.0`，再次执行：

```sh
cargo naxus publish -p company-sdk
```

回到尚未更新双锁的消费者：

```sh
cargo naxus run --locked
cargo naxus run
```

第一条仍输出 `1`，第二条发现同坐标的新修订后输出 `2`，并提交新的双锁。此后 `--locked` 固定新双锁中的内容。该行为需要 `cargo naxus`；普通 `cargo build` 不会刷新同版本内容。

提交 `Cargo.lock`、`naxus.lock` 和不含凭据的 `.naxus/config.toml`。业务项目忽略以下运行材料：

```gitignore
.naxus/client.lock
.naxus/transactions/
.naxus/publication*
.naxus/publications/
```

需要更换约束内的 SemVer 时使用 `cargo naxus update`。完整命令、发布恢复和离线条件见[客户端说明](client.md)，部署与故障处理见[运行维护](operations.md)和[常见问题](troubleshooting.md)。
