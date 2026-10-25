# naxus-store

Naxus 单实例持久化存储。mutable 仓库允许相同版本新内容形成新修订；历史快照保留原始索引和对象。immutable 仓库保持包体及依赖语义，允许 yank 状态影响后续快照。SQLite 元数据与本地内容寻址对象共同构成仓库，不需要独立数据库服务。

发布路径使用 `RepositoryFailure` 区分状态冲突、授权拒绝和容量不足；`PublishBatchTooLarge` 表达固定发布结构上限，`ResourceLimitExceeded` 表达受管资源配额。调用者按错误类型决定协议行为，不应解析包名或诊断文字。

控制请求使用 `RepositoryFailure::InvalidRequest` 表达确定性输入无效，`NotFound` 表达资源不存在；附加错误 context 仍保留类型。session 和保留租约的未知、过期及归属不符统一按不存在拒绝，不泄露他人保护状态，也不延长失效租约。租约释放仍为幂等操作。

管理入口在 store 边界分类用户名、包名、版本、游标、token 能力和动作范围。非法输入及跨仓快照选择使用 `InvalidRequest`，超出账户范围的 token 使用 `Forbidden`，重复用户名、已撤销历史的保留请求及清除保护阻挡使用 `Conflict`。管理选集超限使用 `ResourceLimitExceeded`，真实持久化或完整性失败保留内部依赖错误。这些领域拒绝不创建账户、令牌、预览、保留点或清除任务，也不改变 HEAD。

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

`backup` 使用 SQLite 一致备份及独立对象副本，`restore` 在新目录核验完整数据集后公开，稳定身份保持原值。备份包含敏感账号元数据，应由受限运维保护。组件不提供多实例写入、在线覆盖恢复或高可用。

默认单制品 256 MiB、保留空闲空间 2 GiB、SQLite 等待五秒、快照深度预算 1024。数据根由调用者传入，含 objects、staging、trash、SQLite/WAL/SHM 与实例锁，须整体持久保存。

数据根归运行账号所有，Unix 权限为 0700；程序会收紧已有数据根权限并持久化新建目录链，拒绝末级符号链接。

一个数据根只允许一个活动写入者，使用进程内门禁及 OS 锁。支持 Linux 本地 ext4/XFS/Btrfs/F2FS/overlayfs、macOS APFS/HFS+；Docker Desktop 使用 Linux 原生 named volume，不支持宿主目录共享数据挂载。Windows 及其他文件系统不支持。

发布批次的固定条目数或元数据大小超限返回 `PublishBatchTooLarge`，调用方需要缩小发布意图。受管资源配额耗尽返回 `ResourceLimitExceeded`；协议层根据错误类型分类，不解析本地诊断文字。批次拒绝不改变此前已接受条目和 HEAD。

许可证可选择 [MIT](https://github.com/nasa-runtime/naxus/blob/main/crates/naxus-store/LICENSE-MIT) 或 [Apache-2.0](https://github.com/nasa-runtime/naxus/blob/main/crates/naxus-store/LICENSE-APACHE)。
