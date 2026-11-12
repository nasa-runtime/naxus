# naxus-store

Naxus 单实例持久化存储。mutable 仓库允许相同版本新内容形成新修订；历史快照保留原始索引和对象。immutable 仓库保持包体及依赖语义，允许 yank 状态影响后续快照。SQLite 元数据通过 NASA SQLite Mapper trait 访问，与本地内容寻址对象共同构成仓库，不需要独立数据库服务。

发布路径使用 `RepositoryFailure` 区分状态冲突、授权拒绝和容量不足；`PublishBatchTooLarge` 表达固定发布结构上限，`ResourceLimitExceeded` 表达受管资源配额。调用者按错误类型决定协议行为，不应解析包名或诊断文字。

控制请求使用 `RepositoryFailure::InvalidRequest` 表达确定性输入无效，`NotFound` 表达资源不存在；附加错误 context 仍保留类型。session 和保留租约的未知、过期及归属不符统一按不存在拒绝，不泄露他人保护状态，也不延长失效租约。租约释放仍为幂等操作。

管理入口在 store 边界分类用户名、包名、版本、游标、token 能力和动作范围。非法输入及跨仓快照选择使用 `InvalidRequest`，超出账户范围的 token 使用 `Forbidden`，重复用户名、已撤销历史的保留请求及清除保护阻挡使用 `Conflict`。管理选集超限使用 `ResourceLimitExceeded`，真实持久化或完整性失败保留内部依赖错误。这些领域拒绝不创建账户、令牌、预览、保留点或清除任务，也不改变 HEAD。

## 运行架构与 SQL

```text
业务调用（阻塞执行域）
  → 对象生命周期门 / 元数据互斥
  → 固定 Mapper 命令
  → 宿主运行时中的单一消费任务
  → NASA 环境事务 → 单连接 SQLite
```

全部业务 SQL 与 schema 变更集中在 `statements.rs` 的 Mapper trait，通过 `#[Query]`、`#[Insert]`、`#[Update]`、`#[Delete]`、`#[Execute]` 执行。存储方法只选择固定接口和绑定值，不提供任意 SQL 入口。BLOB 与 NULL 分别传递，超出 SQLite 有符号整数范围的值拒绝写入或解码。

Mapper 绑定 `datasources.naxus` 并关闭查询缓存，HEAD、权限、租约与清除证据按当前事务读取。查询只在完整结果集满足行数与字节预算后交付；超限返回错误，不向业务交付部分结果。

## 宿主生命周期

服务端先调用 `Store::prepare` 取得数据卷实例权威，再由 NASA Application 的 `db` 组件创建 `datasources.naxus`，最后通过 `Store::open_managed` 绑定同一数据库。关闭责任由 Application 持有。实例锁同时绑定到 SQLite 文件 owner，启动回滚、调用取消和运行时退出都不能早于原生连接收口交接对象目录。独立使用 `Store::open` 时，调用者必须提供正在运行的 Tokio 多线程运行时，并在 `spawn_blocking` 或已进入该运行时的外部同步线程中调用；组件不创建第二个运行时。独立来源在 Store 释放时等待原生关闭。

元数据串行消费任务始终在同一个 NASA 事务上下文中处理命令。显式提交等待物理确认；作用域提前退出请求回滚。事务超期后拒绝其余命令，直到该业务事务结束，后续新操作才可重新开始。调用者不能在异步 worker 中直接执行同步存储 API。

结构升级兼容已有 `user_version` 进度，后续进度写入 `metadata` 的 `schema_revision` 键。结构、进度及首次服务身份按事务提交。未知进度、物理结构异常或外键缺失拒绝开放；旧数据的 UUID、快照、账号及对象摘要保持原值。

## 持久化顺序

原始制品在同盘 staging 流式校验大小、容量及 SHA-256，文件与目录同步后安装正式对象。SQLite 使用 WAL/FULL，原子提交修订、快照、审计、批次终态及 HEAD；预期 HEAD 和幂等身份防止并发覆盖及重试产生歧义。每 32 层以内产生完整读取检查点。

批次 API 检查固定 hosted 依赖闭包；底层 `commit` 面向已完成协议、权限和归档检查的可信调用者，不代替入站授权。同步接口必须放入有界阻塞执行域。

