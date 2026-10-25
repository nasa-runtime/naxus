# Naxus Docker 镜像

Rust 本地制品仓库与远程私服，支持同版本新修订、不可变快照、公共 sparse registry 缓存和同端口 Admin。开发机与 CI 使用配套 `cargo naxus` 客户端；镜像只运行服务，不包含 Rust 编译器或 Cargo。

[English guide](../README.en.md)

## 最简启动

镜像地址为 `nasaruntime/naxus:1.0.0`。部署时使用已发布的版本标签，也可固定多架构 manifest digest。

```sh
docker run -d --name naxus --restart unless-stopped --stop-timeout 75 \
  -p 127.0.0.1:7443:7443 \
  -v naxus-data:/var/lib/naxus \
  -v naxus-logs:/usr/local/logs/naxus \
  nasaruntime/naxus:1.0.0
```

打开 `http://127.0.0.1:7443/admin/`。首次启动的用户名为 `admin`，查看一次性口令：

```sh
docker exec naxus cat /var/lib/naxus/bootstrap/admin.password
```

登录后必须换密，临时口令文件随后移除；复用已有数据卷不会重置账号。创建开发用户及仓库权限，再签发客户端读取或发布 token。发布同版本新内容还需要显式覆盖权限。

```sh
docker logs -f naxus
docker exec naxus naxus healthcheck
docker stop naxus
docker start naxus
```

默认只把端口暴露到宿主回环地址，适合本机或同宿主反向代理。服务常驻运行，`unless-stopped` 在 Docker 重启后恢复服务；Docker 自身需要配置开机启动。

## Compose

源码仓库已提供默认镜像、持久卷、75 秒停机宽限和标准输出日志轮转：

```sh
docker compose -f docker/compose.yml up -d
docker compose -f docker/compose.yml logs -f
```

可通过 `NAXUS_IMAGE` 选择其他已发布标签或 digest。Compose 的 `down` 保留数据；`down --volumes` 会删除 named volume，仅在明确不再需要这些数据时使用。

## 数据卷与运行身份

| 内容 | 容器路径 | 挂载方式 |
| --- | --- | --- |
| 完整仓库数据 | `/var/lib/naxus` | named volume `naxus-data`，必须持久保存 |
| 文件日志 | `/usr/local/logs/naxus` | named volume `naxus-logs` |
| 默认 YAML | `/opt/naxus/zcf/application.yml` | 镜像内置，可将配置目录只读挂载到 `/opt/naxus/zcf` |
| 备份目录 | `/var/backups/naxus` | 备份操作时挂载独立卷或受限路径 |

数据包含 SQLite/WAL/SHM、原始对象、稳定服务/仓库 UUID、账号、快照、恢复和清除记录，不能只挂载 objects。一个数据根只允许一个活动写入者。

默认 UID/GID 为 `10001:10001`，新建 named volume 自动使用镜像预置目录的所有权。数据根属于运行用户且权限为 0700；不需要以 root 启动。原生 Linux 使用自定义 bind mount 或 `--user` 时，先设置目录所有权，不能仅授予组写权限。镜像不会自行修改任意宿主路径的所有者。

Docker Desktop 使用 Linux 原生 named volume，宿主共享目录不能作为数据根。Linux bind mount 需位于 ext4/XFS/Btrfs/F2FS/overlayfs；配置文件只读挂载不受数据根的该限制。只读容器根可加 `--read-only`，数据和日志卷仍需可写。

## 挂载自定义 YAML 目录

准备真实存在、可读取的 `/path/to/naxus-conf/application.yml`，将整个配置目录挂载到容器。数据仍使用持久卷：

```sh
docker run -d --name naxus --restart unless-stopped --stop-timeout 75 \
  -p 127.0.0.1:7443:7443 \
  -v naxus-data:/var/lib/naxus \
  -v naxus-logs:/usr/local/logs/naxus \
  --mount type=bind,src=/path/to/naxus-conf,dst=/opt/naxus/zcf,readonly \
  nasaruntime/naxus:1.0.0
```

完整配置以仓库的 `zcf/application.yml` 为基础；多个 hosted/proxy、认证和镜像回源示例见[配置文档](../docs/configuration.md)。日志保持：

```yaml
log:
  path: ${NAXUS_LOG_PATH:/usr/local/logs/${application.name}}
```

只读只限制容器写入，宿主仍可编辑文件。目录挂载允许编辑器通过原子替换保存 application.yml，容器也能看到新文件；单文件 bind mount 可能继续引用旧 inode。

内置 YAML 通过占位符读取环境变量。自定义文件若把端口、路径等写成固定值，同名环境变量不会自动覆盖，须保留相应占位符。当前业务配置在启动时固定，修改目录中的 YAML 后执行 `docker restart naxus`；没有运行中热加载。环境变量属于容器创建配置，修改后需重建容器，单纯 restart 不会更新环境。

Compose 可在服务的 volumes 中追加：

```yaml
- /path/to/naxus-conf:/opt/naxus/zcf:ro
```

## 环境变量与端口

