# HTTP 接口与失败语义

Admin、Cargo registry、扩展 JSON API、健康和指标共用 `server.port`。本文路径相对服务入口；反向代理可剥离 `public_base_url` 的规范路径前缀后转发。下载地址由服务配置构造，不从任意 Host 头推导。

## 身份边界

Cargo 与扩展 API 使用独立 token，可通过 `Authorization: Bearer <token>` 或 Cargo 原始 token 头提供。managed 模式复验当前账号与 token 权限。浏览器 Cookie 不能代替 Cargo token，发布权限也不授予物理清除权限。

Admin 的 `/admin/api/v1/login` 建立浏览器会话。认证后的写操作需要同源 `Origin`、会话 Cookie 和 `X-CSRF-Token`。Cookie 的有效范围为 `/admin/`，初始换密状态不能执行普通管理动作。

## 入口

| 前缀或路径 | 用途 |
| --- | --- |
| `/admin/` | 同源管理页面，内嵌资源不依赖外部 CDN |
| `/admin/api/v1` | 用户、token、修订、管理预览、保留点及清除任务 |
| `/repositories/{repository}/index/` | hosted 当前逻辑 sparse 索引 |
| `/repositories/{repository}/snapshots/{snapshot}/index/` | 不可变快照 sparse 索引 |
| `/repositories/{repository}/api/v1/crates/new` | 原生 Cargo 发布 |
| `/repositories/{repository}/api/v1/crates/{name}/{version}/yank`、`/unyank` | 原生 Cargo yank/unyank |
| `/proxies/{proxy}/index/` | 独立公开来源的持久 sparse 缓存 |
| `/proxies/{proxy}/sessions` | 固定公开解析视图及物化集合 |
| `/api/naxus/v1` | 仓库能力、身份、解析、发布批次及保留租约 |
| `/livez`、`/readyz`、`/metrics` | NASA 进程状态、读取就绪和有限基数指标 |

sparse 请求先读取 `config.json`，再按标准 crate 名称路径读取 JSONL；包体使用响应中提供的下载模板。调用方保留原始包体字节并核对索引 checksum，不自行拼接其他来源的地址。

## 扩展 JSON API

以下路径相对 `/api/naxus/v1`：

| 方法与路径 | 输入与结果 |
| --- | --- |
| `GET /capabilities` | 无认证读取服务 UUID、协议范围、认证和限制 |
| `GET /identity` | 返回当前主体摘要，不公开原始 token |
| `GET /repositories`、`GET /proxies` | 返回当前可访问仓库与公开来源 |
| `POST /resolutions` | repository、client_protocol、packages、可选 fixed_snapshot；返回完整固定解析及租约 |
| `GET /repositories/{repository}/head` | 当前快照与 ETag，可使用 If-None-Match |
| `GET /repositories/{repository}/snapshots/{snapshot}` | 同仓精确快照 |
| `POST /repositories/{repository}/batches` | expected_head 与 idempotency_key；返回持久批次 |
| `GET /repositories/{repository}/batches/{batch}` | 当前主体的原批次状态与已登记内容 |
| `PUT /repositories/{repository}/batches/{batch}/crates/{name}/{version}` | 二进制 .crate 与 X-Checksum-Sha256；登记校验后的条目 |
| `POST /repositories/{repository}/batches/{batch}/commit` | 原批次权威决定提交，不另接收新幂等键或新 HEAD |
| `DELETE /repositories/{repository}/batches/{batch}` | 明确终止未提交批次，已提交终态不逆转 |
| `POST /repositories/{repository}/leases/{lease}` | 原主体续期，返回同仓库、快照和集合的证据 |
| `DELETE /repositories/{repository}/leases/{lease}` | 幂等释放原租约 |

一个批次的固定条目数最多 4096，完整索引条目数组最多 32 MiB。客户端发布日志同样受 32 MiB 限制。批次回执允许 32 MiB 加 64 KiB，其他控制 JSON 上限为 16 MiB；上传包体由配置的制品上限控制。

## 管理动作

目录与历史：`GET /admin/api/v1/repositories/{repository}/packages`、`GET .../packages/{name}/{version}/revisions?before=<cursor>`。后者从新到旧返回 items 和可选 next。

逻辑管理先 `POST /admin/api/v1/hosted/previews`，再 `POST .../hosted/previews/{id}/commit`，提交已确认 fingerprint 和 idempotency_key。动作支持 remove、yank、unyank、rollback；回退目标必须属于精确坐标。

物理清除先 `POST /admin/api/v1/deletion-previews`，再 `POST .../deletion-previews/{id}/commit` 确认 fingerprint。受理返回 202 和持久任务，不代表已释放字节；通过 `GET /admin/api/v1/deletions/{id}` 查询进度。预览、权限或保护引用变化时拒绝提交。

这些管理动作面向已认证管理主体；Cargo 客户端不能用发布 token 调用。页面和直接 HTTP 请求接受相同授权裁决。

## 错误与重试

JSON 错误包含稳定 code、安全 message、request_id，并保留 errors.detail；正文 ID 与 x-request-id 响应头一致。不得依据英文提示文本推断状态。

| 状态 | 含义与处理 |
| --- | --- |
| 400 | 请求结构、名称、版本、游标或能力组合无效；调整输入 |
| 401 | 身份无效；Cargo registry 声明 Cargo challenge，Admin 不声明 |
| 403 | 权限、Origin 或 CSRF 拒绝；不会作为依赖故障重试 |
| 404 | 资源不存在，或未知管理路径 |
| 405 | 方法不受支持，管理 API 使用同一错误体并保留 Allow |
| 409 | HEAD、幂等键、仓库策略、重复账户或保护引用冲突；复核当前状态 |
| 410 | 精确历史修订已经永久撤销 |
| 413 | 上传或固定批次上限超出；明确旧意图终态后缩小输入 |
| 422 | 归档或元数据语义无效 |
| 429 | 认证并发或受管资源配额不足 |
| 502 | 上游认证、授权或其他回源错误；不等同于调用方 Naxus 凭据失效 |
| 503 | 排空、数据库、完整性依赖或暂时不可用 |
| 507 | 数据卷容量不足 |

错误响应为 no-store，不回显口令、CSRF、SQL 或内部路径。未知管理 GET/HEAD 为 404，同源未知写路径也是 404；写请求仍先服从排空和 Origin 门禁。

索引及逻辑 HEAD 使用 ETag；固定快照和对象使用不可变内容身份。保留租约或物化已经失效时不能用旧成功响应维持可用性。上游切换和冷却的完整分类见[配置说明](configuration.md)。
