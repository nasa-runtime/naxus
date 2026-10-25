# GitHub 与 Docker Hub 交付

## 源码仓库

公开入口为 `https://github.com/nasa-runtime/naxus`，默认分支 `main`。源码包含服务、两个客户端、库组件、双许可证、YAML 和部署文档；令牌、数据根、日志和本机运行材料不进入仓库。

```sh
git clone https://github.com/nasa-runtime/naxus.git
cd naxus
cargo build --workspace --locked
```

README 首屏说明同版本发布与公开缓存，独立架构章节描述持久化和构建顺序。[首次接入](quickstart.md)、[客户端](client.md)、[配置](configuration.md)、[接口](protocol.md)、[运维](operations.md)和[容器](../docker/README.md)分别覆盖对应使用边界。库 rustdoc 复用各组件 README。

GitHub 仓库公开源码不等于上传 crates.io 或 Docker Hub；这些入口使用各自凭据和发布权限。源码 push 只触发 Rust 构建流程，镜像上传需显式运行 Publish Docker image。

## 构建产物

源码使用 Cargo workspace，服务与两个客户端在同一提交构建。GitHub Actions 的 Rust workflow 支持 push、pull request 和手动运行，覆盖 Linux/macOS、最低 Rust 1.97 和 stable。默认分支的 push 或手动运行还生成 Linux amd64、arm64 二进制归档及对应服务镜像归档。

二进制归档包含 `naxus`、`cargo-naxus`、`cargo-credential-naxus`、许可证、README、配置与部署文档，并提供 SHA256SUMS。直接使用归档中的可执行文件时不需要重新编译；运行 Cargo 子命令的客户端仍需要本机安装 Cargo 1.97 或更新版本。服务镜像只包含服务运行内容，不包含 Rust 工具链或开发账号。

镜像由同次构建的服务二进制装配，流程核对平台、UID、OCI 标签，并逐字节比较镜像内程序和归档程序。镜像归档与二进制归档在 GitHub Actions 保留十四天；超过保留期后需对目标提交重新运行构建。

从 GitHub Actions 的成功 Rust run 下载 `binaries-linux-amd64` 或 `binaries-linux-arm64`，在归档目录使用 `sha256sum --check SHA256SUMS` 核对后解压。Linux 归档依赖 glibc，服务镜像提供对应 Debian 运行环境；不将它作为 musl 静态二进制或 Windows/macOS 程序使用。macOS 从源码安装。

工作流不自动创建 GitHub Release。维护者需要长期分发时，将成功 run 的二进制归档、SHA256SUMS 和对应源码 SHA 一起附到明确版本的 Release；不能用之后重建的文件替换已经说明的内容身份。

## 仓库设置

启用 GitHub Actions，并设置默认分支保护。创建名为 `release` 的 GitHub environment，按团队要求配置发布审批。Docker Hub 账号须具有目标仓库的推送及概述更新权限。

启用仓库 Private Vulnerability Reporting，让 SECURITY.md 中的私有报告入口可用。Issue 使用问题模板接收脱敏环境与重现步骤；发布账号、仓库管理权限和 secrets 按团队分工限制。

| 配置 | 位置 | 含义 |
| --- | --- | --- |
| `DOCKERHUB_USERNAME` | Repository variable | Docker Hub 登录账号 |
| `DOCKERHUB_NAMESPACE` | Repository variable，可选 | 组织或用户 namespace；省略时使用登录账号 |
| `DOCKERHUB_TOKEN` | Repository 或 release environment secret | Docker Hub 凭据，不写入 YAML 或源码 |
| `BEHAVIOR_CHECK_APP_ID` | Repository variable，必填 | 独立行为验收 GitHub App 的数字 ID；仅接受该发布者的 Check Run |

源码的 repository 元数据与 OCI source 使用 `https://github.com/nasa-runtime/naxus`；迁入其他组织前同步调整这些公开身份。Docker Hub namespace 由变量控制，不与源码组织名强绑定。

Docker Hub 需预先建立公开的 `<namespace>/naxus` 仓库。发布 token 具有该仓库 push 和概述更新能力；不授予应用容器该 token。`docker/README.md` 是 Hub 完整概述的来源，上传时其中的相对文档链接固定到精确 GitHub 提交。

## 独立运行凭据

`BEHAVIOR_CHECK_APP_ID` 对应由发布者信任、已安装到目标仓库的 GitHub App。该 App 需要创建 Check Run 的权限，并在独立运行环境对精确源码和两架构镜像作出判断。源码仓库的静态构建不会自行签发该凭据，也不会自动部署该 App。

Check Run 名称、发布者、head_sha、完成状态、HTTPS details_url、JSON 合同及两个 config digest 都必须匹配。`suites` 的必需字段为 publication_recovery、resolution_binding、authorization、deletion_recovery、proxy_cache、backup_restore、linux_clients。只有对应工作确已完成时才由可信 App 标为 true；本地文件或同名的其他发布者不能替代这份权威。

## 镜像上传

在默认分支运行成功的 Rust workflow 中，取得 run ID，以及步骤摘要中的 amd64、arm64 **config digest**。它们与 manifest digest、manifest list digest 表达不同内容，不能互换。

手动运行 Publish Docker image workflow，输入该 run ID、两个 `sha256:` config digest，以及 `behavior_check_run_id`。流程要求构建事件可信、构建已成功、源码 SHA 等于本次发布 SHA 和默认分支远端 SHA；只加载该次归档，不重新编译。登录 Docker Hub 前还须回读独立行为凭据，核对发布者、源码和两架构镜像；未配置可信 App 或缺少凭据时拒绝上传。随后核对镜像层与运行配置，将两个平台原样上传并生成版本清单。

Rust workflow 的 format、Clippy、build、rustdoc 和 package 证明静态及打包条件，不替代运行行为凭据。独立 App 应发布名为 `naxus-behavior-acceptance` 的成功 Check Run，并提供 HTTPS `details_url`。其 `output.text` 使用 JSON：`contract` 为 `naxus.behavior-acceptance`，`head_sha` 为精确提交，`images` 包含 `amd64`、`arm64` 的 config digest，`suites` 中 `publication_recovery`、`resolution_binding`、`authorization`、`deletion_recovery`、`proxy_cache`、`backup_restore`、`linux_clients` 必须全部为布尔值 `true`。凭据只对对应源码和镜像有效。

已存在的版本只能继续完成相同内容的交付，不能用不同配置摘要替换。流程更新并回读 Docker Hub 中英文概述，复验远端两平台配置摘要后才指向 latest，再确认 latest 与版本清单字节一致。部署者应固定版本或内容摘要，避免把 latest 当作可复现入口。

workflow 文件定义发布机制；上传是否获准由仓库权限与 release environment 控制。没有配置目标仓库和凭据时，本地构建仍可完成，流程不会自动创建远端仓库。

## 内容身份与恢复边界

源码提交、Cargo 包版本、镜像 config digest、平台 manifest digest 和多平台 index digest 分别表达不同身份。业务依赖的同版本覆盖能力不意味着 Naxus 自身公开版本、crates.io 归档或镜像版本标签可以任意换内容。

源提交变化后重新构建并取得对应输入，不能复用旧 run 或旧镜像凭据。上传只完成一部分时，流程允许相同源码和摘要继续完成；已存在版本身份不同则停止，并选择新的产品版本。修改 README 后也要重新生成并读取最终归档，不能把工作区文档当作已有远端内容。
