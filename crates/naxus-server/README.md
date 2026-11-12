# naxus-server

NASA Application 管理的常驻 Cargo 仓库服务，提供同版本修订发布、不可变快照读取、公开 registry 缓存和同源 Admin。唯一 listener 承载 sparse index、流式对象、原生 Cargo publish/yank、工作区批次及解析/物化扩展 API，保留 Cargo 原始状态与二进制正文。

## 运行架构与生命周期

`bootstrap_loader` 在数据库建连前取得数据根实例锁并核对配置，Application 的 `db` 组件创建 `datasources.naxus`。异步 `install` 取得同一来源，initializer 在阻塞执行域打开受管 Store，完成结构升级、持久任务恢复和引用校验后才开放服务。配置冲突、未知结构或无法解释的对象缺失都会阻止就绪。

NASA Application 统一管理 listener、信号、受管维护任务和 readiness；`db` 组件持有 SQLite owner、连接观测与关闭责任。实例锁覆盖数据库与对象目录的真实使用周期，原生连接关闭前不能交接数据根。YAML、日志、密码学、浏览器身份合同及授权快照复用 NASA，不创建第二个运行时或 listener。

## 公开来源与缓存

公开来源按配置顺序回源，保存原始索引和校验包体。固定 session、条件请求、同源镜像视图、DNS/重定向门禁与独立并发预算限制来源混淆及回源放大。当前只支持公开且无需认证的 HTTP sparse 上游。

索引、下载配置及包体共用回源结果分类：404、网络可用性失败及 5xx 允许切换；401/403、TLS 与完整性失败停止。429 按 Retry-After 进入该配置上游的有界冷却，期间立即跳过，不阻塞业务任务。每上游事件、切换原因和剩余冷却通过统一指标出口观测；具体配置和稳定错误码见[配置说明](https://github.com/nasa-runtime/naxus/blob/release/docs/configuration.md)。

## Admin 与权限

Admin 负责账号、强制首次换密、仓库范围、读取/发布 token、撤销、审计、包历史、逻辑管理和持久清除。Cookie 不等同于 Cargo token，写请求要求 Origin 与 CSRF；物理清除只由有权限管理主体受理。远程使用 managed 认证及 HTTPS 反向代理，无认证模式仅允许回环部署。

Admin 与扩展 API 共用稳定错误体和 NASA 已校验的请求关联 ID，正文 `request_id` 与 `x-request-id` 一致；不存在仓库与权限拒绝分别返回 404、403。浏览器会话失效时停止在途请求与任务轮询，清空旧身份及页面并回到登录；403 保留登录态，启动时的 503 或网络故障提供重新连接入口。

Admin 的非法名称、版本、游标和能力组合返回 400，超出账户仓库范围返回 403，重复账户或清除保护阻挡返回 409。未知 `/admin/` 页面和 API 路径由管理兜底返回统一 404，不声明 Cargo challenge；写请求仍先检查排空与同源边界。registry 路径保持原有 Cargo 认证行为。

## 配置、观测与边界

`datasources.naxus.driver` 必须为 `sqlite`，数据库路径必须为 `naxus.data.root/naxus.sqlite3`，使用单连接、WAL、FULL 和外键检查。业务及结构 SQL 由存储层 Mapper trait 声明；自定义 YAML 必须包含该 datasource。SQL 或事务预算耗尽时明确失败，不自动重试业务写入；完整设置见[配置说明](https://github.com/nasa-runtime/naxus/blob/release/docs/configuration.md)。

`/healthz` 表示进程存活，`/readyz` 反映受管读取依赖状态；低容量通过写入状态单独表达。显式启用 `grafana.observability` 出口后，`/metrics` 在同一端口提供业务和 SQLite 指标；默认不另启指标端口。SQL 日志不输出绑定参数，指标标签不使用任意包名、用户或 URL。

日志为 `${NAXUS_LOG_PATH:/usr/local/logs/${application.name}}`，默认原生数据根 `~/.naxus/repository`。Admin 和 registry 共用 `server.port`。组件不提供多节点写入、企业 SSO、Git index 代理或服务端执行归档代码。

许可证可选择 [MIT](https://github.com/nasa-runtime/naxus/blob/release/crates/naxus-server/LICENSE-MIT) 或 [Apache-2.0](https://github.com/nasa-runtime/naxus/blob/release/crates/naxus-server/LICENSE-APACHE)。
