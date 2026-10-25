# cargo-naxus

Naxus 的 Cargo 客户端。`cargo naxus publish --workspace --atomic` 通过 Cargo 原生打包和预期 HEAD 原子提交内部组件；开发构建固定不可变仓库快照，让相同 crate 名称和版本的新内容可以被下一次默认构建发现。

要求 Cargo 1.97 或更新版本，以及 Linux/macOS 本地持久文件系统。与 `cargo-credential-naxus` 一同放入 PATH，通过 `cargo naxus configure --endpoint <HTTPS地址> --repository <仓库名>` 配置服务。令牌由 `NAXUS_TOKEN` 或受限 token 文件提供，不写入项目配置。

支持 `build`、`check`、`test`、`run`、`update`、`publish` 和 `install`。默认保持已选 SemVer 并刷新内容；`--locked` 保持双锁固定修订，`--offline --locked` 要求已有完整缓存。`Cargo.lock` 和 `naxus.lock` 应一同提交，持久日志负责两文件间的恢复。

`install` 是发布到 Naxus 的兼容入口，与 `publish` 使用相同流程，不安装本机可执行程序。完整命令与模式见[客户端说明](https://github.com/nasa-runtime/naxus/blob/release/docs/client.md)。

私有依赖须显式指定 Naxus registry。公开 registry 由服务映射代理；path 依赖仅支持同一工作区成员。不支持 Git 依赖、`[patch]`、`[replace]` 或 Cargo 配置 include。发布在独立源码副本中进行，不更改业务 manifest 和锁文件。网络、来源或校验失败不提交半完成双锁。

发布归档在首个远端写入前固定到 `.naxus/publications/`，并由 `.naxus/publication.json` 记录服务、主体、预期 HEAD、幂等键和批次阶段。下次在线命令先恢复原操作；发布命令完成恢复后不会接着打包新源码，需要再次执行 publish 才发布后续改动。成功收据保存在 `.naxus/publication-receipt.json`，这些恢复材料不提交 Git。

`cargo naxus publish --abort-pending` 仅终止或确认原批次，不选择新制品，也不删除已提交包。过期或已终止批次须明确确认后才能开始新发布；主体、服务变化或终态不确定时保留现场。存在未恢复发布时拒绝离线构建。

HTTP 413 表示上传或批次超过固定大小边界，重试原输入不能解除限制。客户端保留原发布日志并提示先执行 `cargo naxus publish --abort-pending`，明确结束旧意图后缩小输入再发布；不自动丢弃已有制品或换键重发。资源配额耗尽的 429 保留独立分类。

许可证可选择 [MIT](https://github.com/nasa-runtime/naxus/blob/release/crates/cargo-naxus/LICENSE-MIT) 或 [Apache-2.0](https://github.com/nasa-runtime/naxus/blob/release/crates/cargo-naxus/LICENSE-APACHE)。
