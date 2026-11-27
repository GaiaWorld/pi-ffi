//! 宿主进程内的统一管理记录（host authority）。

#[path = "authority_inner.rs"]
mod inner;

use crate::{
    loader::{
        Entrypoints, InitializationEntry, InitializationVersion, MappedIdentity, NativeModule, VerifiedDeployment, VerifiedFiles, VerifiedImage,
    },
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
        retained_images: Vec::new(),
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
    retained_images: Vec<RetainedImage>,
    rounds: Vec<RoundRecord>,
    sources: Vec<RegisteredSource>,
    holdings: Vec<HoldingRecord>,
    calls: Vec<CallRecord>,
}

// 已退役及正在移交的系统引用由进程记录持有。旧管理代留在 ModuleRecord。
// 同一实际映像成功复用时，先下线原保留记录，再提交新管理代。
// @trace 34
struct RetainedImage {
    // 本次保留来源的管理代，仅用来确认保留记录身份。
    retired_epoch: u64,
    // 无文件句柄（file handle）的已核资料。
    deployment: Arc<VerifiedDeployment>,
    // 与 Held 引用或当前登记的系统操作对应，只作身份比较。
    mapped: MappedIdentity,
    phase: RetentionPhase,
}

enum RetentionPhase {
    // 原管理代关闭准入，引用和文件由其退役操作持有。
    Retiring,
    // 已确认归还核验文件，可以按已核构建重新接入。
    Held(NativeModule),
    // 新代等待核验，原引用已经移入此新代的系统操作。
    Revalidating { epoch: u64 },
}

struct ModuleRecord {
    epoch: u64,
    // 装入准备到明确结束期间保有实际文件。确认结束后移出记录持有。
    files: Option<Arc<VerifiedFiles>>,
    // 只在仍有明确真实系统引用时用于映像关联。引用一旦进入可能失效的归还阶段，
    // 该值退出匹配条件。已结束的保留映像身份归 RetainedImage。
    mapped: Option<MappedIdentity>,
    image: Option<Arc<VerifiedImage>>,
    library: Option<NativeModule>,
    // 先保存从实际已核映像复制的元信息。装入中和隔离时只作身份及占用依据。
    // 只有活动代可把与已批准清单匹配的元信息交付宿主。
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
    // 只由准备方法选择，与实际输入变体一致；后续不得换版。
    version: InitializationVersion,
    // 本轮已发出的操作号（operation identifier）。实际动作仍由其原记录保存。
    issued_operations: Vec<OperationIdV1>,
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
    // 完成退役前先从操作及旧代移出并在锁外销毁文件持有。
    // 未确认的失败保留已有证据；完成状态可以是 None。
    files: Option<Arc<VerifiedFiles>>,
    purpose: SystemPurpose,
    phase: SystemPhase,
    library: Option<NativeModule>,
}

// 已承接引用的操作按其许可和来源收口。旧的系统阶段不单独赋予释放资格。
// @trace 36
enum SystemPurpose {
    Load,
    Revalidate { retired_epoch: u64 },
    Retire,
    ReturnDuplicate { original_epoch: u64 },
}

enum SystemPhase {
    Reserved,
    Opening,
    OwnsReference,
    // 已开始可能影响真实引用的系统归还。结果不明时只隔离。
    ReleasingReference,
    // 系统明确确认归还，仍需完成进程记录和核验文件收口。
    ReleaseConfirmed,
    Finished,
}

