use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::{Duration, SystemTime},
};

use anyhow::Result;
use reqwest::StatusCode;
use tokio::time::Instant;

/// 单个物理上游的结果，不把授权与完整性拒绝解释为可恢复的可用性问题。
#[derive(Clone, Copy, Debug)]
pub(crate) enum Failure {
    Missing,
    Unauthorized,
    Forbidden,
    RateLimited,
    CoolingDown,
    Transient,
    InvalidResponse,
}

impl std::fmt::Display for Failure {
    /// 业务作用：向内部诊断提供固定原因，避免地址或凭据进入公共错误。
    /// 参数说明：
    /// - `formatter`：不包含上游正文的诊断输出。
    ///
    /// 返回：可按错误类型裁决的失败说明。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Missing => "upstream resource missing",
            Self::Unauthorized => "upstream authentication rejected",
            Self::Forbidden => "upstream authorization rejected",
            Self::RateLimited => "upstream rate limited",
            Self::CoolingDown => "upstream is cooling down",
            Self::Transient => "upstream transport or service unavailable",
            Self::InvalidResponse => "upstream response is not trusted",
        })
    }
}

impl std::error::Error for Failure {}

/// 固定配置位置的冷却与观测状态；不按 crate 或请求创建新记录。
pub(crate) struct UpstreamRuntime {
    pub proxy: String,
    pub position: usize,
    cooldown: Mutex<Option<Instant>>,
    pub events: [AtomicU64; 8],
    pub failovers: [AtomicU64; 4],
}

pub(crate) const EVENTS: [&str; 8] = [
    "success",
    "not_found",
    "unauthorized",
    "forbidden",
    "rate_limited",
    "transient",
    "integrity",
    "other_status",
];
pub(crate) const FAILOVERS: [&str; 4] = ["not_found", "transient", "rate_limited", "cooldown"];

impl UpstreamRuntime {
    /// 业务作用：为配置中的精确上游建立固定大小的限流状态，不接受动态业务键。
    /// 参数说明：
    /// - `proxy`：逻辑来源配置名。
    /// - `position`：该来源内的上游列表位置。
    ///
    /// 返回：尚未冷却、计数为零的运行状态。
    pub fn new(proxy: &str, position: usize) -> Self {
        Self {
            proxy: proxy.to_owned(),
            position,
            cooldown: Mutex::new(None),
            events: std::array::from_fn(|_| AtomicU64::new(0)),
            failovers: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// 业务作用：在每次网络阶段前复验上游冷却，避免配置和包体绕过索引阶段的限流。
    /// 参数说明: 无。
    ///
    /// 返回：冷却结束或没有限流时成功；未到期立即拒绝，不等待期限流逝。
    pub fn check(&self) -> Result<()> {
        let mut until = self.cooldown.lock().map_err(|_| Failure::InvalidResponse)?;
        if until.is_some_and(|until| until > Instant::now()) {
            return Err(Failure::CoolingDown.into());
        }
        *until = None;
        Ok(())
    }

    /// 业务作用：按 HTTP 状态建立统一的停止、切换或限流结果，所有代理阶段共用。
    /// 参数说明：
    /// - `status`：上游响应状态。
    /// - `retry_after`：可选 Retry-After 原文。
    /// - `maximum`：管理员允许的最大冷却秒数。
    ///
    /// 返回：成功及条件响应继续读取；404、暂时失败和限流允许切换，认证或其他异常停止。
    pub fn response(
        &self,
        status: StatusCode,
        retry_after: Option<&str>,
        maximum: u64,
    ) -> Result<()> {
        let (event, failure) = match status.as_u16() {
            200..=299 | 304 => (0, None),
            404 => (1, Some(Failure::Missing)),
            401 => (2, Some(Failure::Unauthorized)),
            403 => (3, Some(Failure::Forbidden)),
            429 => (4, Some(Failure::RateLimited)),
            500..=599 => (5, Some(Failure::Transient)),
            _ => (7, Some(Failure::InvalidResponse)),
        };
        if event != 5 {
            self.events[event].fetch_add(1, Ordering::Relaxed);
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            let delay = retry_after
                .map(str::trim)
                .and_then(|value| {
                    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
                        Some(Duration::from_secs(value.parse::<u64>().unwrap_or(maximum)))
                    } else {
                        httpdate::parse_http_date(value)
                            .ok()
                            .map(|time| time.duration_since(SystemTime::now()).unwrap_or_default())
                    }
                })
                .unwrap_or(Duration::from_secs(30))
                .min(Duration::from_secs(maximum));
            let next = Instant::now() + delay;
            let mut until = self.cooldown.lock().map_err(|_| Failure::InvalidResponse)?;
            // 并发较短的 Retry-After 不能撤销另一个请求已取得的更长冷却证据。
            *until = Some(until.map_or(next, |current| current.max(next)));
        }
        failure.map_or(Ok(()), |failure| Err(failure.into()))
    }

    /// 业务作用：仅对明确可恢复的失败开放下一镜像，未知校验错误默认停止。
    /// 参数说明：
    /// - `error`：同一上游的网络、状态或静态校验失败。
    ///
    /// 返回：允许继续时累计固定原因；认证、TLS 和内容完整性错误拒绝降级。
    pub fn failover(&self, error: &anyhow::Error) -> bool {
        let reason = match error.downcast_ref::<Failure>() {
            Some(Failure::Missing) => 0,
            Some(Failure::Transient) => {
                self.events[5].fetch_add(1, Ordering::Relaxed);
                1
            }
            Some(Failure::RateLimited) => 2,
            Some(Failure::CoolingDown) => 3,
            Some(Failure::Unauthorized | Failure::Forbidden) => return false,
            _ => {
                self.events[6].fetch_add(1, Ordering::Relaxed);
                return false;
            }
        };
        self.failovers[reason].fetch_add(1, Ordering::Relaxed);
        tracing::warn!(proxy = %self.proxy, upstream = self.position, reason = FAILOVERS[reason], "公开代理尝试下一个同源镜像");
        true
    }

    /// 业务作用：提供单调时钟下的剩余冷却，不暴露上游地址或依赖系统时间跳变。
    /// 参数说明: 无。
    ///
    /// 返回：剩余秒数；无冷却为零，状态不可用按零观测但网络门禁仍拒绝。
    pub fn remaining(&self) -> f64 {
        self.cooldown
            .lock()
            .ok()
            .and_then(|until| *until)
            .map_or(0.0, |until| {
                until
                    .saturating_duration_since(Instant::now())
                    .as_secs_f64()
            })
    }
}

/// 业务作用：区分可恢复传输失败与 TLS 身份验证失败，禁止后者被备用地址掩盖。
/// 参数说明：
/// - `error`：reqwest 的完整来源链。
///
/// 返回：超时、连接及流中断可重试；TLS 或协议解码失败按不可信响应停止。
pub(crate) fn transport(error: reqwest::Error) -> anyhow::Error {
    let tls = untrusted_transport(&error);
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(&error);
    let mut interrupted = false;
    for _ in 0..64 {
        let Some(cause) = current else {
            break;
        };
        if cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
            matches!(
                error.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::TimedOut
            )
        }) {
            interrupted = true;
            break;
        }
        if cause.downcast_ref::<hyper::Error>().is_some_and(|error| {
            error.is_incomplete_message() || error.is_closed() || error.is_canceled()
        }) {
            interrupted = true;
            break;
        }
        current = inner_error(cause);
    }
    if !tls && (error.is_timeout() || error.is_connect() || error.is_body() || interrupted) {
        anyhow::Error::new(Failure::Transient).context(error)
    } else {
        anyhow::Error::new(Failure::InvalidResponse).context(error)
    }
}

