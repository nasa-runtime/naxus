use std::{
    fmt::Write,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use nasa::application::{
    LegacyMetricsSource, MetricDescriptor, MetricKind, MetricSample, MetricValue,
};

/// 固定指标目录，配置名与位置构成有界标签，不包含 crate、账号、请求或上游地址。
#[derive(Default)]
pub(crate) struct Metrics {
    pub index_cache: AtomicU64,
    pub artifact_cache: AtomicU64,
    pub index_requests: AtomicU64,
    pub artifact_requests: AtomicU64,
    pub index_bytes: AtomicU64,
    pub artifact_bytes: AtomicU64,
    pub deletion_failures: AtomicU64,
    pub free_bytes: AtomicU64,
    pub reserve_bytes: AtomicU64,
    pub writable: AtomicU64,
    upstreams: Vec<Arc<crate::upstream::UpstreamRuntime>>,
}

macro_rules! descriptor {
    ($name:ident, $public:literal, $help:literal, $kind:ident, $unit:literal) => {
        static $name: MetricDescriptor = MetricDescriptor {
            name: $public,
            help: $help,
            unit: $unit,
            kind: MetricKind::$kind,
            label_names: &[],
            histogram_bounds: &[],
        };
    };
}

descriptor!(
    INDEX_CACHE,
    "naxus_proxy_index_cache_hits_total",
    "使用持久索引缓存或固定视图的次数。",
    Counter,
    ""
);
descriptor!(
    ARTIFACT_CACHE,
    "naxus_proxy_artifact_cache_hits_total",
    "使用服务端现存完整公开包体的次数。",
    Counter,
    ""
);
descriptor!(
    INDEX_REQUESTS,
    "naxus_upstream_index_requests_total",
    "向镜像发送的索引与下载配置请求次数。",
    Counter,
    ""
);
descriptor!(
    ARTIFACT_REQUESTS,
    "naxus_upstream_artifact_requests_total",
    "向镜像发送的包体请求次数，包含最终未通过校验的响应。",
    Counter,
    ""
);
descriptor!(
    INDEX_BYTES,
    "naxus_upstream_index_bytes_total",
    "从上游实际读取的索引与下载配置正文总字节。",
    Counter,
    "bytes"
);
descriptor!(
    ARTIFACT_BYTES,
    "naxus_upstream_artifact_bytes_total",
    "从上游实际读取的公开归档总字节，包含失败流已读取的部分。",
    Counter,
    "bytes"
);
descriptor!(
    DELETION_FAILURES,
    "naxus_deletion_failures_total",
    "受管删除任务未达到完整持久边界的累计次数。",
    Counter,
    ""
);
descriptor!(
    FREE_BYTES,
    "naxus_storage_free_bytes",
    "最近一次维护观察到的数据卷空闲字节。",
    Gauge,
    "bytes"
);
descriptor!(
    RESERVE_BYTES,
    "naxus_storage_reserve_bytes",
    "数据卷写入必须保留的空闲字节。",
    Gauge,
    "bytes"
);
descriptor!(
    WRITABLE,
    "naxus_storage_writable",
    "最近一次容量观测是否允许继续写入。",
    Gauge,
    ""
);

static UPSTREAM_EVENTS: MetricDescriptor = MetricDescriptor {
    name: "naxus_upstream_events_total",
    help: "每个配置上游的 HTTP 状态及传输或完整性事件累计。",
    unit: "",
    kind: MetricKind::Counter,
    label_names: &["proxy", "upstream", "event"],
    histogram_bounds: &[],
};
static UPSTREAM_FAILOVERS: MetricDescriptor = MetricDescriptor {
    name: "naxus_upstream_failovers_total",
    help: "按固定原因允许转向下一同源镜像的次数。",
    unit: "",
    kind: MetricKind::Counter,
    label_names: &["proxy", "upstream", "reason"],
    histogram_bounds: &[],
};
static UPSTREAM_COOLDOWN: MetricDescriptor = MetricDescriptor {
    name: "naxus_upstream_cooldown_seconds",
    help: "每个配置上游当前剩余的单调时钟冷却秒数。",
    unit: "seconds",
    kind: MetricKind::Gauge,
    label_names: &["proxy", "upstream"],
    histogram_bounds: &[],
};

static DESCRIPTORS: [&MetricDescriptor; 13] = [
    &INDEX_CACHE,
    &ARTIFACT_CACHE,
    &INDEX_REQUESTS,
    &ARTIFACT_REQUESTS,
    &INDEX_BYTES,
    &ARTIFACT_BYTES,
    &DELETION_FAILURES,
    &FREE_BYTES,
    &RESERVE_BYTES,
    &WRITABLE,
    &UPSTREAM_EVENTS,
    &UPSTREAM_FAILOVERS,
    &UPSTREAM_COOLDOWN,
];

impl Metrics {
    /// 业务作用：绑定启动时已枚举的上游观测状态，禁止请求驱动标签增长。
    /// 参数说明：
    /// - `upstreams`：与服务回源门禁共享的固定配置集合。
    ///
    /// 返回：统一指标源，最多观测配置允许的三十二个来源各十六个物理上游。
    pub fn new(upstreams: Vec<Arc<crate::upstream::UpstreamRuntime>>) -> Self {
        Self {
            upstreams,
            ..Self::default()
        }
    }
    /// 业务作用：为管理总览提供与统一指标出口相同的低基数计数。
    /// 参数说明: 无。
    ///
    /// 返回：缓存命中、回源次数与字节累计，不包含凭据或来源地址。
    pub fn overview(&self) -> serde_json::Value {
        serde_json::json!({
            "index_cache_hits": self.index_cache.load(Ordering::Relaxed),
            "artifact_cache_hits": self.artifact_cache.load(Ordering::Relaxed),
            "upstream_index_requests": self.index_requests.load(Ordering::Relaxed),
            "upstream_artifact_requests": self.artifact_requests.load(Ordering::Relaxed),
            "upstream_index_bytes": self.index_bytes.load(Ordering::Relaxed),
            "upstream_artifact_bytes": self.artifact_bytes.load(Ordering::Relaxed),
        })
    }
}

impl LegacyMetricsSource for Metrics {
    /// 业务作用：启动期间固定全部指标族，交给 NASA 的唯一进程目录执行冲突检查。
    /// 参数说明: 无。
    ///
    /// 返回：固定目录；上游标签仅取自已验证的启动配置与固定事件集合。
    fn descriptors(&self) -> &'static [&'static MetricDescriptor] {
        &DESCRIPTORS
    }

    /// 业务作用：以无 I/O 的原子采样提供当前进程累计与最近容量观测。
    /// 参数说明: 无。
    ///
    /// 返回：基础计数及有界配置上游样本，采集不占用 SQLite 或对象生命周期锁。
    fn snapshot(&self) -> Option<Vec<MetricSample>> {
        let counters = [
            &self.index_cache,
            &self.artifact_cache,
            &self.index_requests,
            &self.artifact_requests,
            &self.index_bytes,
            &self.artifact_bytes,
            &self.deletion_failures,
            &self.free_bytes,
            &self.reserve_bytes,
            &self.writable,
        ];
        let mut samples = DESCRIPTORS
            .iter()
            .zip(counters)
            .map(|(descriptor, counter)| MetricSample {
                name: descriptor.name,
                labels: Vec::new(),
                value: match descriptor.kind {
                    MetricKind::Counter => MetricValue::Counter(counter.load(Ordering::Relaxed)),
                    _ => MetricValue::Gauge(counter.load(Ordering::Relaxed) as f64),
                },
            })
            .collect::<Vec<_>>();
        for upstream in &self.upstreams {
            let labels = vec![
                ("proxy", upstream.proxy.clone()),
                ("upstream", upstream.position.to_string()),
            ];
            for (event, counter) in crate::upstream::EVENTS.iter().zip(&upstream.events) {
                let mut labels = labels.clone();
                labels.push(("event", (*event).to_owned()));
                samples.push(MetricSample {
                    name: UPSTREAM_EVENTS.name,
                    labels,
                    value: MetricValue::Counter(counter.load(Ordering::Relaxed)),
                });
            }
            for (reason, counter) in crate::upstream::FAILOVERS.iter().zip(&upstream.failovers) {
                let mut labels = labels.clone();
                labels.push(("reason", (*reason).to_owned()));
                samples.push(MetricSample {
                    name: UPSTREAM_FAILOVERS.name,
                    labels,
                    value: MetricValue::Counter(counter.load(Ordering::Relaxed)),
                });
            }
            samples.push(MetricSample {
                name: UPSTREAM_COOLDOWN.name,
                labels,
                value: MetricValue::Gauge(upstream.remaining()),
            });
        }
        Some(samples)
    }

    /// 业务作用：为 NASA 兼容文本出口提供同一组样本，不建立第二个指标目录。
    /// 参数说明：
    /// - `output`：共享的 Prometheus 文本缓冲。
    ///
    /// 返回：只追加本组件的有限样本。
    fn render_prometheus(&self, output: &mut String) {
        for sample in self.snapshot().unwrap_or_default() {
            let _ = write!(output, "{}", sample.name);
            if !sample.labels.is_empty() {
                let _ = write!(output, "{{");
                for (index, (name, value)) in sample.labels.iter().enumerate() {
                    if index > 0 {
                        let _ = write!(output, ",");
                    }
                    let _ = write!(output, "{name}=\"{value}\"");
                }
                let _ = write!(output, "}}");
            }
            match sample.value {
                MetricValue::Counter(value) => {
                    let _ = writeln!(output, " {value}");
                }
                MetricValue::Gauge(value) => {
                    let _ = writeln!(output, " {value}");
                }
                _ => {}
            }
        }
    }
}
