# naxus-client

项目单入口配置、凭据来源限制与双锁文件事务。配置只保存服务地址、仓库、稳定服务身份和受保护凭据文件引用，不保存令牌正文。

## 协作方式

`cargo-naxus` 先读取 `ClientConfig`，取得 `ClientGuard` 并恢复已记录的锁事务，再执行远端解析和 Cargo 命令。成功后由 `commit_locks` 持久保存新旧材料与恢复日志，替换 `Cargo.lock` 和 `naxus.lock`。此库不决定发布修订、不发起上游请求，也不管理服务端对象。

恢复只接受持久日志中的新旧摘要；外部修改不会被猜测覆盖。客户端同时持有进程内工作区门禁与 OS 锁，只支持满足本地持久化合同的 Linux/macOS 文件系统。恢复后可清理精确 UUID 无主操作目录，未知名称或符号链接拒绝处理。

凭据提供器只响应同一服务已映射的 hosted、不可变快照及公开 proxy/session 来源；配置不含令牌正文，环境凭据或受限文件由调用者提供。

客户端私有目录 `.naxus` 在 Unix 上使用 0700，并在公开双锁恢复日志前持久化目录链。配置解析失败的诊断不回显配置正文。

许可证可选择 [MIT](https://github.com/nasa-runtime/naxus/blob/main/crates/naxus-client/LICENSE-MIT) 或 [Apache-2.0](https://github.com/nasa-runtime/naxus/blob/main/crates/naxus-client/LICENSE-APACHE)。