/// 业务作用：穿透 I/O 自定义包装检查 TLS 与协议身份错误，避免来源链省略内层证书原因。
/// 参数说明：
/// - `error`：HTTP 客户端的完整错误包装。
///
/// 返回：证书、无效协议数据或过深包装按不可信失败处理；普通网络可用性错误保留重试资格。
fn untrusted_transport(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    for _ in 0..64 {
        let Some(error) = current else {
            return false;
        };
        if error.is::<rustls::Error>() {
            return true;
        }
        if let Some(error) = error.downcast_ref::<std::io::Error>() {
            if matches!(
                error.kind(),
                std::io::ErrorKind::InvalidData | std::io::ErrorKind::PermissionDenied
            ) {
                return true;
            }
        }
        current = inner_error(error);
    }
    true
}

/// 业务作用：在同步存储读取流失败后恢复跨 I/O 包装的回源分类。
/// 参数说明：
/// - `error`：流桥接与暂存操作返回的错误链。
///
/// 返回：已明确分类的上游错误；未知错误不获得镜像切换资格。
pub(crate) fn embedded_failure(error: &anyhow::Error) -> Option<Failure> {
    let mut current = Some(error.as_ref());
    for _ in 0..64 {
        let cause: &(dyn std::error::Error + 'static) = current?;
        if let Some(failure) = cause.downcast_ref::<Failure>() {
            return Some(*failure);
        }
        current = inner_error(cause);
    }
    None
}

/// 业务作用：保留 I/O 自定义错误自身，而非只依赖可能跳过内层错误的 source 实现。
/// 参数说明：
/// - `error`：当前错误包装。
///
/// 返回：自定义 I/O 原因优先，其余采用标准来源链；没有下层时返回 None。
fn inner_error<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a (dyn std::error::Error + 'static)> {
    error
        .downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::get_ref)
        .map(|inner| inner as &(dyn std::error::Error + 'static))
        .or_else(|| error.source())
}
