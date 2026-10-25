use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use nasa::application::DependencyState;
use tokio_util::sync::CancellationToken;

use crate::ServerState;

/// 业务作用：由 NASA 持有唯一维护任务，持续完成已受理删除、控制记录过期、容量观测及轮转核验。
/// 参数说明：
/// - `state`：实例资源与只读就绪贡献。
/// - `cancel`：NASA 唯一停机权威。
///
/// 返回：取消时等待当前有界存储操作结束；工作者丢失或任务异常交由框架关键任务策略处理。
pub(crate) async fn run(state: Arc<ServerState>, cancel: CancellationToken) -> Result<()> {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut cursor = String::new();
    let mut last_verified: Option<Instant> = None;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                state.draining.store(true, std::sync::atomic::Ordering::Release);
                return Ok(());
            }
            _ = interval.tick() => {}
            _ = state.maintenance_wake.notified() => {}
        }
        let store = state
            .store
            .get()
            .cloned()
            .context("repository maintenance has no store")?;
        let permit = match state.work.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => continue,
        };
        let verify = last_verified.is_none_or(|last| last.elapsed() >= Duration::from_secs(60));
        let after = verify.then(|| cursor.clone());
        let operation = tokio::task::spawn_blocking(move || -> Result<_> {
            let _permit = permit;
            let mut deletion_failed = false;
            store.collect_unpublished()?;
            if let Some(id) = store.pending_deletion()? {
                if let Err(error) = store.run_deletion(&id) {
                    // 持久阶段不回滚，先记录退避，再让其他到期任务获得执行机会。
                    store.defer_deletion(&id, &error)?;
                    deletion_failed = true;
                }
            }
            Ok((
                store.maintain(after.as_deref())?,
                store.pending_deletion()?.is_some(),
                deletion_failed,
            ))
        });
        // 阻塞操作拥有独立预算，取消时仍等待其到达持久边界，不能遗留无所有者的文件动作。
        let outcome = operation
            .await
            .context("repository maintenance worker failed")?;
        match outcome {
            Ok((status, pending, deletion_failed)) => {
                use std::sync::atomic::Ordering;
                if deletion_failed {
                    state
                        .metrics
                        .deletion_failures
                        .fetch_add(1, Ordering::Relaxed);
                    tracing::error!("持久清除未完成，已保留失败分类并进入退避");
                }
                state
                    .metrics
                    .free_bytes
                    .store(status.free_bytes, Ordering::Relaxed);
                state
                    .metrics
                    .reserve_bytes
                    .store(status.reserve_free_bytes, Ordering::Relaxed);
                state
                    .metrics
                    .writable
                    .store(u64::from(status.writable), Ordering::Relaxed);
                if verify {
                    cursor = status.next_verified_digest.clone();
                    last_verified = Some(Instant::now());
                }
                state
                    .read_readiness
                    .get()
                    .context("read readiness missing")?
                    .observe(DependencyState::Ready, "repository_healthy", Instant::now());
                state
                    .write_readiness
                    .get()
                    .context("write readiness missing")?
                    .observe(
                        if status.writable {
                            DependencyState::Ready
                        } else {
                            DependencyState::Degraded
                        },
                        if status.writable {
                            "storage_writable"
                        } else {
                            "storage_reserve_reached"
                        },
                        Instant::now(),
                    );
                *state
                    .storage_status
                    .write()
                    .map_err(|_| anyhow::anyhow!("storage observation unavailable"))? =
                    Some(status);
                if pending {
                    state.maintenance_wake.notify_one();
                }
            }
            Err(_) => {
                state
                    .read_readiness
                    .get()
                    .context("read readiness missing")?
                    .observe(
                        DependencyState::NotReady,
                        "repository_maintenance_failed",
                        Instant::now(),
                    );
                tracing::error!("仓库维护未达到完整持久边界，保留任务阶段并等待下一次恢复");
            }
        }
    }
}
