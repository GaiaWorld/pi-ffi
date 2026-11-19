//! 宿主进程内的统一管理记录（host authority）。

#[path = "authority_inner.rs"]
mod inner;

use crate::{
    loader::{Entrypoints, MappedIdentity, NativeModule, VerifiedFiles, VerifiedImage},
    package::{HostBuild, Metadata},
};
use parking_lot::Mutex;
use pi_ffi_abi::{
    ApiHeaderV1, CallTargetV1, CallTokenV1, HostControlApiV1, InitIdV1, LeaseV1, OperationIdV1,
    OutcomeV1, SourceDescriptorV1, HOST_CONTROL_MAJOR,
};
use pi_ffi_adapters::{
    config::PublishedConfig, runtime::PublishedRuntime, service::PublishedTerminal,
};
use pi_ffi_core::control::Connection;
use pi_result::{Error, Result};
use std::{ffi::c_void, sync::Arc};

pub(crate) struct Authority {
    state: Mutex<State>,
}

static AUTHORITY: Authority = Authority {
    state: Mutex::new(State {
        build: None,
        next_identifier: 1,
        modules: Vec::new(),
        rounds: Vec::new(),
        sources: Vec::new(),
        holdings: Vec::new(),
        calls: Vec::new(),
    }),
};

static CONTROL: HostControlApiV1 = HostControlApiV1 {
    header: ApiHeaderV1 {
        major: HOST_CONTROL_MAJOR,
        size: core::mem::size_of::<HostControlApiV1>() as u32,
    },
    next_operation: Some(next_operation),
    register_source: Some(register_source),
    track_holding: Some(track_holding),
    enter_call: Some(enter_call),
    leave_call: Some(leave_call),
    request_release: Some(request_release),
    retire_source: Some(retire_source),
};

struct State {
    build: Option<Arc<HostBuild>>,
    next_identifier: u64,
    modules: Vec<ModuleRecord>,
    rounds: Vec<RoundRecord>,
    sources: Vec<RegisteredSource>,
    holdings: Vec<HoldingRecord>,
    calls: Vec<CallRecord>,
}

struct ModuleRecord {
    epoch: u64,
    files: Arc<VerifiedFiles>,
    mapped: Option<MappedIdentity>,
    image: Option<Arc<VerifiedImage>>,
    library: Option<NativeModule>,
    metadata: Option<Arc<Metadata>>,
    phase: ModulePhase,
    failure: Option<Arc<Error>>,
}

enum ModulePhase {
    Loading,
    Loaded,
    Unloading,
    AwaitingRelease,
    Released,
    Quarantined,
}

struct RoundRecord {
    id: InitIdV1,
    phase: RoundPhase,
    inputs: Option<Arc<PreparedInputs>>,
    connection: Option<Connection>,
    ffi_progress: Option<FfiProgress>,
    init_failure: Option<Arc<Error>>,
    close_failure: Option<Arc<Error>>,
}

enum RoundPhase {
    Preparing,
    Prepared,
    Initializing,
    Initialized,
    Rejected,
    NeedsClose,
    Closing,
    ReturningHoldings,
    Closed,
    CleanupFailed,
}

struct CallbackScope {
    authority: &'static Authority,
    actor: u64,
    id: InitIdV1,
    owner: CodeOwner,
}

