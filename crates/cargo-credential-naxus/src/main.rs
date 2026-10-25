use std::{
    io::{BufRead, Read, Write},
    path::PathBuf,
};

use anyhow::{ensure, Context, Result};
use naxus_client::ClientConfig;
use serde_json::{json, Value};

/// 业务作用：按 Cargo credential provider 协议读取受限来源的项目凭据。
/// 参数说明: 无。
///
/// 返回：来源匹配时提供 get 结果；其他 registry 和凭据写入动作明确拒绝。
fn main() -> Result<()> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        arguments.iter().any(|value| value == "--cargo-plugin"),
        "invoke through the Cargo credential-provider protocol"
    );
    let mut output = std::io::stdout().lock();
    writeln!(
        output,
        "{}",
        json!({
            "v": [1]
        })
    )?;
    output.flush()?;
    let mut input = std::io::stdin().lock();
    loop {
        let mut line = String::new();
        let read = input.by_ref().take(65537).read_line(&mut line)?;
        if read == 0 {
            return Ok(());
        }
        ensure!(
            read <= 65536 && line.ends_with('\n'),
            "credential request exceeds protocol limit"
        );
        let request: Value = serde_json::from_str(&line)?;
        let mut provider_arguments = arguments.clone();
        if let Some(values) = request.get("args").and_then(Value::as_array) {
            provider_arguments.extend(values.iter().filter_map(Value::as_str).map(str::to_owned));
        }
        let position = provider_arguments
            .iter()
            .position(|value| value == "--config")
            .context("credential provider requires --config")?;
        let path = PathBuf::from(
            provider_arguments
                .get(position + 1)
                .context("missing provider configuration path")?,
        );
        let config = ClientConfig::load(&path)?;
        let index = request
            .pointer("/registry/index-url")
            .and_then(Value::as_str)
            .context("missing registry URL")?;
        let response = if !config.permits_credential_index(index) {
            json!({
                "Err": {
                    "kind": "url-not-supported"
                }
            })
        } else if request.get("kind").and_then(Value::as_str) != Some("get") {
            json!({
                "Err": {
                    "kind": "operation-not-supported"
                }
            })
        } else if let Some(token) = config.token(&path)? {
            json!({
                "Ok": {
                    "kind": "get",
                    "token": token.as_str(),
                    "cache": "session",
                    "operation_independent": true,
                }
            })
        } else {
            json!({
                "Err": {
                    "kind": "not-found"
                }
            })
        };
        writeln!(output, "{response}")?;
        output.flush()?;
    }
}
