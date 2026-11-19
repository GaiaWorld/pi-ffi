use super::*;

pub(super) const fn exports_new<S: Send + 'static>(declaration: Declaration) -> Exports<S> {
    Exports {
        declaration,
        state: Mutex::new(PluginState {
            current: None,
            closed: Vec::new(),
        }),
    }
}

pub(super) unsafe fn export_metadata<S: Send + 'static>(
    exports: &Exports<S>,
    out: *mut MetadataV1,
) -> OutcomeV1 {
    todo!(
        "保护整个 C 入口，检查输出槽的可检查前提，写 UUID 和不可变版本文本。无业务锁，无来源持有。"
    )
}

pub(super) unsafe fn export_initialize<S, T, R>(
    exports: &Exports<S>,
    runtime: *const ObjectRefV1,
    terminal: *const ObjectRefV1,
    config: *const ObjectRefV1,
    initialize: impl FnOnce(Inputs<T, R>, &mut Option<S>) -> Result<()>,
) -> InitReplyV1
where
    S: Send + 'static,
    T: Send + Sync + 'static,
    R: IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static,
{
    todo!("保护完整 C 入口。核必传与轮次、预留记录并占用；从 Config 建连接，先接收终端和已有运行时，再取走配置。锁外调用插件，捕获可展开恐慌并保存状态。结果单列实际进度，诊断按 B1 独立交接。明确未开始时发布最后诊断并在锁外放开插件持有的来源。")
}

pub(super) unsafe fn export_initialize_without_runtime<
    S: Send + 'static,
    T: Send + Sync + 'static,
>(
    exports: &Exports<S>,
    runtime: *const ObjectRefV1,
    terminal: *const ObjectRefV1,
    config: *const ObjectRefV1,
    initialize: impl FnOnce(Inputs<T>, &mut Option<S>) -> Result<()>,
) -> InitReplyV1 {
    todo!("复用同一标准初始化与诊断路径。有运行时输入则在取走配置以前拒绝。缺席时只接收真实终端和配置。")
}

pub(super) unsafe fn export_close<S: Send + 'static>(
    exports: &Exports<S>,
    id: InitIdV1,
    close: impl FnOnce(&mut Option<S>) -> Result<()>,
) -> OutcomeV1 {
    todo!("保护完整 C 入口。只占用原轮次并在锁外退出，保存 Busy 的真实进度。成功后实施单次析构。最终失败保留原因并禁止重做不确定操作。关闭确认后记录无来源持有的结果，在锁外归还诊断来源。")
}