| 变量 | 镜像或内置 YAML 默认值 | 作用 |
| --- | --- | --- |
| `NAXUS_HOST` | `0.0.0.0` | 容器内监听地址 |
| `NAXUS_PORT` | `7443` | 容器内服务端口，Admin、Cargo、健康和指标共用 |
| `NAXUS_PUBLIC_URL` | `http://127.0.0.1:7443` | 客户端和浏览器使用的规范外部入口 |
| `NAXUS_DATA_PATH` | `/var/lib/naxus` | 完整数据根；修改时同步调整数据卷目标 |
| `NAXUS_LOG_PATH` | `/usr/local/logs/naxus` | 文件日志根；修改时同步调整日志卷目标 |
| `NAXUS_LOG_LEVEL` | `info` | 日志级别 |
| `NAXUS_AUTH_MODE` | `managed` | 账号及独立 token 认证，远程部署保持该值 |
| `NAXUS_HEALTH_URL` | 按 NAXUS_PORT 构造回环 readyz | 自定义 YAML 固定其他端口时可显式指定健康检查入口 |
| `TZ` | `Etc/UTC` | IANA 时区，例如 `Asia/Shanghai` |

宿主端口由 `-p` 控制，与容器内监听端口分开。仅将宿主端口改为 8443 时保留容器端口，并同步规范 URL：

```sh
docker run -d --name naxus --restart unless-stopped --stop-timeout 75 \
  -p 127.0.0.1:8443:7443 \
  -e NAXUS_PUBLIC_URL=http://127.0.0.1:8443 \
  -e TZ=Asia/Shanghai \
  -v naxus-data:/var/lib/naxus \
  -v naxus-logs:/usr/local/logs/naxus \
  nasaruntime/naxus:1.0.0
```

如需把容器端口本身改为 8443，还要设置 `NAXUS_PORT=8443` 并使用 `-p 127.0.0.1:8443:8443`。只改端口映射不会改变应用监听或对外 URL。

`DOCKERHUB_*` 只用于 GitHub 发布，不传给服务容器。业务 token 在 Admin 签发，客户端通过 `NAXUS_TOKEN` 或受限 token 文件提供，不写进 Dockerfile、业务 manifest 或 Git。

## 远程访问

设置 `NAXUS_PUBLIC_URL=https://registry.example.com`，由 HTTPS 反向代理转发到服务端口。客户端远程入口要求 HTTPS；容器内 HTTP 仍由一个 listener 处理 Admin 与 Cargo。代理配置上传大小、请求期限和规范 Origin，并按网络范围限制管理与观测路径。

容器与反向代理不在同一宿主时，按实际网络设置宿主监听地址或加入相同 Docker 网络。将宿主端口开放到外网本身不提供 TLS。Admin 默认地址是规范 URL 加 `/admin/`。

## 精简运行层与构建

运行层从固定摘要的 Alpine 提取 CA 证书、时区和静态 BusyBox 基础命令，与静态 musl 服务程序一起装入空白镜像。镜像不包含编译工具、客户端或开发账号，也不使用可执行压缩器。保留标准退出、信号和持久化语义。

从源码构建：

```sh
docker build -f docker/Dockerfile -t naxus:local .
```

`docker/Dockerfile.artifact` 使用同一运行层，接收 `dist/naxus` 中对应目标平台的静态程序。发布流程将同一构建的程序同时放入 Linux 二进制归档与服务镜像，核对两者字节身份。提供 `linux/amd64` 和 `linux/arm64`；原生架构自动选择对应镜像。

## 备份与恢复

先停服，再使用同一镜像运行备份；备份目标必须不存在：

```sh
docker stop naxus
docker run --rm -v naxus-data:/var/lib/naxus -v naxus-backups:/var/backups/naxus \
  nasaruntime/naxus:1.0.0 backup --data-dir /var/lib/naxus --output /var/backups/naxus/saved
docker start naxus
```

备份保存 SQLite 一致视图、独立对象副本和摘要清单，不是复制运行中的数据库主文件。备份含账号散列及可能存在的引导材料，应受限存放并保留独立副本。

恢复到新卷：

```sh
docker run --rm -v naxus-restored:/var/lib/naxus -v naxus-backups:/var/backups/naxus:ro \
  nasaruntime/naxus:1.0.0 restore --backup /var/backups/naxus/saved --data-dir /var/lib/naxus/restored
```

新服务挂载 `naxus-restored:/var/lib/naxus` 并设置 `NAXUS_DATA_PATH=/var/lib/naxus/restored`。恢复保留 UUID、账号和历史；外部 YAML、TLS 和 secret 文件另行保存。Compose 的实际卷名可能带项目前缀，备份命令应使用对应卷名。

升级前停服并保留完整备份，复用规范 URL 和持久卷。旧程序不认识新 schema 时拒绝打开，回退应使用旧镜像及升级前备份。单节点停机期间允许短暂不可用。

## 管理与容量

删除从 Admin 发起，没有删除 CLI。逻辑移除不释放字节；物理清除需要预览、明确历史撤销和保护引用检查。HEAD、保留点、有效租约与活动读者不能被强制跨过。公开缓存清理不封禁坐标，同版本和未来版本可再次回源。

默认单制品上限 256 MiB、保留空闲空间 2 GiB；低容量拒绝新写入并保留已有完整对象读取。文件日志默认上限和轮转由 YAML 管理，Compose 另限制标准输出日志大小。维护、完整性和失败语义见[运维说明](../docs/operations.md)。

## Container summary

Naxus provides same-version Rust publishing, immutable snapshots, public registry caching and a same-port Admin UI. The image contains a static musl executable, CA certificates, time zones and static BusyBox tools, without a package manager or dynamic libraries. It defaults to UID/GID 10001, port 7443, managed authentication, persistent data at /var/lib/naxus and logs at /usr/local/logs/naxus. Mount the whole data root, bind custom YAML read-only, and set NAXUS_PUBLIC_URL to the client-facing HTTPS endpoint for remote use. See the [English guide](../README.en.md) for client behavior.