// 本轮原持有（holding）按版本保存；V1 不创建虚假的第四输入。
pub(crate) enum PreparedInputs {
    #[cfg(feature = "init_v1")]
    V1 {
        config: PublishedConfig,
        terminal: PublishedTerminal,
        runtime: Option<PublishedRuntime>,
    },
    #[cfg(feature = "init_v2")]
    V2 {
        config: PublishedConfig,
        terminal: PublishedTerminal,
        runtime: Option<PublishedRuntime>,
        data: pi_ffi_adapters::data::PublishedData,
    },
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
    // 随本轮调用资格复制的准确入口，关闭仍使用原 entries.close。
    pub(crate) initialize: InitializationEntry,
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

归属与准入（ownership and admission）。装入前先核真实文件，再在短锁内查
活动、正在变更、已保留及隔离记录。活动复用在锁内取得原文件和实际映像的 Arc，
锁外核配套，再在锁内验同一有效代。已保留映像先取无句柄资料，锁外核候选。
二次入锁仍是原保留记录且允许接入时，预留新代并把原引用移入其 SystemOperation；
操作持有至核对或错误收口，禁止重复装入。新代重新核实际映像、依赖、入口和
元信息，成功才发布。材料对照失败不改变保留记录。无法确认实际映像时隔离所持引用。

退役（retirement）。先核旧轮、未退出的来源和持有、回调、标准调用均已结束，
再核本代直接文件持有与映像内文件属于同一 Arc。仍有外部核验或调用持有其 Arc
时返回 Busy。全部外借从同一短锁取得；检查该映像只有管理记录的一份引用，
文件只有管理记录与映像的两份引用。锁内关闭新准入后再移动持有。下游析构和
平台操作均在锁外。装入、核验及归还资格按管理代检查。

退役操作按包选择分支。默认在归还文件前预留保留记录，以便失败前恢复原管理代。
在旧代仍处于退役时移出并销毁核验文件及可调用入口资料，含操作自己的一份文件
持有。锁内将同一系统引用移到保留记录，发布旧代完成和保留可用。调用者依旧代
记录重放，未完成的移交返回 Busy。提前卸载仅在实际已核制品和原轮满足前提时
由锁外系统调用一次；确认后以同样次序归还文件并完成旧代。

加载失败（load failure）。确认未取得引用则按原结果结束预留。已取得而缺少
释放依据则在预留记录保留引用和原错误，关闭相关地址。已从实际映像复制出的
完整 UUID／SemVer 先入记录，再判定清单和请求。隔离以实际及已批准的各相关
身份保持占用：无实际身份时按该次已批准清单；不同则同时保持两组。
新轮次准入和已准备轮次首次正式初始化都核此占用。其它已开始轮次若映像和
调用资格仍经确认，可按原权限关闭。只有完整核验的冗余引用
能在主引用受保护时归还；原活动代的文件和映像 Arc 持有至系统结果已经收录，
发生展开时也比该冗余操作后退出。主引用保护已失时只保留冗余引用，不启用归还。
保留映像重核失败时，短锁内把原 Revalidating 条目的有界责任和仍有的系统引用
交给新操作对应的隔离记录，再移除原重核条目；原退役代的结果仍可重放。

系统归还开始前，清除该操作项用于匹配的映像地址。只有同一实际引用或仍在用的
原主引用可证明映像关联。未确认的返回依原已核文件、版本和受保护关系隔离，
旧地址不得用于匹配新装入的制品。已确认归还先记 ReleaseConfirmed，再锁外结束
文件持有，记录只作最终确认，不重新调用平台。

退出与维护（drop and maintenance）。普通装入、保留映像重核及冗余引用的已知
中断保留实际引用并隔离，均不排入维护释放。退役的系统动作尚未发生且文件仍在
原记录时，可将实际引用放回原记录，设 AwaitingRelease 继续原动作。默认退役已
移出所有核验文件且保留记录已预留时，文件和映像的局部对象在操作的析构前结束。
析构凭原 Retiring 条目和原代完成 Held 移交。系统已确认归还时只结束核验材料
及确认旧代。其它不可确认的阶段保留已知引用及失败。维护只取 AwaitingRelease。

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

    // 先核指定版本真实入口及配套库，再预留；缺入口 Unsupported，不发布输入。
    // @trace 6
    pub(crate) fn reserve_round(
        &'static self,
        epoch: u64,
        version: InitializationVersion,
        has_runtime: bool,
    ) -> Result<(InitIdV1, Connection)> {
        inner::authority_reserve_round(self, epoch, version, has_runtime)
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

    // 按旧代核对全部作用域及映像、文件持有；原结果可重放。
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

    // 预留的普通装入才调用平台；已登记复用切换到同一现有系统引用。
    // @trace 27
    pub(crate) fn open(&mut self) -> Result<()> {
        inner::system_open(self)
    }

    // @trace 28
    pub(crate) fn library(&self) -> Result<&NativeModule> {
        inner::system_library(self)
    }

    // 只有本次装入操作可记录从当前已核映像复制的完整身份。
    // 先保存事实，再由宿主核清单与请求；失败诊断不承担身份存储。
    // @trace 35
    pub(crate) fn record_metadata(&self, metadata: &Metadata) -> Result<()> {
        inner::system_record_metadata(self, metadata)
    }

    // 只在实际映像、完整身份及原持有仍相符时发布，或确认并返回原活动代。
    // @trace 29
    pub(crate) fn confirm_loaded(
        self,
        image: VerifiedImage,
        metadata: Metadata,
    ) -> Result<ModuleView> {
        inner::system_confirm_loaded(self, image, metadata)
    }

    // 锁外组合原错误，先按已知实际结果收录所持引用，停止未核对的地址。
    // @trace 30
    pub(crate) fn reject(self, cause: Error) -> Error {
        inner::system_reject(self, cause)
    }

    // 继续系统已确认归还的操作。只退还本次的核验材料并确认原记录。
    // 此入口的失败亦由原记录保全，供调用方和晚到请求取得。
    // @trace 37
    fn finish_confirmed_release(&mut self) -> Result<()> {
        inner::system_finish_confirmed_release(self)
    }

    // 在既有退役保护中保留引用或归还具备资格的引用，归还文件后确认旧代结束。
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