struct CallbackScopes {
    host: CallbackScope,
    plugin: CallbackScope,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CodeOwner {
    Host,
    Plugin { epoch: u64 },
}

struct RegisteredSource {
    owner: CodeOwner,
    id: InitIdV1,
    source: u64,
    descriptor: SourceDescriptorV1,
    registration_operation: OperationIdV1,
    retirement: Option<SourceRetirement>,
    maintenance_pending: bool,
    maintenance_operation: Option<OperationIdV1>,
    state: SourceState,
    failure: Option<Arc<Error>>,
}

// 安全前提：宿主校验登记作用域和代码所属。descriptor 的表和上下文受来源保活
// 及本次访问资格保护，各项操作按 B1 的线程安全合同调用，无裸指针交给使用方。
unsafe impl Send for RegisteredSource {}

// 先登记的来源退役（source retirement）占用唯一实际操作。
// request 是来源经宿主入口提交的原标识；control 用于推进真实来源检查。
struct SourceRetirement {
    request: Option<OperationIdV1>,
    control: OperationIdV1,
}

enum SourceState {
    Registered,
    Retiring,
    CheckingRetirement {
        thread: std::thread::ThreadId,
        diagnostic: Option<HoldingKey>,
    },
    RetirementFailed {
        kind: u32,
        diagnostic_failure_kind: u32,
    },
    ReleasingContext,
    Retired,
    Quarantined,
}

// 表与上下文从唯一来源登记取用。内部记录只保存已经匹配过的持有身份。
struct HoldingKey {
    lease: LeaseV1,
    kind: u32,
}

struct HoldingRecord {
    target: HoldingKey,
    // 与本条记录一并预留；重放不重新分配。来源对象归还复用此操作编号。
    release_operation: OperationIdV1,
    phase: HoldingPhase,
    failure: Option<Arc<Error>>,
}

enum HoldingPhase {
    Tracked,
    Requested,
    Releasing,
    Returned,
    Quarantined,
}

enum CallIdentifier {
    Standard(u64),
    // 同请求的回执不是新使用资格。每个独立调用取得自己的操作编号。
    Source {
        token: CallTokenV1,
        operation: OperationIdV1,
        rights: u64,
        active: bool,
    },
}

struct CallRecord {
    token: CallIdentifier,
    actor: u64,
    owner: CodeOwner,
    id: InitIdV1,
    target: Option<HoldingKey>,
}

pub(crate) struct ModuleView {
    pub(crate) epoch: u64,
    pub(crate) metadata: Arc<Metadata>,
}

pub(crate) enum LoadAdmission {
    Reuse(ModuleView),
    New(SystemOperation),
}

/// 一次已登记的系统装入或释放，跟踪已取得引用和实际系统结果。
pub(crate) struct SystemOperation {
    authority: &'static Authority,
    epoch: u64,
    files: Arc<VerifiedFiles>,
    phase: SystemPhase,
    library: Option<NativeModule>,
}

enum SystemPhase {
    Reserved,
    Opening,
    OwnsReference,
    ReleasingReference,
    Finished,
}

pub(crate) struct PreparedInputs {
    pub(crate) config: PublishedConfig,
    pub(crate) terminal: PublishedTerminal,
    pub(crate) runtime: Option<PublishedRuntime>,
}

#[derive(Clone, Copy)]
pub(crate) enum Operation {
    Initialize,
    Close,
}

#[derive(Clone, Copy)]
pub(crate) enum FfiProgress {
    InitializationNotStarted,
    InitializationCompleted,
    InitializationNeedsClose,
    CloseCompleted,
    ClosePending,
    OutcomeUnknown,
}

pub(crate) struct RoundCall {
    authority: &'static Authority,
    token: u64,
    id: InitIdV1,
    finished: bool,
    pub(crate) entries: Entrypoints,
    pub(crate) image: Arc<VerifiedImage>,
    pub(crate) connection: Connection,
    pub(crate) inputs: Option<Arc<PreparedInputs>>,
}

pub(crate) enum RoundAdmission {
    Call(RoundCall),
    Replay { error: Option<Arc<Error>> },
    ReturnHoldings,
}

/*
唯一短锁保护版本占用、轮次、已登记调用和归还记录。系统装卸、插件和来源
调用、诊断格式化及可能是最后一份所有权的析构，在锁外。活动调用令牌、不重复
的操作编号和来源身份在同一处登记。来源控制函数返回后才移除其保护。

独立持有完成登记时原子预留归还操作，宿主操作的 actor 为零。对象归还用本编号；
诊断 C 归还无编号，同一记录只授予一次宿主派发资格。首次引导可先在原保护里
显式归还，宿主再按既有记录重放；来源只改变一次实际状态。退役前继续保存来源、
表和上下文的合法核对依据。活动来源调用绑定原操作、请求权限、持有及真实作用域。
相同活动输入取得同一个令牌；改输入返回 Conflict，结束后的原请求不能重新取得
资格。归还一次后只保留重放所需记录，不再持有代码；原作用域彻底退出后可回收。

来源维护进度（source maintenance）有独立待办标记和在途操作。一次有界推进启用
新编号，锁外调用；期间产生的新工作继续记录。来源退役（source retirement）先
固定原请求与实际控制操作并关闭新业务。未进入独占时允许合法 READ／CLEANUP
重入和既有归还。有其它活动调用或维护（maintenance）时返回 Busy。独占检查期间
只准当前同步检查线程、同一来源登记一个新诊断，并保存完整持有供返回时核对。
同持有可重放，其它访问不开始。有持有或返回与所登记诊断不符时不能确认退出；
通过核验且全部实际保护归还后，才持独占归还上下文一次。未齐则解除独占并保持退役进度。
明确检查失败时保留原分类，解除独占后交付本次合法登记的诊断。终局重放从
宿主记录返回已有失败。需要本地报告时用宿主已拥有的数据；C 重放缺少可
重新交付的诊断时，保留原操作类别和首次有限结果的非零次级类别。首次交付完整
诊断的终局重放用 InvalidState 作为次级类别，归零整份诊断字段。原结果可能在来源尚未写入重放记录时
产生，终局后不再通过来源检查取报告。上下文释放失败或结果不明时保存宿主
错误，停用相关地址。

申请安装时先占实际文件；系统返回后以活身份确认，再与可信构建匹配。读取
元信息用装入调用持有的模块。额外系统引用或装入后拒绝均显式归还，失败留记录。
释放不确定时材料和宿主错误持续存在，任意已失效入口立即停止使用。
装入和释放的记录先于系统调用，SystemOperation 在核验、正常拒绝及展开时
持有引用。销毁时只归回已确定的引用或标明不明结果，系统操作始终显式进行。

本轮先预留真实进度和诊断接管，来源退役按实际持有。已知插件关闭后只推进本库
清理。类 Busy 的结果无阻塞，优先保留原错误分类及已发生效果，重放时不会安插
新的初始化或关闭。每条继续执行的操作都要验证本轮仍有效。
*/

impl Authority {
    // @trace 1
    pub(crate) fn process(build: HostBuild) -> Result<&'static Self> {
        inner::authority_process(build)
    }

