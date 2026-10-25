# 客户端与锁文件

`cargo-naxus` 负责工作区解析、快照选择、原子发布和原生 Cargo 编排；`cargo-credential-naxus` 根据同一项目配置向受信任索引提供凭据。业务项目需要同时安装两个程序，并保留原生 Cargo。

## 命令

| 命令 | 行为 |
| --- | --- |
| `cargo naxus configure --endpoint <URL> --repository <名称或UUID>` | 核对服务身份并保存工作区入口和来源映射 |
| `cargo naxus build`、`check`、`test`、`run` | 保持已选版本，检查同版本修订，再运行对应 Cargo 命令 |
| `cargo naxus update` | 允许 Cargo 按 manifest 约束重新选择版本并物化来源 |
| `cargo naxus publish --workspace --atomic` | 将工作区选定成员在同一预期 HEAD 上原子提交 |
| `cargo naxus publish -p <package>` | 发布指定成员；未包含的私有工作区依赖须已经存在于目标仓库 |
| `cargo naxus publish --abort-pending` | 确认或终止原未完成批次，不删除已提交包 |
| `cargo naxus install` | `publish` 的兼容入口，发布到 Naxus；不安装可执行文件到本机 |
| `cargo naxus --version`、`--help` | 显示客户端版本或命令入口 |

配置命令支持 `--manifest-path`。发布还支持 `--repository`、`--manifest-path` 和 `--no-verify`；后者只跳过 Cargo 打包时的编译验证，不能跳过服务端归档与来源检查。发布不支持 `--dry-run`、`--offline` 等未声明控制参数；不识别的意图会被明确拒绝。

构建命令接受受支持的原生 Cargo 参数，如 `--release`、`--features`、`--target` 和 `--target-dir`。`--manifest-path` 和 `--target-dir` 相对路径按调用目录固定。`run -- <业务参数>` 的双横线后内容保留原样。

## 配置与凭据

在工作区根目录或其子目录执行客户端，配置从父目录寻找 `.naxus/config.toml`。配置绑定规范 endpoint、服务 UUID、hosted 仓库和逻辑来源映射。再次 configure 可同步同一服务的新映射，不能手动替换身份绕过核对。

凭据读取顺序为 `NAXUS_TOKEN`、`service.token_file`。相对 token 文件路径以客户端配置文件所在目录为基准。凭据文件必须仅当前用户可读；配置只保存路径。提供器不持久保存 token，也不向未知索引、其他服务或上游镜像提供业务凭据。

私有依赖显式使用 `registry = "naxus"`。公开依赖保留原来的逻辑 registry，物理请求经过 Naxus。独立公开来源不与同名私有包或其他 registry 合并。

## 默认、锁定与离线

| 模式 | 版本与内容 | 网络与本机条件 |
| --- | --- | --- |
| 默认命令 | 已有解析输入不变时保持 SemVer，刷新同版本修订；输入变化时允许重新解析 | 在线固定快照、取得租约并物化公开来源 |
| `update` | 在 manifest 约束内重新选择版本 | 在线取得新的来源视图 |
| `--locked` | 保持双锁记录的修订、摘要与来源 | 联网时仍复验服务身份、历史可用性和物化撤销 |
| `--locked --offline` / `--frozen` | 只使用完整双锁和本机缓存 | 不取得服务端租约；缺失缓存、配置不匹配或未完成发布时失败 |

`Cargo.lock` 固定 Cargo 图，`naxus.lock` 记录服务、来源、快照、修订与物化身份。两个文件连同无凭据配置一起提交。单独复制、删除或修改一个锁文件不能作为可靠更新方式；客户端遇到外部编辑会拒绝覆盖。

双锁的新旧材料与摘要先持久保存，再依次替换两个文件。中断后恢复同一新状态或已验证的旧状态；网络、校验或失去租约不能提交半完成双锁。在线构建每分钟续期十分钟租约，退出释放；离线构建只保证已有本机内容，不代表远端历史获得持续保留。

## 发布恢复

在任何远端写入前，客户端将固定归档存入 `.naxus/publications/`，把服务、主体、预期 HEAD、幂等键与批次阶段写入 `.naxus/publication.json`。下次在线命令先恢复同一意图，不重新打包后来改动的源码。成功收据位于 `.naxus/publication-receipt.json`。

恢复 publish 成功后，该命令结束；如需发布恢复期间新增的源码，须再次执行 publish。显式终止不能同时携带新的包选择、仓库选择或打包选项。不能确认终态、服务身份或发布主体时保留原材料。

413 表示固定大小或条目边界超限，重复原输入不会解除限制。先执行 `cargo naxus publish --abort-pending` 明确原意图终态，再缩小输入发布。429 表示受管资源配额，可按服务容量处理。不要手动删除恢复日志或修改全局 Cargo 缓存来跳过这两类拒绝。

## 来源与平台边界

受管工作区不接受 Git 依赖、`[patch]`、`[replace]`、Cargo 配置 `include`、`--config`、`--lockfile-path` 或 `-Z` 来源与锁文件覆盖。path 依赖只支持同一工作区成员；其他独立项目先发布到 Naxus。

客户端仅支持满足本地锁和目录同步合同的 Linux/macOS 文件系统。远程私服使用 HTTPS，回环入口允许 HTTP；不支持 Windows、任意本地源码跨工作区注入或服务端执行 Cargo 构建。
