use super::*;
use crate::loader;
use pi_result::{Error, ErrorKind};

pub(super) unsafe fn terminal_input<T: Send + Sync + 'static>(
    terminal: Arc<T>,
) -> TerminalInput<T> {
    TerminalInput { terminal }
}

pub(super) fn manager_process(build: HostBuild) -> Result<Manager> {
    Authority::process(build).map(|authority| Manager { authority })
}

pub(super) fn manager_find(
    manager: &Manager,
    packages: &[PluginPackage],
    request: &PluginRequest,
) -> Result<PluginPackage> {
    let _ = manager;
    let mut matching = packages.iter().filter(|package| {
        let manifest = package.manifest();
        manifest.uuid == request.uuid && request.version.matches(&manifest.version)
    });
    let best = matching
        .clone()
        .max_by(|left, right| {
            left.manifest()
                .version
                .cmp_precedence(&right.manifest().version)
        })
        .ok_or_else(|| Error::new(ErrorKind::NotFound).attach(request.version.to_string()))?;
    let selected_identity = loader::file_identity(best)?;
    let selected = best.manifest();
    for package in &mut matching {
        let manifest = package.manifest();
        if std::ptr::eq(package, best)
            || !manifest.version.cmp_precedence(&selected.version).is_eq()
        {
            continue;
        }
        if loader::file_identity(package)? != selected_identity
            || manifest.version != selected.version
            || manifest.library.sha256 != selected.library.sha256
            || manifest.rust_build_sha256 != selected.rust_build_sha256
            || manifest.allocator != selected.allocator
        {
            return Err(Error::new(ErrorKind::Conflict).attach(selected.version.to_string()));
        }
    }
    Ok(best.clone())
}

pub(super) fn manager_load(
    manager: &Manager,
    package: &PluginPackage,
    request: &PluginRequest,
) -> Result<Plugin> {
    todo!("装入前校版本请求和构建记录；预留文件记录。通过实际装入记录在锁外装入，先记实际映像再校验配套及入口。受保护读取并复制元信息。新的清单和实际版本范围都须匹配已有模块副本。复用前显式归还多取的引用。失败保留实际受影响的映像和占用。装入后失败保留真实释放结果。")
}

pub(super) fn manager_maintain(manager: &Manager, budget: usize) -> Result<usize> {
    todo!("在现有责任内取有界工作，锁外调用来源或执行已知尚未进行的系统释放。未知结果保留；不驱动插件或整个运行时。")
}

pub(super) fn plugin_metadata(plugin: &Plugin) -> &Metadata {
    &plugin.metadata
}

pub(super) fn plugin_prepare<T, R>(
    plugin: &Plugin,
    terminal: TerminalInput<T>,
    config: Config,
    runtime: Option<Arc<R>>,
) -> Result<Initialization>
where
    T: Send + Sync + 'static,
    R: IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static,
{
    todo!("把本轮实际有无运行时交给 Authority::reserve_round。取得已核配套、已登记本轮的连接后，按 B2—B4 装包，每项只登记一次。登记后的失败进入明确的宿主归还路径。")
}

pub(super) fn plugin_prepare_without_runtime<T: Send + Sync + 'static>(
    plugin: &Plugin,
    terminal: TerminalInput<T>,
    config: Config,
) -> Result<Initialization> {
    todo!("沿同一轮次准备逻辑，将无运行时传给 Authority::reserve_round。获得连接后只登记配置和终端。")
}

pub(super) fn plugin_current_initialization(plugin: &Plugin) -> Result<Option<Initialization>> {
    todo!("核本模块代际，只返回当前本地管理入口，不查询插件的业务数据。")
}

pub(super) fn plugin_unload(plugin: &Plugin) -> Result<()> {
    todo!("停开新 C 调用并检查退出条件；唯一移出系统持有，锁外释放。成功结束本引用。失败记录保留占用并禁止原地址复用。")
}

pub(super) fn initialization_id(initialization: &Initialization) -> InitIdV1 {
    InitIdV1 {
        module_epoch: initialization.epoch,
        round: initialization.round,
    }
}

pub(super) fn initialization_run(initialization: &Initialization) -> Result<()> {
    todo!("取得同一轮次调用资格，核实实际依赖，用配置外层保护及终端、可选运行时借用同步调用。先记效果先由 Authority 核本模块诊断登记，再处理 B1 诊断，最后归还实际保护。")
}

pub(super) fn initialization_close(initialization: &Initialization) -> Result<()> {
    todo!("不等待在途调用。插件明确完成后保存回执，在本模块持有内校验并接管诊断，再推进 B1 归还。已成功部分不重跑，旧轮只回放原结果。")
}