    // @trace 2
    pub(crate) fn build(&self) -> Result<Arc<HostBuild>> {
        inner::authority_build(self)
    }

    // @trace 3
    pub(crate) fn reserve_load(&'static self, files: Arc<VerifiedFiles>) -> Result<LoadAdmission> {
        inner::authority_reserve_load(self, files)
    }

    /// 在读取插件元信息前记下实际所取映像。
    // @trace 25
    pub(crate) fn record_mapping(&self, epoch: u64, mapped: MappedIdentity) -> Result<()> {
        inner::authority_record_mapping(self, epoch, mapped)
    }

    // @trace 6
    pub(crate) fn reserve_round(&'static self, epoch: u64) -> Result<(InitIdV1, Connection)> {
        inner::authority_reserve_round(self, epoch)
    }

    // @trace 7
    pub(crate) fn publish_inputs(&self, id: InitIdV1, inputs: PreparedInputs) -> Result<()> {
        inner::authority_publish_inputs(self, id, inputs)
    }

    // @trace 8
    pub(crate) fn fail_prepare(&self, id: InitIdV1, cause: Error) -> Error {
        inner::authority_fail_prepare(self, id, cause)
    }

    // @trace 9
    pub(crate) fn current_round(&self, epoch: u64) -> Result<Option<InitIdV1>> {
        inner::authority_current_round(self, epoch)
    }

