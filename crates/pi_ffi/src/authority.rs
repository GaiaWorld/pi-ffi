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
    operation: OperationIdV1,
    state: SourceState,
    failure: Option<Arc<Error>>,
}

// 安全前提：宿主校验登记作用域和代码所属。descriptor 的表和上下文受来源保活
// 及本次访问资格保护，各项操作按 B1 的线程安全合同调用，无裸指针交给使用方。
unsafe impl Send for RegisteredSource {}

enum SourceState {
    Registered,
    Retiring,
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
    Source(CallTokenV1),
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
