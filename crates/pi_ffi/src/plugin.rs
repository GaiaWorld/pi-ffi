//! 手工包装（manual wrapping）标准 Rust 入口。

#[path = "plugin_inner.rs"]
mod inner;

use crate::{
    abi::{InitIdV1, InitReplyV1, MetadataV1, ObjectRefV1, OutcomeV1},
    config::Config,
    runtime::{IdleDispatch, Runtime, RuntimeQuery, RuntimeWait},
    service::Terminal,
};
use parking_lot::Mutex;
use pi_ffi_abi::LeaseV1;
use pi_ffi_core::{diagnostic::ManagementFailure, source::Registry};
use pi_result::Result;
use std::sync::Arc;

/// 插件在编译时声明的只读库元信息（read-only library metadata）。
pub struct Declaration {
    /// 与后续版本保持相同的 UUID。
    pub uuid: [u8; 16],
    /// 完整语义化版本（SemVer）文本。
    pub version: &'static str,
}

/// 已按本轮核验并接收的原生 Rust 初始化输入。
pub struct Inputs<T, R = ()> {
    pub terminal: Terminal<T>,
    pub config: Config,
    pub runtime: Option<Runtime<R>>,
}

/// 插件实现方在本模块声明一份。S 为插件自有状态，宿主不接触。
pub struct Exports<S> {
    declaration: Declaration,
    state: Mutex<PluginState<S>>,
}

struct PluginState<S> {
    current: Option<PluginRound<S>>,
    closed: Vec<InitIdV1>,
}

struct PluginRound<S> {
    id: InitIdV1,
    bound: BoundInputs,
    phase: PluginPhase,
    state: Option<S>,
    source: Option<Arc<Registry>>,
    init_failure: Option<ManagementFailure>,
    close_failure: Option<ManagementFailure>,
}

struct BoundInputs {
    config: LeaseV1,
    terminal: LeaseV1,
    runtime: Option<LeaseV1>,
}

enum PluginPhase {
    Initializing,
    Initialized,
    Rejected,
    NeedsClose,
    Closing,
    Closed,
    CleanupFailed,
}

/*
输入必传项、完整持有与同一权威先于实际初始化。重复调用不接收另一份配置。
先接收终端和有值的运行时，再取走配置并调用插件逻辑。单一短锁保留初始化或关闭资格及实际进度。业务函数、C 回调和状态析构在锁外。
开始调用插件逻辑前预留本轮记录。插件记录已发生的资源和服务登记，便于发生
失败或栈展开后关闭。关闭检查对应标识，旧轮回放不会操作新轮。

退出只由插件决定：原来登记的服务、派生引用和自身工作均由插件关闭。S 在
当前调用中唯一借出。已开始的关闭尚未退出时报 Busy，存回实际状态供后续
推进。关闭完成后才在锁外销毁 S。恐慌或结果不明保留后续责任，禁止盲目重跑。

本模块保存稳定诊断快照，不保存已移交的诊断句柄。新一次合法报告在本轮
有效 Registry 下建立独立诊断。配置未能建立连接时，边界错误返回双分类，
宿主结合实际失败步骤报告。明确未开始或插件关闭成功时，锁内移出当前插件
持有的诊断来源，写好最后一次结果和无持有快照后在锁外归还。重复调用使用
已有快照及有限分类，避免重新登记同轮来源。宿主先退役这个来源，再确认它
持有的配置来源退出。元信息没有业务初始化依赖。
*/

impl<S: Send + 'static> Exports<S> {
    /// 建立本插件模块自己的标准接入和轮次记录。
    // @trace 4
    pub const fn new(declaration: Declaration) -> Self {
        inner::exports_new(declaration)
    }

    /// 只在有效调用中写出编译期元信息。
    ///
    /// # Safety
    ///
    /// 调用者须持有本模块，out 指向足够且可独占写入的宿主输出槽。
    /// 成功后调用方在释放模块保护前复制声明。
    // @trace 5
    pub unsafe fn metadata(&self, out: *mut MetadataV1) -> OutcomeV1 {
        unsafe { inner::export_metadata(self, out) }
    }

    /// 标准同步初始化。插件在产生效果前通过 S 保有本轮真实的清理状态。
    ///
    /// # Safety
    ///
    /// 非空输入由遵约宿主按同轮条件保活至返回。接入应在本次调用线程拆包。
    /// 插件必须能通过自己的关闭入口回收实际发生的所有效果。
    // @trace 6
    pub unsafe fn initialize<T, R>(
        &self,
        runtime: *const ObjectRefV1,
        terminal: *const ObjectRefV1,
        config: *const ObjectRefV1,
        initialize: impl FnOnce(Inputs<T, R>, &mut Option<S>) -> Result<()>,
    ) -> InitReplyV1
    where
        T: Send + Sync + 'static,
        R: IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static,
    {
        unsafe { inner::export_initialize(self, runtime, terminal, config, initialize) }
    }

    /// 插件只接受省略运行时的输入。终端和配置仍须按本轮交接。
    ///
    /// # Safety
    ///
    /// 同 initialize 的输入存续及本轮清理合同。
    // @trace 8
    pub unsafe fn initialize_without_runtime<T: Send + Sync + 'static>(
        &self,
        runtime: *const ObjectRefV1,
        terminal: *const ObjectRefV1,
        config: *const ObjectRefV1,
        initialize: impl FnOnce(Inputs<T>, &mut Option<S>) -> Result<()>,
    ) -> InitReplyV1 {
        unsafe {
            inner::export_initialize_without_runtime(self, runtime, terminal, config, initialize)
        }
    }

    /// 插件关闭已确认的轮次。未完成时保存真实进度，由插件报告 Busy 或详细失败。
    ///
    /// # Safety
    ///
    /// 调用方保活模块和当前 C 调用。业务关闭应处理自己的在途工作及实际
    /// 登记引用。重复入口不允许销毁新轮次的状态。
    // @trace 7
    pub unsafe fn close(
        &self,
        id: InitIdV1,
        close: impl FnOnce(&mut Option<S>) -> Result<()>,
    ) -> OutcomeV1 {
        unsafe { inner::export_close(self, id, close) }
    }
}