    // @trace 10
    pub(crate) fn begin_round(
        &'static self,
        id: InitIdV1,
        operation: Operation,
    ) -> Result<RoundAdmission> {
        inner::authority_begin_round(self, id, operation)
    }

    // @trace 11
    pub(crate) fn remember_progress(&self, call: &RoundCall, progress: FfiProgress) {
        inner::authority_remember_progress(self, call, progress)
    }

    // @trace 24
    pub(crate) fn validate_diagnostic(&self, call: &RoundCall, outcome: &OutcomeV1) -> Result<()> {
        inner::authority_validate_diagnostic(self, call, outcome)
    }

    // @trace 12
    pub(crate) fn finish_round(&self, call: RoundCall, result: Result<()>) -> Result<()> {
        inner::authority_finish_round(self, call, result)
    }

    // @trace 13
    pub(crate) fn prepare_unload(&'static self, epoch: u64) -> Result<Option<SystemOperation>> {
        inner::authority_prepare_unload(self, epoch)
    }

    // @trace 15
    pub(crate) fn maintain(&self, round: Option<InitIdV1>, budget: usize) -> Result<usize> {
        inner::authority_maintain(self, round, budget)
    }
}

// @trace 16
unsafe extern "C" fn next_operation(context: *mut c_void, out: *mut OperationIdV1) -> u32 {
    unsafe { inner::next_operation(context, out) }
}

// @trace 17
unsafe extern "C" fn register_source(
    context: *mut c_void,
    operation: OperationIdV1,
    descriptor: SourceDescriptorV1,
    out: *mut u64,
) -> OutcomeV1 {
    unsafe { inner::register_source(context, operation, descriptor, out) }
}

// @trace 18
unsafe extern "C" fn track_holding(context: *mut c_void, target: CallTargetV1) -> u32 {
    unsafe { inner::track_holding(context, target) }
}

// @trace 19
unsafe extern "C" fn enter_call(
    context: *mut c_void,
    target: CallTargetV1,
    operation: OperationIdV1,
    rights: u64,
    out: *mut CallTokenV1,
) -> u32 {
    unsafe { inner::enter_call(context, target, operation, rights, out) }
}

// @trace 20
unsafe extern "C" fn leave_call(context: *mut c_void, token: CallTokenV1) -> u32 {
    unsafe { inner::leave_call(context, token) }
}

// @trace 21
unsafe extern "C" fn request_release(context: *mut c_void, target: CallTargetV1) -> u32 {
    unsafe { inner::request_release(context, target) }
}

// @trace 22
unsafe extern "C" fn retire_source(
    context: *mut c_void,
    source: u64,
    operation: OperationIdV1,
) -> OutcomeV1 {
    unsafe { inner::retire_source(context, source, operation) }
}

impl Drop for RoundCall {
    // @trace 33
    fn drop(&mut self) {
        inner::round_call_drop(self)
    }
}

impl SystemOperation {
    // @trace 26
    pub(crate) fn epoch(&self) -> u64 {
        inner::system_epoch(self)
    }

    // @trace 27
    pub(crate) fn open(&mut self) -> Result<()> {
        inner::system_open(self)
    }

    // @trace 28
    pub(crate) fn library(&self) -> Result<&NativeModule> {
        inner::system_library(self)
    }

    // @trace 29
    pub(crate) fn confirm_loaded(
        self,
        image: VerifiedImage,
        metadata: Metadata,
    ) -> Result<ModuleView> {
        inner::system_confirm_loaded(self, image, metadata)
    }

    // @trace 30
    pub(crate) fn reject(self, cause: Error) -> Error {
        inner::system_reject(self, cause)
    }

    // @trace 31
    pub(crate) fn release(self) -> Result<()> {
        inner::system_release(self)
    }
}

impl Drop for SystemOperation {
    // @trace 32
    fn drop(&mut self) {
        inner::system_operation_drop(self)
    }
}
