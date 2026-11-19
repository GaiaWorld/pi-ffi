use super::*;

pub(super) unsafe fn terminal_input<T: Send + Sync + 'static>(
    terminal: Arc<T>,
) -> TerminalInput<T> {
    todo!("保存实际 Arc，不改变终端，不接管服务。")
}

pub(super) fn manager_process(build: HostBuild) -> Result<Manager> {
    todo!("绑定宿主的一份 Authority；插件接入不初始化一个独立的宿主。")
}

pub(super) fn manager_find(
    manager: &Manager,
    packages: &[PluginPackage],
    request: &PluginRequest,
) -> Result<PluginPackage> {
    todo!("按 UUID 和 VersionReq 匹配记录，使用 cmp_precedence。核不同文件的同优先级，拒绝歧义。期间不装入。")
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
    todo!("借用已校验的宿主副本，无插件调用和生命周期状态锁。")
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
    todo!("先核批准的配套 Rust 依赖。预登记本轮和占用，建立实际共同连接。按 B2—B4 装包，每项只登记一次。失败进入明确的宿主归还路径。")
}

pub(super) fn plugin_prepare_without_runtime<T: Send + Sync + 'static>(
    plugin: &Plugin,
    terminal: TerminalInput<T>,
    config: Config,
) -> Result<Initialization> {
    todo!("沿同一轮次准备逻辑，省略运行时装包及运行时输入专有核验。")
}

pub(super) fn plugin_current_initialization(plugin: &Plugin) -> Result<Option<Initialization>> {
    todo!("核本模块代际，只返回当前本地管理入口，不查询插件的业务数据。")
}

pub(super) fn plugin_unload(plugin: &Plugin) -> Result<()> {
    todo!("停开新 C 调用并检查退出条件；唯一移出系统持有，锁外释放。成功结束本引用。失败记录保留占用并禁止原地址复用。")
}

pub(super) fn initialization_id(initialization: &Initialization) -> InitIdV1 {
    todo!("用管理入口的数据重建同一轮次标识。")
}

pub(super) fn initialization_run(initialization: &Initialization) -> Result<()> {
    todo!("取得同一轮次调用资格，核实实际依赖，用配置外层保护及终端、可选运行时借用同步调用。先记效果先由 Authority 核本模块诊断登记，再处理 B1 诊断，最后归还实际保护。")
}

pub(super) fn initialization_close(initialization: &Initialization) -> Result<()> {
    todo!("不等待在途调用。插件明确完成后保存回执，在本模块持有内校验并接管诊断，再推进 B1 归还。已成功部分不重跑，旧轮只回放原结果。")
}
