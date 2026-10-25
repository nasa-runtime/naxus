/// 业务作用：把仓库的初始化、日志、唯一 Web listener 和停机交给 NASA 应用生命周期。
/// 参数说明：
/// - `app`：尚未开放业务入口的应用容器。
///
/// 返回：仓库资源及路由全部登记成功后继续启动；失败由框架统一清理。
#[nasa::application("log", "web", config = naxus_server::bootstrap_loader)]
async fn main(app: nasa::Application) -> anyhow::Result<()> {
    naxus_server::install(&app)?;
    Ok(())
}
