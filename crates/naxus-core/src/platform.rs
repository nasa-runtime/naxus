use anyhow::{ensure, Result};

/// 业务作用：在有界输入与嵌套预算内复用 Cargo 的 target 语法，保护服务解析不可信索引时的调用栈。
/// 参数说明：
/// - `target`：原始命名平台或 cfg 表达式，成功后仍由调用者保留原文。
///
/// 返回：声明不超过 16 KiB、字符串外括号至多嵌套 64 层且 Cargo 可解析时成功，不计算当前平台是否匹配。
pub(crate) fn validate(target: &str) -> Result<()> {
    ensure!(
        target.len() <= 16 * 1024,
        "dependency target exceeds size limit"
    );
    let mut quoted = false;
    let mut depth = 0usize;
    // Cargo 的表达式解析与析构使用递归；先以无递归扫描限制结构深度，字符串内的括号没有结构意义。
    for byte in target.bytes() {
        match byte {
            b'"' => quoted = !quoted,
            b'(' if !quoted => {
                depth += 1;
                ensure!(depth <= 64, "dependency target exceeds nesting limit");
            }
            b')' if !quoted => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    target
        .parse::<cargo_platform::Platform>()
        .map_err(|_| anyhow::anyhow!("invalid dependency target platform"))?;
    Ok(())
}
