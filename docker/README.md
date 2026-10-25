# Naxus 容器

同版本可覆盖的 Rust 私服，带不可变历史快照、公开 sparse registry 缓存和同端口 Admin。开发人员用 `cargo naxus` 消费同版本新修订；普通 Cargo 不会自动刷新同版本内容。

[English overview and client guide](../README.en.md)

## 构建

```sh
docker build --file docker/Dockerfile --tag naxus:local .
export NAXUS_IMAGE=naxus:local
```

源码镜像使用固定 Rust 与 Debian 基础镜像摘要。`docker/Dockerfile.artifact` 接受 `dist/naxus` 中与目标平台匹配的预构建二进制，供交付流程使用。目标平台为 Linux `amd64` 和 `arm64`；跨平台源构建需要配置 Buildx 的对应构建节点或模拟器。

Docker Hub 地址和版本由发布者指定，不能把示例地址当作已有公开镜像。使用远程镜像时，将 `NAXUS_IMAGE` 设为实际的 `<namespace>/naxus:<version>`，或固定为 `<namespace>/naxus@sha256:<manifest-digest>`。服务镜像只提供 Naxus；开发机和 CI 另行安装 `cargo-naxus` 与 credential provider。

## 启动和持久卷

把 `NAXUS_IMAGE` 设置为实际镜像引用，然后运行：

```sh
docker compose --file docker/compose.yml up --detach
docker compose --file docker/compose.yml logs --follow
```

Compose 默认只把宿主 `127.0.0.1:7443` 暴露给本机或本机反向代理，使用 `restart: unless-stopped` 和 75 秒停机宽限。Docker 服务本身需配置开机启动。

| 项目 | 镜像默认值 |
| --- | --- |
| 运行用户 | UID/GID `10001:10001` |
| 单一服务端口 | `7443`，Admin 路径 `/admin/` |
| 数据根 | `/var/lib/naxus` |
| 日志根 | `/usr/local/logs/naxus` |
| 配置 | `/opt/naxus/zcf/application.yml` |
| 备份挂载点 | `/var/backups/naxus` |
| Cargo 认证 | managed |
| 停机信号 | SIGTERM |

数据和日志分别使用 named volume。整个数据根必须持久保存，包括 SQLite/WAL/SHM、对象、稳定身份和清除证据；不能仅保存 objects。一个数据根只运行一个活动实例。

Docker Desktop 使用 Linux 原生 named volume，宿主共享目录不能作为数据根。原生 Linux 允许受支持本地文件系统 bind mount。数据根归运行账号所有，权限为 0700；使用自定义宿主目录或 UID 时，提前配置对应所有权，只有组写权限不足以满足该合同。镜像不要求 root，也不禁止运维显式指定 `--user 0:0`；不通过启动脚本自动改变任意宿主目录所有权。

日志表达式为 `${NAXUS_LOG_PATH:/usr/local/logs/${application.name}}`。自定义日志路径必须可写。只读容器根需要为数据和日志提供可写挂载；健康检查仅查询就绪状态，不写入制品。

单容器也可以直接使用只读根运行，数据和日志保存在各自可写卷中：

```sh
docker run --detach --name naxus --read-only --restart unless-stopped --stop-timeout 75 --publish 127.0.0.1:7443:7443 --volume naxus-data:/var/lib/naxus --volume naxus-logs:/usr/local/logs/naxus --log-driver json-file --log-opt max-size=10m --log-opt max-file=3 "$NAXUS_IMAGE"
```

## 首次登录

```sh
docker compose --file docker/compose.yml exec naxus cat /var/lib/naxus/bootstrap/admin.password
```

打开 `http://127.0.0.1:7443/admin/`，用户为 `admin`，使用生成的临时口令登录并设置新口令。不要将实际口令复制到公共工单或日志。换密后临时明文移除；已有数据卷重启不会重置账号。

在 Admin 创建开发用户及其仓库范围，再创建读取/发布 token。通过客户端 `NAXUS_TOKEN` 或受限文件提供 token，不把它写进 Dockerfile、业务 manifest 或 Git。业务工作区接入：

```sh
cargo naxus configure --endpoint https://registry.example.com --repository team-dev
cargo naxus publish --workspace --atomic
cargo naxus build
cargo naxus build --locked
```

## 远程部署

设置 `NAXUS_PUBLIC_URL=https://registry.example.com`，由 HTTPS 反向代理转发到容器。业务端只接受远程 HTTPS；服务端仍由一个 listener 处理 Admin 与 Cargo。代理配置上传大小、请求期限、规范 Host/Origin，并按网络需求限制 `/admin/` 和观测路径。

挂载完整自定义 YAML 可配置多个 hosted/proxy：

```yaml
volumes:
  - ./application.yml:/opt/naxus/zcf/application.yml:ro
```

文件中日志路径仍使用统一表达式。省略 proxies 默认 crates.io；显式列表替换默认值，同一来源的镜像按顺序回源，独立 registry 不混合。配置示例见[配置文档](../docs/configuration.md)。

## 清理与备份

删除操作只从 Admin 发起。逻辑移除不释放空间，物理清除须通过影响预览和保护引用检查。公开缓存清除不封禁坐标，同版本和未来版本可重新回源。持久任务在页面关闭后继续，失败保留阶段并退避，可由运维重试。

先停止服务，再使用同一镜像的一次性备份进程。以下卷名是示例，应替换为实际 Compose 数据卷名：

```sh
docker stop --time 75 naxus
docker run --rm --volume naxus-data:/var/lib/naxus --volume naxus-backups:/var/backups/naxus "$NAXUS_IMAGE" backup --data-dir /var/lib/naxus --output /var/backups/naxus/saved
docker start naxus
```

备份目标须不存在。备份使用 SQLite 一致备份 API、独立对象副本和摘要清单，不是复制单个运行中的数据库文件。将备份另存到安全介质；卷本身不能替代异地副本。

恢复到全新数据卷：

```sh
docker run --rm --volume naxus-restored:/var/lib/naxus --volume naxus-backups:/var/backups/naxus:ro "$NAXUS_IMAGE" restore --backup /var/backups/naxus/saved --data-dir /var/lib/naxus/restored
```

新服务的 `NAXUS_DATA_PATH` 指向 `/var/lib/naxus/restored`，并挂载该新卷。恢复保留 UUID、账号和历史；外部 YAML、TLS 与 secret 文件单独保管。备份包含账号散列和可能存在的引导材料，按敏感运维数据保护。

## 健康与升级

镜像内置 `naxus healthcheck`，请求回环 `/readyz`，只在成功状态退出零。默认单制品上限 256 MiB、保留 2 GiB 空闲，低容量拒绝新写入而保留已有读取。`/metrics` 输出有限指标族，不包含 token、用户名或任意 URL 标签。

升级前停服并保留完整备份，重新创建容器时复用数据与日志卷及相同规范 URL。新版本只允许向前迁移；旧版本不认识 schema 时拒绝打开。回退使用旧镜像及升级前备份，不直接让旧程序读取新 schema。单节点停机期间允许短暂不可用。

## Container summary

Naxus supports mutable development coordinates with immutable snapshots, persistent public registry caching and a same-port Admin UI. Use the companion Cargo client for same-version refresh. The image defaults to UID/GID 10001, managed authentication, `/var/lib/naxus` data and `/usr/local/logs/naxus` logs. Persist the whole data root using a native local volume, stop gracefully, and take a consistent backup before upgrading. There is no multi-writer HA or transparent zero-downtime upgrade. See the [English overview](../README.en.md) for client behavior and limits.
