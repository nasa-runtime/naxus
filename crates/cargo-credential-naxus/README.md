# cargo-credential-naxus

Naxus 的 Cargo credential provider。根据项目配置中的服务身份和来源映射，为 hosted、固定快照、proxy 及解析 session 提供同一账号的独立 token，避免为每个快照 URL 保存一份凭据。

与 `cargo-naxus` 一同安装到 PATH，由客户端生成的 Cargo 配置调用。凭据读取顺序为 `NAXUS_TOKEN`、`service.token_file`；文件必须仅当前用户可读。provider 不将令牌持久化到 Cargo credentials，不向未映射来源提供凭据。

要求 Cargo 1.97 或更新版本。远程入口使用 HTTPS，回环本机入口允许 HTTP。更改服务 UUID 或来源映射后应重新执行 configure，不能用手工替换身份绕过校验。

许可证可选择 [MIT](https://github.com/nasa-runtime/naxus/blob/main/crates/cargo-credential-naxus/LICENSE-MIT) 或 [Apache-2.0](https://github.com/nasa-runtime/naxus/blob/main/crates/cargo-credential-naxus/LICENSE-APACHE)。
