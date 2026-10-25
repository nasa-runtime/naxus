use anyhow::{ensure, Result};
use url::Url;

/// 业务作用：校验服务对外入口的凭据传输边界，内部 HTTP listener 不受此地址合同影响。
/// 参数说明：
/// - `endpoint`：客户端实际使用并接收认证信息的规范服务地址。
///
/// 返回：无嵌入凭据且为 HTTPS 或回环 HTTP 时成功；远程明文和附加查询状态拒绝。
pub fn validate_service_url(endpoint: &Url) -> Result<()> {
    ensure!(
        endpoint.username().is_empty()
            && endpoint.password().is_none()
            && endpoint.query().is_none()
            && endpoint.fragment().is_none(),
        "service URL must not contain credentials or query parameters"
    );
    let local = match endpoint.host() {
        Some(url::Host::Domain(name)) => name == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    ensure!(
        endpoint.scheme() == "https" || (endpoint.scheme() == "http" && local),
        "remote Naxus access requires HTTPS"
    );
    Ok(())
}

/// 业务作用：形成 Cargo 逻辑来源的唯一比较身份，阻止等价 crates.io 配置竞争同一 replacement。
/// 参数说明：
/// - `source`：Git registry URL 或带 `sparse+` 的逻辑索引声明。
///
/// 返回：无凭据、查询与 fragment 的规范身份；sparse 必须有末尾斜线，标准 crates.io 别名合一，其他来源保留协议差异。
pub fn registry_identity(source: &str) -> Result<String> {
    let sparse = source.starts_with("sparse+");
    let url = Url::parse(source.strip_prefix("sparse+").unwrap_or(source))?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "invalid logical registry source"
    );
    // Cargo 将末尾斜线作为 sparse 协议前提；不能保存后再静默改写业务的来源身份。
    ensure!(
        !sparse || (source.ends_with('/') && url.path().ends_with('/')),
        "sparse registry URL must end in a slash"
    );
    let canonical = url.as_str().trim_end_matches('/');
    if (!sparse && canonical == "https://github.com/rust-lang/crates.io-index")
        || (sparse && canonical == "https://index.crates.io")
    {
        return Ok("https://github.com/rust-lang/crates.io-index".to_owned());
    }
    Ok(format!(
        "{}{canonical}",
        if sparse {
            "sparse+"
        } else {
            ""
        }
    ))
}