入站发布通过 `install_validated` 在私有暂存上检查归档、长度和来源，通过后才进入正式对象区；直接安装 API 面向已验证字节的可信调用者。`open_repository_object` 在同一生命周期门禁内核对同仓有效引用并登记活动读取，单个撤销不会隐藏仍有效的共享摘要。

## 历史与清除

提供 fixed snapshot 解析、可续期保留租约、活动读取守卫、逻辑移除、yank、回退与物理清除预览/持久任务。HEAD、保留引用、有效租约、活动读者和共享来源限制清除。对象按存储代次执行，旧任务不能处理重装代次；合法历史撤销返回 `ArtifactGone`。

`revision_history_page` 按精确坐标从新到旧分页，每页最多二百条；返回的排他游标允许在新修订发布后继续读取更早历史。

启动恢复批次和清除阶段再核验数据库与对象。未经授权的丢失或损坏拒绝打开。后台调用 `maintain` 分批回收短期元数据、checkpoint WAL、观察容量并轮转核验对象，不默认删除历史制品。

公开 proxy 保存原始索引、每镜像条件证据、固定 session、已核验对象及物化集合。账号、会话、受限 token、权限与审计保存在同一数据库；凭据校验摘要不包含原始 token。

Admin 变更入口接收完整 `AccessIdentity`，在同一数据库事务内复验账号启用状态、口令与授权代际、角色及仓库范围。调用者不能用主体 UUID 替代该权限证据，也不能继续使用换密或授权变化前取得的身份。

## 备份与运行

`backup` 使用 NASA 原生一致备份及独立对象副本，`restore` 在新目录核验完整数据集后公开，数据库备份为独立 DELETE 文件，恢复通过组件明确转换成全新 WAL 文件，稳定身份保持原值。备份包含敏感账号元数据，应由受限运维保护。组件不提供多实例写入、在线覆盖恢复或高可用。

结构升级前应保存完整备份。恢复要求目标目录不存在；回退程序时使用对应程序能够识别的升级前备份，不能仅替换二进制后继续写入已升级的数据根。

## 配置与观测

默认单制品 256 MiB、保留空闲空间 2 GiB、SQLite 等待五秒、快照深度预算 1024。独立来源默认单连接，单次 SQL 三十秒、事务五分钟，结果最多一百万行或 256 MiB；超过预算明确失败。服务端由 `datasources.naxus` 显式配置这些上限。数据根由调用者传入，含 objects、staging、trash、SQLite/WAL/SHM 与实例锁，须整体持久保存。

来源必须使用 `driver: sqlite`、WAL、FULL、外键及可写模式，数据库文件与对象根必须属于同一仓库。容量不足拒绝新增写入，已有完整对象仍可读取；来源身份或完整性异常会阻断使用，不能通过普通探针解除。连接、SQL、维护与隔离状态由 NASA `db` 组件登记，启用观测出口后可从同一服务端口读取指标；完整配置见[配置说明](https://github.com/nasa-runtime/naxus/blob/release/docs/configuration.md)。

数据根归运行账号所有，Unix 权限为 0700；程序会收紧已有数据根权限并持久化新建目录链，拒绝末级符号链接。

一个数据根只允许一个活动写入者，使用进程内门禁及 OS 锁。支持 Linux 本地 ext4/XFS/Btrfs/F2FS/overlayfs、macOS APFS/HFS+；Docker Desktop 使用 Linux 原生 named volume，不支持宿主目录共享数据挂载。Windows 及其他文件系统不支持。

发布批次的固定条目数或元数据大小超限返回 `PublishBatchTooLarge`，调用方需要缩小发布意图。受管资源配额耗尽返回 `ResourceLimitExceeded`；协议层根据错误类型分类，不解析本地诊断文字。批次拒绝不改变此前已接受条目和 HEAD。

许可证可选择 [MIT](https://github.com/nasa-runtime/naxus/blob/release/crates/naxus-store/LICENSE-MIT) 或 [Apache-2.0](https://github.com/nasa-runtime/naxus/blob/release/crates/naxus-store/LICENSE-APACHE)。
