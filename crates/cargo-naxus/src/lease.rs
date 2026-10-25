use std::{
    sync::{mpsc, Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{ensure, Result};
use naxus_core::Resolution;

use crate::remote::Remote;

pub(crate) struct LeaseGuard {
    remote: Remote,
    path: String,
    last_confirmed: Arc<Mutex<Instant>>,
    stop: mpsc::Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl LeaseGuard {
    /// 业务作用：在构建命令存续期间续期已经取得的历史快照保留权。
    /// 参数说明：
    /// - `remote`：已核对身份的服务端连接。
    /// - `resolution`：刚刚成功建立的解析与租约。
    ///
    /// 返回：每分钟续期的有界守卫；线程建立失败时不宣称已持续保护。
    pub fn begin(remote: &Remote, resolution: &Resolution) -> Result<Self> {
        uuid::Uuid::parse_str(&resolution.lease_id)?;
        let path = format!(
            "/api/naxus/v1/repositories/{}/leases/{}",
            resolution.snapshot.repository_id, resolution.lease_id
        );
        // 首次续期给本地计时提供保守起点，不依赖客户端与服务端的系统时钟同步。
        let started = Instant::now();
        remote.renew(&path)?.validate(resolution)?;
        let last_confirmed = Arc::new(Mutex::new(started));
        let confirmed = last_confirmed.clone();
        let connection = remote.clone();
        let resource = path.clone();
        let binding = resolution.clone();
        let (stop, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("naxus-lease".to_owned())
            .spawn(move || {
                let mut interval = Duration::from_secs(60);
                while matches!(
                    receiver.recv_timeout(interval),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    let started = Instant::now();
                    if connection
                        .renew(&resource)
                        .and_then(|proof| proof.validate(&binding))
                        .is_ok()
                    {
                        if let Ok(mut last) = confirmed.lock() {
                            // 已失去的期限不能由后来的成功掩盖；服务端也拒绝复活到期租约。
                            if last.elapsed() >= Duration::from_secs(540) {
                                break;
                            }
                            *last = started;
                        } else {
                            break;
                        }
                        interval = Duration::from_secs(60);
                    } else {
                        interval = Duration::from_secs(10);
                    }
                }
            })?;
        Ok(Self {
            remote: remote.clone(),
            path,
            last_confirmed,
            stop,
            worker: Some(worker),
        })
    }

    /// 业务作用：在发布锁文件或报告构建成功前复验持续保留权。
    /// 参数说明: 无。
    ///
    /// 返回：距离最后确认不足九分钟才成功，预留网络和提交窗口。
    pub fn validate(&self) -> Result<()> {
        let last = self
            .last_confirmed
            .lock()
            .map_err(|_| anyhow::anyhow!("retention lease state unavailable"))?;
        ensure!(
            last.elapsed() < Duration::from_secs(540),
            "retention lease protection was lost; retry the Naxus command"
        );
        Ok(())
    }
}

impl Drop for LeaseGuard {
    /// 业务作用：先停止续期再释放引用，确保命令结束后没有后台线程重新取得保护。
    /// 参数说明: 无。
    ///
    /// 返回：等待最多一次有界续期完成，然后尝试关闭；进程崩溃仍由服务端期限兜底。
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = self.remote.close(&self.path);
    }
}
