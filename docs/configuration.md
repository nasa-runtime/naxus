# 配置

主配置选择优先级为 `naxus serve --config <路径>` → 非空 `APP_CONFIG_FILE` → `zcf/application.yml`。环境变量未设置或为空时使用默认路径；相对主路径以进程工作目录解析。选定文件不可读、解析或校验失败时拒绝启动，不回退到其他文件。NASA YAML 解析占位符，未知业务键会被拒绝。

例如，使用部署目录中的完整配置：

```sh
APP_CONFIG_FILE=/etc/naxus/application.yml naxus serve
```

`APP_CONFIG_FILE` 选择整份应用配置，不是附加业务配置。文件须同时包含必要的 Application、日志、Web、仓库与 `datasources.naxus` 设置，可从 [默认 YAML](../zcf/application.yml) 调整。配置在启动时固定；文件变化后重启服务，容器环境变化后重建容器。

```yaml
application:
  name: naxus
  mode: service
  shutdown_timeout_ms: 60000
log:
  level: info
  path: ${NAXUS_LOG_PATH:/usr/local/logs/${application.name}}
server:
  host: 0.0.0.0
  port: 7443
  health: true
  request_body_limit_bytes: 268435456
naxus:
  public_base_url: https://registry.example.com
  data:
    root: /var/lib/naxus
    hard_free_bytes: 2147483648
    max_artifact_bytes: 268435456
  auth:
    mode: managed
  repositories:
    - name: team-dev
      policy: mutable
    - name: release
      policy: immutable
  proxies:
    - name: crates-io
      logical_index: https://github.com/rust-lang/crates.io-index
      index_ttl_seconds: 300
      upstreams:
        - index: https://index.crates.io/
          download_hosts: [static.crates.io, crates.io]
          request_timeout_seconds: 30
    - name: partner
      logical_index: sparse+https://partner.example.com/index/
      upstreams:
        - index: https://mirror-one.example.com/index/
          download_hosts: [packages.example.com]
        - index: https://mirror-two.example.com/index/
          download_hosts: [packages.example.com]
datasources:
  naxus:
    driver: sqlite
    sqlite:
      path: ${naxus.data.root}/naxus.sqlite3
      create_if_missing: true
      journal_mode: wal
      synchronous: full
      foreign_keys: true
      busy_timeout_ms: 5000
    max_connections: 1
    min_connections: 1
    transaction_timeout_ms: 300000
    max_result_rows: 1000000
    max_result_bytes: 268435456
    min_free_bytes: ${naxus.data.hard_free_bytes:2147483648}
```

物理 index 地址不带 `sparse+`；`logical_index` 保留 Cargo 识别的来源身份，禁止嵌入凭据、query 或 fragment。crates.io 的逻辑身份固定为官方 Git index 地址，即使实际使用 HTTP sparse 回源；官方 Git 与 sparse 别名按同一身份判重，其他 registry 的 Git/sparse 协议身份仍相互独立。一个逻辑来源内最多十六个同源镜像；独立来源必须单独配置 proxy。显式上游列表替换默认值，需回落官方源时把它明确放入列表。

`download_hosts` 的跨域 HTTPS 下载默认限定端口 443。同源 index 地址允许其显式端口。默认拒绝任何 DNS 候选地址落入私网、回环、链路本地或保留范围。内网镜像可在相应上游配置 `allow_private_network: true`；这是该上游的显式信任边界，也允许 HTTP。跨来源重定向仍须符合白名单，不自动继承业务凭据或环境代理。

`request_timeout_seconds` 限制单个上游 HTTP 请求的 DNS、跳转和完整响应读取，默认 30 秒，允许 1–120 秒。一次索引或包体回源的排队和全部镜像合计最多 120 秒；前序失败不重置总预算。较短的单源期限为后续镜像保留时间。

索引、`config.json` 与包体使用相同切换策略：404、连接/DNS/流中断、超时及 5xx 可以尝试下一同源镜像；401/403 立即停止，Naxus 返回 HTTP 502 和 `upstream_unauthorized` / `upstream_forbidden`，表示上游拒绝访问，不要求业务客户端更换 Naxus 凭据。TLS 身份、响应结构、来源映射、checksum 或已有坐标冲突也立即停止，异常内容不进入缓存，不通过后备地址掩盖。

429 解析 `Retry-After` 的秒数或 HTTP-date；缺失或非法时使用 30 秒，过大的秒数和未来日期截断到该上游的 `max_cooldown_seconds`。此配置默认 300 秒，允许 1–3600 秒。零秒或过去日期允许立即再试。冷却按当前进程内的 proxy 与上游列表位置隔离，索引、配置和包体共用；冷却时立即跳过并尝试下一镜像，不阻塞等待，也不保存为 404。并发响应不能缩短已有较长冷却；服务重启会清空冷却状态。

