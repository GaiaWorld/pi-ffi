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

/// 初始化 v1 的原生输入（native inputs），不包含初始化数据（initialization data）。
#[cfg(feature = "init_v1")]
pub struct InputsV1<T, R = ()> {
    /// 本轮接收的实际 P2P 终端（P2P service terminal）持有，按其 Rust 接口（API）使用。
    /// 共享实例仍由宿主管理，插件不得关闭它。
    pub terminal: Terminal<T>,
    /// 本轮按值取得的原生配置（Config），由插件拥有并负责使用与清理。
    pub config: Config,
    /// 本轮可选的运行时（runtime）独立持有；None 表示未注入。
    /// 共享实例的运行和关闭责任仍归宿主，插件不得通过该输入关闭它。
    pub runtime: Option<Runtime<R>>,
}

/// 已按本轮核验并接收的初始化 v2 原生输入。
#[cfg(feature = "init_v2")]
pub struct Inputs<T, R = (), D: Send + Sync + 'static = ()> {
    /// 本轮接收的实际 P2P 终端（P2P service terminal）持有，按其 Rust 接口（API）使用。
    /// 共享实例仍由宿主管理，插件不得关闭它。
    pub terminal: Terminal<T>,
    /// 本轮按值取得的原生配置（Config），由插件拥有并负责使用与清理。
    pub config: Config,
    /// 本轮可选的运行时（runtime）独立持有；None 表示未注入。
    /// 共享实例的运行和关闭责任仍归宿主，插件不得通过该输入关闭它。
    pub runtime: Option<Runtime<R>>,
    /// 双方协商类型的初始化数据（initialization data），按值取得，不要求 `Clone`。
    pub data: D,
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
    init_failure: Option<Arc<ManagementFailure>>,
    close_failure: Option<Arc<ManagementFailure>>,
}

// 完整租约（lease）与初始化版本共同绑定；同轮变体不同也视为冲突。
enum BoundInputs {
    #[cfg(feature = "init_v1")]
    V1 {
        config: LeaseV1,
        terminal: LeaseV1,
        runtime: Option<LeaseV1>,
    },
    #[cfg(feature = "init_v2")]
    V2 {
        config: LeaseV1,
        terminal: LeaseV1,
        runtime: Option<LeaseV1>,
        data: LeaseV1,
    },
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
各版本先预核终端（terminal）和有值的运行时（runtime）；v2 另外预核数据 D。
首次可能消费前记 INIT_NEEDS_CLOSE 并保留 Initializing，关闭此时仍 Busy。
v1 只取配置（Config）；v2 先取 D 再取配置，交给对应版本回调。
接收中的 Option<D> 保存在边界（boundary）保护闭包外，交付前失败时另行保护析构（Drop）。
析构（Drop）失败先固定 CleanupFailed/close_failure，再处理诊断（diagnostic）；
通用结算和关闭（close）不能把该失败覆盖为成功。交付以后 D 的业务语义完全透明。
单一短锁（lock）保留初始化（initialization）或关闭（close）资格及实际进度，
业务函数、C 回调（callback）和状态析构（Drop）在锁外。
开始调用插件逻辑前预留本轮记录。插件记录已发生的资源和服务登记，便于发生
失败或栈展开后关闭。关闭检查对应标识，旧轮回放不会操作新轮。

退出只由插件决定：原来登记的服务、派生引用和自身工作均由插件关闭。S 在
当前调用中唯一借出。已开始的关闭尚未退出时报 Busy，存回实际状态供后续
推进。关闭完成后才在锁外销毁 S。恐慌或结果不明保留后续责任，禁止盲目重跑。

本模块保存共享的不可变诊断快照，不保存已移交的诊断句柄。短锁内只复制
快照或来源的共享持有，锁外建立、报告或最后销毁快照。新一次合法报告在本轮
有效 Registry 下建立独立诊断。配置未能建立连接时，边界错误返回双分类，
宿主结合实际失败步骤报告。明确未开始或插件关闭成功时，锁内移出当前插件
持有的诊断来源，写好最后一次结果和无持有快照后在锁外归还。重复调用使用
已有快照及有限分类，避免重新登记同轮来源。宿主先退役这个来源，再确认它
持有的配置来源退出。元信息没有业务初始化依赖。
*/

impl<S: Send + 'static> Exports<S> {

