# GitHub 与 Docker Hub 交付

## 源码仓库

公开入口为 `https://github.com/nasa-runtime/naxus`，默认分支 `release`。开发提交进入 `master`，发布时合并到 `release` 并创建对应产品标签；当前产品版本与源码标签均为 `1.0.0`。源码包含服务、两个客户端、库组件、双许可证、YAML 和部署文档；令牌、数据根、日志和本机运行材料不进入仓库。

```sh
git clone https://github.com/nasa-runtime/naxus.git
cd naxus
cargo build --workspace --locked
```

README 首屏说明同版本发布与公开缓存，独立架构章节描述持久化和构建顺序。[首次接入](quickstart.md)、[客户端](client.md)、[配置](configuration.md)、[接口](protocol.md)、[运维](operations.md)和[容器](../docker/README.md)分别覆盖对应使用边界。库 rustdoc 复用各组件 README。

GitHub 仓库公开源码不等于上传 crates.io 或 Docker Hub；这些入口使用各自凭据和发布权限。源码 push 只触发 Rust 构建流程，镜像上传需显式运行 Publish Docker image。

## 构建产物

源码使用 Cargo workspace，服务与两个客户端在同一提交构建。GitHub Actions 的 Rust workflow 支持 master/release 分支 push、pull request 和手动运行，覆盖 Linux/macOS、最低 Rust 1.97 和 stable。默认分支的 push 或手动运行还生成 Linux amd64、arm64 二进制归档及对应服务镜像归档；产品标签绑定已合并的提交，不单独重复触发构建。

二进制归档包含 `naxus`、`cargo-naxus`、`cargo-credential-naxus`、许可证、README、配置与部署文档，并提供 SHA256SUMS。直接使用归档中的可执行文件时不需要重新编译；运行 Cargo 子命令的客户端仍需要本机安装 Cargo 1.97 或更新版本。服务镜像只包含服务运行内容，不包含 Rust 工具链或开发账号。

镜像由同次构建的服务二进制装配，流程核对平台、UID、OCI 标签，并逐字节比较镜像内程序和归档程序。镜像归档与二进制归档在 GitHub Actions 保留十四天；超过保留期后需对目标提交重新运行构建。

从 GitHub Actions 的成功 Rust run 下载 `binaries-linux-amd64` 或 `binaries-linux-arm64`，在归档目录使用 `sha256sum --check SHA256SUMS` 核对后解压。Linux 归档使用静态 musl 程序，无需 GNU 动态加载器；数据根仍需满足受支持的本地文件系统合同。它不是 Windows/macOS 程序，macOS 从源码安装。

工作流不自动创建 GitHub Release。维护者需要长期分发时，将成功 run 的二进制归档、SHA256SUMS 和对应源码 SHA 一起附到明确版本的 Release；不能用之后重建的文件替换已经说明的内容身份。

## 仓库设置

启用 GitHub Actions，并设置默认分支保护。创建名为 `release` 的 GitHub environment，按团队要求配置发布审批。Docker Hub 账号须具有目标仓库的推送及概述更新权限。

启用仓库 Private Vulnerability Reporting，让 SECURITY.md 中的私有报告入口可用。Issue 使用问题模板接收脱敏环境与重现步骤；发布账号、仓库管理权限和 secrets 按团队分工限制。

| 配置 | 位置 | 含义 |
| --- | --- | --- |
| `DOCKERHUB_USERNAME` | Repository variable | Docker Hub 登录账号 |
| `DOCKERHUB_NAMESPACE` | Repository variable，可选 | 组织或用户 namespace；省略时使用登录账号 |
| `DOCKERHUB_TOKEN` | Repository 或 release environment secret | Docker Hub 凭据，不写入 YAML 或源码 |

源码的 repository 元数据与 OCI source 使用 `https://github.com/nasa-runtime/naxus`；迁入其他组织前同步调整这些公开身份。Docker Hub namespace 由变量控制，不与源码组织名强绑定。

Docker Hub 需预先建立公开的 `<namespace>/naxus` 仓库。发布 token 具有该仓库 push 和概述更新能力；不授予应用容器该 token。`docker/README.md` 是 Hub 完整概述的来源，上传时其中的相对文档链接固定到精确 GitHub 提交。

## 镜像上传

在默认分支运行成功的 Rust workflow 中，取得 run ID，以及步骤摘要中的 amd64、arm64 **config digest**。它们与 manifest digest、manifest list digest 表达不同内容，不能互换。

手动运行 Publish Docker image workflow，输入该 run ID 和两个 `sha256:` config digest。流程要求构建事件可信、构建已成功、工作流提交等于默认分支远端提交，并将镜像身份绑定到该次成功构建的源码 SHA；只加载该次归档，不重新编译。随后核对镜像层与运行配置，将两个平台原样上传并生成版本清单。

如果源码与当前工作流提交不同，仅允许祖先提交之后的 `.github/workflows/docker-publish.yml` 和 `docs/delivery.md` 差异；任意产品输入变化都要求重新构建。镜像标签、OCI revision 和概述中的源码链接始终使用原始构建提交。发布使用 GitHub Actions 的仓库令牌读取构建归档，使用 Docker Hub Token 上传镜像和更新概述，无需额外安装 GitHub App。

已存在的版本只能继续完成相同内容的交付，不能用不同配置摘要替换。流程更新并回读 Docker Hub 中英文概述，复验远端两平台配置摘要后才指向 latest，再确认 latest 与版本清单字节一致。部署者应固定版本或内容摘要，避免把 latest 当作可复现入口。

workflow 文件定义发布机制；上传是否获准由仓库权限与 release environment 控制。没有配置目标仓库和凭据时，本地构建仍可完成，流程不会自动创建远端仓库。

## 内容身份与恢复边界

源码提交、Cargo 包版本、镜像 config digest、平台 manifest digest 和多平台 index digest 分别表达不同身份。业务依赖的同版本覆盖能力不意味着 Naxus 自身公开版本、crates.io 归档或镜像版本标签可以任意换内容。

源提交变化后重新构建并取得对应输入，不能复用旧 run 或旧镜像摘要。上传只完成一部分时，流程允许相同源码和摘要继续完成；已存在版本身份不同则停止，并选择新的产品版本。修改 README 后也要重新生成并读取最终归档，不能把工作区文档当作已有远端内容。