启用[观测出口](#观测出口)后，`/metrics` 提供 `naxus_upstream_events_total{proxy,upstream,event}`、`naxus_upstream_failovers_total{proxy,upstream,reason}` 和 `naxus_upstream_cooldown_seconds{proxy,upstream}`。`upstream` 为从零开始的配置位置，不含地址；标签集合受配置上限约束。事件区分成功、缺失、认证、授权、限流、暂时不可用和完整性问题；切换原因是 `not_found`、`transient`、`rate_limited`、`cooldown`。事件可包含同一响应的成功接收与后续完整性拒绝，不作为独立 HTTP 请求数使用。

默认地址策略还拒绝 6to4 隧道、已废弃中继及 IPv6 文档网段，避免将特殊用途地址当作普通公网回源。地址用途参考 IANA 的 [IPv4](https://www.iana.org/assignments/iana-ipv4-special-registry/) 和 [IPv6](https://www.iana.org/assignments/iana-ipv6-special-registry/) 登记；显式受信任的内网镜像仍由 `allow_private_network` 控制。

## 认证

`sparse+` 逻辑来源的原始声明必须以 `/` 结尾；服务端和客户端都在保存配置或发起解析前拒绝缺少斜线的来源，不静默追加。Git registry URL 不受此限制。

- `managed`：本地账号、浏览器会话及独立 Cargo token，远程部署推荐使用。
- `token`：配置 `naxus.auth.token_file` 的受保护高熵单一凭据；供受控服务集成使用，具有全部仓库读写权限，不能登录 Admin。
- `none`：仅回环本机部署允许，Cargo 接口没有用户隔离；Admin 仍需独立账号登录。

可配置 `naxus.admin.bootstrap_password_file` 提供首次管理员引导口令；文件必须为当前用户受限读取的实体文件。未配置则自动生成 `<data-root>/bootstrap/admin.password`。已有账号不会重新引导。

`public_base_url` 必须对应业务使用的规范入口。服务本身接受反向代理转发的 HTTP，业务客户端访问远程地址必须 HTTPS。已有锁文件绑定该 URL；更换域名需要保留旧入口兼容期。URL 支持由代理去除的路径前缀；前缀只允许未保留字符组成的路径段，不能包含凭据、query 或 fragment。

## 环境变量

| 环境变量 | 默认配置中的作用 |
| --- | --- |
| `APP_CONFIG_FILE` | 非空时选择主 YAML；显式 `serve --config` 优先 |
| `NAXUS_HOST` / `NAXUS_PORT` | 监听地址，默认 `127.0.0.1:7443` |
| `NAXUS_PUBLIC_URL` | 对外规范地址，默认 `http://127.0.0.1:7443` |
| `NAXUS_DATA_PATH` | 数据根，未设置时使用 `~/.naxus/repository`，不得显式设为空值 |
| `NAXUS_LOG_PATH` / `NAXUS_LOG_LEVEL` | 日志路径与级别 |
| `NAXUS_AUTH_MODE` | 覆盖认证模式 |
| `NAXUS_ACCESS_MODE` | 未提供 AUTH_MODE 时的默认模式；容器设为 managed |
| `NAXUS_TOKEN` | 客户端 token，只由客户端读取 |
| `NAXUS_HEALTH_URL` | healthcheck 的显式回环 readiness URL |

## 客户端配置

通过 `cargo naxus configure` 生成 `.naxus/config.toml`。不要手动替换服务或代理 UUID 来绕过身份检查。来源配置变化后再次执行 configure 同步映射；变更物理镜像不改变逻辑来源身份。

可在 `[service]` 添加 `token_file`，相对路径以配置文件所在目录为基准。环境 `NAXUS_TOKEN` 优先；配置只保存路径，不保存令牌。credential provider 只为当前服务下已映射 hosted、snapshot、proxy 与 session 索引提供凭据。

受管构建不接受 Git 依赖、`[patch]`、`[replace]` 或 Cargo 配置 `include`。客户端在联网解析前检查这些来源替换入口，避免依赖绕过固定仓库身份；本地包依赖仅支持同一工作区成员，由 Cargo 处理。独立工作区或被 exclude 的本地包应通过 Naxus 发布后引用。

## SQLite 数据源

`datasources.naxus` 由 NASA `db` 组件创建，`driver` 固定为 `sqlite`。NASA 可按来源选择已编译的 driver；Naxus 当前仅启用 SQLite，文件元数据不支持换成 MySQL 或 PostgreSQL。数据库路径必须指向 `naxus.data.root/naxus.sqlite3`，且 `max_connections: 1`、WAL、FULL、外键和可写模式是仓库的启动合同。自定义配置文件也须提供这个 datasource。

`operation_timeout_ms` 默认 30000，示例将 `transaction_timeout_ms` 设为 300000。事务覆盖完整业务变更，超过总期限会回滚并拒绝该事务后续 SQL，不自动重试写入。`max_result_rows` 与 `max_result_bytes` 限制单次完整查询的内存使用，示例为一百万行和 256 MiB；结果超限明确失败，不返回截断结果。数据库 `min_free_bytes` 应与 `naxus.data.hard_free_bytes` 保持一致，默认 2 GiB。

数据库文件、WAL、SHM、journal 与 owner 锁必须为当前运行账号的私有实体文件，主文件不能是符号链接或硬链接。取得仓库实例锁后会收紧已有数据库及 sidecar 权限。不要删除活动 WAL、SHM、实例锁或 `.owner.lock`，也不要用其它工具写入服务正在使用的数据库。

## 观测出口

需要在同一个 Web 端口公开 `/metrics` 时，显式配置 NASA 观测出口：

```yaml
grafana:
  observability:
    enabled: true
    identity:
      environment: local
      cluster: local
    prometheus:
      scrape:
        listener: web
```

未启用出口时仍登记组件观测，HTTP 不提供 `/metrics`。非本地部署应填写实际 environment、cluster 和稳定 instance_id，并按网络边界限制指标访问。