    /// 接收初始化 v1 的三输入；所有版本共用本 Exports 的状态（state）。
    /// 有效同轮跨版返回 Conflict；同版按原完整租约重放或返回 Busy，不再次消费。
    ///
    /// # Safety
    ///
    /// 调用方持续保活本模块及同轮真实输入至返回，在本调用线程拆包。
    /// 只有运行时（runtime）可为空；配套 Rust 类型、构建、分配器及代码存续
    /// 必须满足原输入合同。插件通过自己的关闭入口处理实际发生的内部效果。
    #[cfg(feature = "init_v1")]
    // @trace 9
    pub unsafe fn initialize_v1<T, R>(
        &self,
        runtime: *const ObjectRefV1,
        terminal: *const ObjectRefV1,
        config: *const ObjectRefV1,
        initialize: impl FnOnce(InputsV1<T, R>, &mut Option<S>) -> Result<()>,
    ) -> InitReplyV1
    where
        T: Send + Sync + 'static,
        R: IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static,
    {
        unsafe { inner::export_initialize_v1(self, runtime, terminal, config, initialize) }
    }

    /// 专用初始化 v1，仅接受空运行时（runtime）；非空即拒绝，不静默跳过。
    ///
    /// # Safety
    ///
    /// 遵守 initialize_v1 的同轮输入、配套构建及本轮清理前提。
    #[cfg(feature = "init_v1")]
    // @trace 10
    pub unsafe fn initialize_v1_without_runtime<T: Send + Sync + 'static>(
        &self,
        runtime: *const ObjectRefV1,
        terminal: *const ObjectRefV1,
        config: *const ObjectRefV1,
        initialize: impl FnOnce(InputsV1<T>, &mut Option<S>) -> Result<()>,
    ) -> InitReplyV1 {
        unsafe { inner::export_initialize_v1_without_runtime(self, runtime, terminal, config, initialize) }
    }

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
    #[cfg(feature = "metadata_v1")]
    // @trace 5
    pub unsafe fn metadata(&self, out: *mut MetadataV1) -> OutcomeV1 {
        unsafe { inner::export_metadata(self, out) }
    }

    /// 标准同步初始化。插件在产生效果前通过 S 保有本轮真实的清理状态。
    ///
    /// # Safety
    ///
    /// 非空输入由遵约宿主按同轮条件保活至返回。接入应在本次调用线程拆包。
    /// 通过预核的 D 是双方约定的同一实际类型，配套 Rust 构建（matching build）、分配器
    /// （allocator）及其析构／回调代码（drop/callback code）存续至最后使用。
    /// 数据为 `()` 时也须提供第四输入；只有运行时（runtime）可以为空。
    /// 登记类型与 D 不同会在消费前返回 `Conflict`，不要求调用者提前证明二者相等。
    /// 插件必须能通过自己的关闭入口回收实际发生的所有效果。
    #[cfg(feature = "init_v2")]
    // @trace 6
    pub unsafe fn initialize<T, R, D>(
        &self,
        runtime: *const ObjectRefV1,
        terminal: *const ObjectRefV1,
        config: *const ObjectRefV1,
        data: *const ObjectRefV1,
        initialize: impl FnOnce(Inputs<T, R, D>, &mut Option<S>) -> Result<()>,
    ) -> InitReplyV1
    where
        T: Send + Sync + 'static,
        R: IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static,
        D: Send + Sync + 'static,
    {
        unsafe { inner::export_initialize(self, runtime, terminal, config, data, initialize) }
    }

    /// 插件（plugin）只接受省略运行时（runtime）的输入。
    /// 终端（terminal）、配置（Config）和数据（data）仍须按本轮交接。
    ///
    /// # Safety
    ///
    /// 同 initialize 的输入存续及本轮清理合同。
    #[cfg(feature = "init_v2")]
    // @trace 8
    pub unsafe fn initialize_without_runtime<T: Send + Sync + 'static, D: Send + Sync + 'static>(
        &self,
        runtime: *const ObjectRefV1,
        terminal: *const ObjectRefV1,
        config: *const ObjectRefV1,
        data: *const ObjectRefV1,
        initialize: impl FnOnce(Inputs<T, (), D>, &mut Option<S>) -> Result<()>,
    ) -> InitReplyV1 {
        unsafe {
            inner::export_initialize_without_runtime(self, runtime, terminal, config, data, initialize)
        }
    }

    /// 插件关闭已确认的轮次。未完成时保存真实进度，由插件报告 Busy 或详细失败。
    ///
    /// # Safety
    ///
    /// 调用方保活模块和当前 C 调用。业务关闭应处理自己的在途工作及实际
    /// 登记引用。重复入口不允许销毁新轮次的状态。
    #[cfg(feature = "close_v1")]
    // @trace 7
    pub unsafe fn close(
        &self,
        id: InitIdV1,
        close: impl FnOnce(&mut Option<S>) -> Result<()>,
    ) -> OutcomeV1 {
        unsafe { inner::export_close(self, id, close) }
    }
}
