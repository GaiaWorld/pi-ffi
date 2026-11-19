use super::*;
use pi_ffi_core::{boundary::Context, diagnostic};
use pi_result::ErrorKind;

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
    diagnostic::protect_outcome(
        Context {
            operation: "plugin metadata",
            module_epoch: 0,
            round: 0,
            source: 0,
        },
        || {
            let declaration = &exports.declaration;
            if out.is_null()
                || !out.is_aligned()
                || declaration.uuid == [0; 16]
                || semver::Version::parse(declaration.version).is_err()
            {
                return diagnostic::limited_outcome(
                    ErrorKind::InvalidInput,
                    ErrorKind::InvalidState,
                );
            }
            // SAFETY: 调用方提供完整、独占的输出槽（output slot），版本字节只借自本模块。
            unsafe {
                out.write(MetadataV1 {
                    uuid: pi_ffi_abi::UuidV1 {
                        bytes: declaration.uuid,
                    },
                    version: declaration.version.as_ptr(),
                    version_len: declaration.version.len() as u64,
                })
            };
            diagnostic::success_outcome()
        },
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
    todo!("用同一 C 边界保护检查和拒绝有运行时输入的情况，取配置前完成检查。提前返回时也在保护内销毁未调用的初始化闭包。缺席时只接收真实终端和配置。")
}

pub(super) unsafe fn export_close<S: Send + 'static>(
    exports: &Exports<S>,
    id: InitIdV1,
    close: impl FnOnce(&mut Option<S>) -> Result<()>,
) -> OutcomeV1 {
    todo!("保护完整 C 入口。只占用原轮次并在锁外退出，保存 Busy 的真实进度。成功后实施单次析构。采集原错误后，在保护内析构原报告。用 ManagementFailure::finish_report_cleanup 得到最终快照及清理确认结果。清理未确认时保存终局失败；原 Busy 归为 Internal，诊断保留原分类及现有材料，后续只重放。关闭确认后记录无来源持有的结果，在锁外归还诊断来源。")
}
