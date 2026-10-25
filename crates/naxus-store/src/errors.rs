/// 发布意图超过固定结构边界，重试同一上传不能使该批次继续容纳内容。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishBatchTooLarge {
    ItemCount,
    MetadataBytes,
}

impl std::fmt::Display for PublishBatchTooLarge {
    /// 业务作用：向本地调用者说明必须缩小发布意图的固定限制，保留错误类型供协议裁决。
    /// 参数说明：
    /// - `formatter`：不含凭据和本地路径的错误输出目标。
    ///
    /// 返回：对应条目或元数据限制的固定诊断，不表示临时限流。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ItemCount => "publish batch item budget exceeded",
            Self::MetadataBytes => "publish batch metadata budget exceeded",
        })
    }
}

impl std::error::Error for PublishBatchTooLarge {}

/// 受管资源配额已满，HTTP 分类依赖此类型而不解析诊断中的自然语言。
#[derive(Clone, Copy, Debug)]
pub struct ResourceLimitExceeded(pub &'static str);

impl std::fmt::Display for ResourceLimitExceeded {
    /// 业务作用：保留受限资源的本地诊断，同时让上层根据类型稳定识别配额拒绝。
    /// 参数说明：
    /// - `formatter`：本地错误诊断的输出目标。
    ///
    /// 返回：调用点提供的固定资源提示；公开响应由协议层使用独立的安全文案。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for ResourceLimitExceeded {}

/// 控制输入、资源存在性、仓库状态、授权和容量的领域拒绝；公开协议按变体分类。
#[derive(Debug)]
pub enum RepositoryFailure {
    InvalidRequest(&'static str),
    NotFound(&'static str),
    Conflict(String),
    Forbidden(&'static str),
    InsufficientStorage,
}

impl std::fmt::Display for RepositoryFailure {
    /// 业务作用：保留仓库拒绝原因供本地诊断，错误种类独立于包名和文案。
    /// 参数说明：
    /// - `formatter`：服务内部诊断的输出目标。
    ///
    /// 返回：领域失败的诊断文本；公开状态码由调用者按变体确定。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest(reason) | Self::NotFound(reason) => formatter.write_str(reason),
            Self::Conflict(reason) => formatter.write_str(reason),
            Self::Forbidden(reason) => formatter.write_str(reason),
            Self::InsufficientStorage => formatter.write_str("insufficient free storage"),
        }
    }
}

impl std::error::Error for RepositoryFailure {}
