# naxus-server

NASA Application 管理的常驻 Cargo 仓库服务。唯一 listener 承载 sparse index、流式对象、原生 Cargo publish/yank、工作区批次、解析/物化扩展 API 和同源 Admin；保留 Cargo 原始状态与二进制正文。

初始化独占本地数据根并恢复存储，NASA 持有信号、受管维护任务、readiness 和统一指标。YAML、日志、密码学、浏览器身份合同及授权快照复用 NASA，不创建第二个运行时或 listener。

公开来源按配置顺序回源，保存原始索引和校验包体。固定 session、条件请求、同源镜像视图、DNS/重定向门禁与独立并发预算限制来源混淆及回源放大。当前只支持公开且无需认证的 HTTP sparse 上游。

索引、下载配置及包体共用回源结果分类：404、网络可用性失败及 5xx 允许切换；401/403、TLS 与完整性失败停止。429 按 Retry-After 进入该配置上游的有界冷却，期间立即跳过，不阻塞业务任务。每上游事件、切换原因和剩余冷却通过统一指标出口观测；具体配置和稳定错误码见[配置说明](https://github.com/nasa-runtime/naxus/blob/main/docs/configuration.md)。

Admin 负责账号、强制首次换密、仓库范围、读取/发布 token、撤销、审计、包历史、逻辑管理和持久清除。Cookie 不等同于 Cargo token，写请求要求 Origin 与 CSRF；物理清除只由有权限管理主体受理。远程使用 managed 认证及 HTTPS 反向代理，无认证模式仅允许回环部署。

Admin 与扩展 API 共用稳定错误体和 NASA 已校验的请求关联 ID，正文 `request_id` 与 `x-request-id` 一致；不存在仓库与权限拒绝分别返回 404、403。浏览器会话失效时停止在途请求与任务轮询，清空旧身份及页面并回到登录；403 保留登录态，启动时的 503 或网络故障提供重新连接入口。

Admin 的非法名称、版本、游标和能力组合返回 400，超出账户仓库范围返回 403，重复账户或清除保护阻挡返回 409。未知 `/admin/` 页面和 API 路径由管理兜底返回统一 404，不声明 Cargo challenge；写请求仍先检查排空与同源边界。registry 路径保持原有 Cargo 认证行为。

日志为 `${NAXUS_LOG_PATH:/usr/local/logs/${application.name}}`，默认原生数据根 `~/.naxus/repository`。Admin 和 registry 共用 `server.port`。组件不提供多节点写入、企业 SSO、Git index 代理或服务端执行归档代码。

许可证可选择 [MIT](https://github.com/nasa-runtime/naxus/blob/main/crates/naxus-server/LICENSE-MIT) 或 [Apache-2.0](https://github.com/nasa-runtime/naxus/blob/main/crates/naxus-server/LICENSE-APACHE)。
