# Naxus

[English](README.en.md)

当前产品版本：**1.0.0**。

Rust 本地制品仓库与团队远程私服。开发仓库允许 **相同 crate 名称、相同版本重复发布新内容**，团队无需为了每次联调修改版本号。内容变化形成不可变修订和快照，配套 `cargo naxus` 默认刷新已选版本的新修订，`--locked` 保持原来的依赖内容。

业务只配置一个 Naxus 服务地址。Naxus 代理 crates.io 和其他明确配置的公开 sparse registry，首次按需下载并核验原始包体，之后从持久缓存提供相同内容。Admin、registry、健康检查和指标共用一个端口；同一常驻服务可部署在本机、服务器或容器中。

## 同版本发布与可复现构建

```text
company-sdk 1.8.0 → 发布 A → 快照 A → 原始制品 A
                 → 发布 B → 快照 B → 原始制品 B

cargo naxus build           使用已选版本的当前修订
cargo naxus build --locked  使用锁文件固定的修订
cargo naxus update          让 Cargo 重新选择约束内的版本
```

`mutable` 允许新修订覆盖当前逻辑坐标。`immutable` 保持同坐标的包体和依赖语义，允许 yank 状态影响后续解析。历史索引保持不可变；相同内容和元数据重复提交复用现有 HEAD。

普通 `cargo build` 遵守 Cargo 的 registry 缓存和 checksum 合同，**不会自动刷新同版本内容**。同版本开发使用 `cargo naxus`；不可变版本也可用原生 Cargo 的发布、下载与 yank API。[Cargo registry 合同](https://doc.rust-lang.org/cargo/reference/registry-index.html)

## 运行架构

```text
业务工作区 → cargo naxus + credential provider → Naxus 唯一服务端口
                                                    ├─ hosted：坐标 → 修订 → 不可变快照
                                                    ├─ proxy：逻辑来源 → 有序镜像 → 持久缓存
                                                    └─ Admin / 健康 / 指标
                                                        ↓
                                              SQLite 元数据 + 原始制品文件
```

发布先校验并同步制品文件，再在一个 SQLite 事务中提交修订、快照、幂等结果与 HEAD。消费者先固定快照和来源、取得保护租约，再交给原生 Cargo 解析和构建；`Cargo.lock` 与 `naxus.lock` 通过持久日志一起提交。这样同版本更新、历史读取和中断恢复使用明确身份，不依赖清空 Cargo 缓存。

公开包体首次按需拉取并核对 checksum 后长期保存。一个 proxy 对应一个逻辑 registry，其物理镜像按配置顺序尝试；其他独立 registry 保持独立身份。同一个数据根只允许一个活动写入者，服务端不执行上传代码。完整顺序与边界见[架构说明](docs/architecture.md)。

## Docker 启动

镜像支持 `linux/amd64` 和 `linux/arm64`。本机启动时，将仓库数据与文件日志保存在独立持久卷中：

```sh
docker run -d --name naxus --restart unless-stopped --stop-timeout 75 \
  -p 127.0.0.1:7443:7443 \
  -v naxus-data:/var/lib/naxus \
  -v naxus-logs:/usr/local/logs/naxus \
  nasaruntime/naxus:1.0.0
```

Admin 地址：`http://127.0.0.1:7443/admin/`。初始用户名为 `admin`，查看一次性口令：

```sh
docker exec naxus cat /var/lib/naxus/bootstrap/admin.password
```

首次登录后必须修改口令；复用数据卷不会重置账号。`naxus-data` 保存完整仓库数据，`naxus-logs` 保存文件日志。

镜像内置 `application.yml`，无需挂载 YAML 即可启动。自定义配置时，将包含 `application.yml` 的目录只读挂载到 `/opt/naxus/zcf`，修改后重启服务。配置挂载、环境变量和远程 HTTPS 部署见[容器说明](docker/README.md)。

## 从源码构建和启动

要求 Rust/Cargo **1.97 或更新版本**。服务和客户端使用支持本地文件锁、原子改名和目录同步的 Linux/macOS 文件系统；Windows 尚不支持。SQLite 随程序编译，不需要数据库服务、Redis 或注册中心。

```sh
cargo build --workspace --release --locked
cargo install --path . --locked
cargo install --path crates/cargo-naxus --locked
cargo install --path crates/cargo-credential-naxus --locked
mkdir -p "$HOME/.local/state/naxus/logs"
export NAXUS_LOG_PATH="$HOME/.local/state/naxus/logs"
naxus serve --config zcf/application.yml
```

`naxus`、`cargo-naxus` 和 `cargo-credential-naxus` 是三个可执行程序。两个客户端程序应放在同一目录并加入 PATH。

默认监听 `127.0.0.1:7443`，数据在 `~/.naxus/repository`，开发仓库名为 `team-dev`，默认代理 crates.io。本机默认不要求 Cargo token；只有监听地址和对外地址均为回环地址时才能使用该模式。

Admin 地址为 `http://127.0.0.1:7443/admin/`。首次启动生成一次性管理员口令，保存在数据根的 `bootstrap/admin.password`，文件仅当前用户可读。登录后必须换密；重启不会重置账号。开发人员可在 Admin 创建自己的读取或发布 token。

日志统一为 `${NAXUS_LOG_PATH:/usr/local/logs/${application.name}}`。本机用户应将 `NAXUS_LOG_PATH` 设置为可写目录。配置、托管及备份见[运维说明](docs/operations.md)。

从第一次登录、创建 token 到生产者和消费者联调的完整步骤见[首次接入](docs/quickstart.md)。Docker 用户见[容器说明](docker/README.md)；客户端需要单独安装在开发机或 CI 中，服务镜像不包含 Cargo 工具链。

## 业务项目接入

先在 Admin 获取有目标仓库权限的 token，通过 `NAXUS_TOKEN` 环境变量或 `service.token_file` 提供。token 文件须受限为当前用户可读，令牌正文不进入配置、manifest、URL 或 Git。

在生产者和消费者的工作区分别执行一次：

```sh
cargo naxus configure --endpoint https://registry.example.com --repository team-dev
```

只保存 Naxus 入口、服务身份和服务端提供的逻辑来源映射，业务侧不需要配置 crates.io 的物理上游地址。

```sh
cargo naxus publish --workspace --atomic
cargo naxus publish -p company-sdk
```

发布使用 Cargo 原生打包，在独立源码副本内处理内部依赖，原业务 manifest 和 `Cargo.lock` 不被改写。批次以预期 HEAD 原子提交；部分发布仍把其他工作区成员视为私有依赖，未包含成员必须已存在于目标仓库。`--no-verify` 显式跳过 Cargo 打包时的编译验证，服务端归档和来源检查仍执行。

发布前持久保存固定归档和原批次意图；中断后下一条在线命令先恢复原字节与幂等键。恢复 publish 后不自动发布后续源码变更，需再执行一次 publish。`cargo naxus publish --abort-pending` 只处理未完成发布，不删除已提交包。将 `.naxus/publication*` 和 `.naxus/publications/` 加入业务项目的 Git 忽略规则，保留恢复材料直至终态明确；未恢复发布会阻止离线构建。

消费者 manifest：

```toml
[dependencies]
company-sdk = { version = "1.8", registry = "naxus" }
serde = { version = "1", features = ["derive"] }
```

```sh
cargo naxus build
cargo naxus run
cargo naxus check
cargo naxus test
cargo naxus update
cargo naxus build --locked
cargo naxus build --locked --offline
```

默认命令保持已选 SemVer，检查同坐标内容和元数据是否变化；只有 manifest/解析输入变化或显式 `update` 才重新选择约束内版本。离线模式要求双锁完整且本机已有所需 Cargo 缓存。联网 `--locked` 仍检查服务身份、历史可用性及公开物化撤销状态。

将 `Cargo.lock`、`naxus.lock` 和不含凭据的 `.naxus/config.toml` 提交到业务 Git。忽略 `.naxus/client.lock` 和 `.naxus/transactions/`。双锁由持久日志恢复，发现事务之外的文件改动会拒绝覆盖。

## 公开来源和缓存

省略 `naxus.proxies` 时启用 crates.io；显式 `proxies: []` 关闭公开代理；显式列表替换默认配置。完整示例见[配置说明](docs/configuration.md)。

同一逻辑来源的物理上游按列表顺序采用首个有效响应。独立 registry 使用不同 proxy 与稳定 UUID，同名同版本包不会混合。镜像返回成功但缺少所需版本时，客户端在新固定视图中有界重试完整依赖图，不拼接不同镜像的索引行。

索引按 TTL 或显式 update 条件重验；已核验包体按摘要持久保存。同一已知版本的 checksum 改变会被拒绝。默认拒绝回源私网地址、未声明下载来源及不受信任重定向，不向上游发送业务凭据。当前代理公开、无需认证的 HTTP sparse 上游，不提供 Git index 代理、上游私有凭据或自定义 CA 配置。

## Admin 与清除

Admin 提供用户角色与仓库授权、读取/发布 token、撤销、审计与修订历史分页、快照保留点、yank/unyank、回退、逻辑移除、公开缓存清理与持久物理清除。Cookie 与 Cargo token 分开，写请求复验 Origin 和 CSRF；Admin 变更在数据库事务内复验认证时的账号代际、角色与仓库范围。页面没有外部 CDN 依赖。

发布 token 默认只能首次发布；重复发布已知坐标的新内容需显式勾选覆盖权限，也可限制为精确包名列表。提交 HEAD 的事务内再次核对账号和 token 的当前权限。账号换密、停用或授权变化会使旧会话与 token 失效；系统保留至少一个启用的管理员。

- 逻辑移除只改变后续 HEAD 可见性，保留历史与字节。
- hosted 物理清除须显式撤销指定历史修订。HEAD、保留引用、活动构建租约和下载读者会阻止清除；永久清除后历史固定读取返回 `410 Gone`。
- 公开缓存清理不封禁名称或版本。同版本和新增版本都能重新回源；旧物化证据失效后须正常非 locked 构建重新物化。
- 其他来源仍引用共享对象时保留字节。后台任务按存储代次执行，旧任务不能删除重装对象。失败保留进度、退避和错误类别，可从页面重试。

没有删除 CLI。权限按当前角色与仓库 UUID 执行，读取 token 不能发布，发布 token 不能调用物理清除。公开代理是团队共享来源，不适合承载不同用户有独立上游访问控制的私有源。

Admin 会话失效后清空旧身份与页面并返回登录；403 保留登录态，启动遇到 503 或网络故障时提供重连入口。管理输入无效为 400、越权为 403、状态或保护引用冲突为 409，未知管理路径为 404，不声明 Cargo challenge。错误正文 `request_id` 与响应头一致，便于关联日志。完整合同见[HTTP 接口](docs/protocol.md)。

## 部署和边界

服务独立常驻，客户端退出不会结束仓库。默认单节点单写入实例，SQLite WAL/FULL 与先对象后引用的顺序保证原子可见。启动恢复持久任务并核验受保护对象，无法解释的丢失或损坏拒绝就绪。详见[架构说明](docs/architecture.md)。

数据根归运行账号所有，Unix 权限收紧为 0700，末级数据根不能是符号链接。受管构建的 path 依赖仅支持同一工作区成员；不接受 Git 依赖、`[patch]`、`[replace]`、Cargo 配置 include 或覆盖来源/锁文件的命令参数。

Docker 默认 UID/GID `10001:10001`，数据 `/var/lib/naxus`，日志 `/usr/local/logs/naxus`，采用 managed 认证。启动、持久卷、反向代理、备份和升级见 [Docker 文档](docker/README.md)。Linux 支持 ext4/XFS/Btrfs/F2FS/overlayfs，macOS 支持 APFS/HFS+。Docker Desktop 使用 Linux 原生 named volume，宿主目录共享挂载不作为数据卷支持。

默认单包上限 256 MiB、数据卷预留 2 GiB。低容量拒绝新增写入，已有完整包仍可读取。上游中断不撤销已保存依赖图的可读性；显式刷新无法取得新鲜证据时失败且不改写双锁。`/metrics`、`/livez`、`/readyz` 由 NASA 的唯一 Web listener 提供。

从未被业务修订或公开缓存引用的上传对象经过一小时宽限后可由后台回收，开放批次、活动解析和下载仍受保护；这项治理不自动淘汰已发布的历史制品。

不提供多节点并发写、零中断升级、企业 SSO、公开账号注册、Cargo owner/search API 或执行归档代码的服务端验证。构建脚本和过程宏在消费者执行，身份认证不等同于代码可信。

## 组件

| 组件 | 职责 |
| --- | --- |
| [naxus-core](crates/naxus-core/README.md) | 身份、索引、修订、快照与文件系统合同 |
| [naxus-store](crates/naxus-store/README.md) | SQLite、对象、批次、恢复、备份和管理事务 |
| [naxus-registry](crates/naxus-registry/README.md) | 归档读取、manifest 与 Cargo 发布元数据一致性 |
| [naxus-server](crates/naxus-server/README.md) | NASA 受管协议、代理及 Admin |
| [naxus-client](crates/naxus-client/README.md) | 配置、凭据来源与双锁事务 |
| [cargo-naxus](crates/cargo-naxus/README.md) / [cargo-credential-naxus](crates/cargo-credential-naxus/README.md) | 原生 Cargo 编排与凭据提供器 |

## 使用文档

| 内容 | 入口 |
| --- | --- |
| 首次部署、同版本发布和消费 | [首次接入](docs/quickstart.md) |
| 命令、双锁、离线和发布恢复 | [客户端](docs/client.md) |
| 身份、原子提交和历史保护 | [架构](docs/architecture.md) |
| YAML、多个来源、认证和环境变量 | [配置](docs/configuration.md) |
| 路由、认证和错误状态 | [HTTP 接口](docs/protocol.md) |
| 日志、备份、恢复和升级 | [运行维护](docs/operations.md) |
| 镜像、持久卷和 Docker Hub 使用 | [容器](docker/README.md) |
| GitHub 与镜像发布配置 | [交付](docs/delivery.md) |
| 错误处理与支持渠道 | [常见问题](docs/troubleshooting.md) |

## 许可证与安全

可选择 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE)。安全问题遵循 [SECURITY.md](SECURITY.md)，开发约定见 [CONTRIBUTING.md](CONTRIBUTING.md)。
