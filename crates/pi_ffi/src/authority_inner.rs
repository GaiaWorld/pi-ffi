use super::*;
use crate::loader;
use pi_result::{attachment::Operation as ErrorOperation, ErrorKind};

pub(super) fn authority_process(build: HostBuild) -> Result<&'static Authority> {
    let build = Arc::new(build);
    let matches = {
        let mut state = AUTHORITY.state.lock();
        if let Some(current) = &state.build {
            **current == *build
        } else {
            state.build = Some(Arc::clone(&build));
            true
        }
    };
    if matches {
        Ok(&AUTHORITY)
    } else {
        Err(Error::new(ErrorKind::Conflict)
            .attach(pi_result::attachment::Operation("bind process host build")))
    }
}

pub(super) fn authority_build(authority: &Authority) -> Result<Arc<HostBuild>> {
    let build = authority.state.lock().build.clone();
    build.ok_or_else(|| {
        Error::new(ErrorKind::InvalidState)
            .attach(pi_result::attachment::Operation("read process host build"))
    })
}

pub(super) fn authority_reserve_load(
    authority: &'static Authority,
    files: Arc<VerifiedFiles>,
) -> Result<LoadAdmission> {
    let fallback = Arc::new(module_error(
        ErrorKind::Internal,
        "native load interrupted before confirmation",
        None,
    ));
    let identity = files.identity();
    let admission = {
        let mut state = authority.state.lock();
        if let Some(record) = state.modules.iter().find(|record| {
            record
                .files
                .as_ref()
                .is_some_and(|current| current.identity() == identity)
        }) {
            reuse_snapshot(record).map(LoadSelection::Reuse)
        } else {
            reserve_load_record(authority, &mut state, &files, &fallback).map(LoadSelection::New)
        }
    }
    .map_err(|(kind, failure)| module_error(kind, "reserve native load", failure))?;
    match admission {
        LoadSelection::New(operation) => Ok(LoadAdmission::New(operation)),
        LoadSelection::Reuse(original) => {
            compare_reuse(&original, &files)?;
            recheck_reuse(authority, &original)?;
            Ok(LoadAdmission::Reuse(original.view))
        }
    }
}

pub(super) fn authority_reserve_round(
    authority: &'static Authority,
    epoch: u64,
    has_runtime: bool,
) -> Result<(InitIdV1, Connection)> {
    todo!("短锁内核模块已装入并取同一份只读映像资料。锁外按 has_runtime 核实际配套库。检查期间插件可以退出。插件映像标识仅用于比较，查询只用当前系统枚举、实际文件和宿主持有的适配库地址。通过后以原模块代际、同一资料和已装入状态重新核对。同锁比较 UUID、完整版本和占用，准备唯一新轮。锁外用稳定的两项管理视图建立本地 Connection。此前检查失败不登记本轮及输入，登记后的失败保留实际已发生准备处置。")
}

pub(super) fn authority_publish_inputs(
    authority: &Authority,
    id: InitIdV1,
    inputs: PreparedInputs,
) -> Result<()> {
    todo!("核同一准备轮。一次安装已登记的三类实际输入，结果不明时延续原记录。")
}

pub(super) fn authority_fail_prepare(authority: &Authority, id: InitIdV1, cause: Error) -> Error {
    todo!("准备失败仍保留真实来源与已提交归还，清理确认后结束占用。返回原分类及阶段。")
}

pub(super) fn authority_current_round(
    authority: &Authority,
    epoch: u64,
) -> Result<Option<InitIdV1>> {
    todo!("按当前实际模块代际读尚活动或待清的本地轮次，结果脱离短锁。")
}

pub(super) fn authority_begin_round(
    authority: &'static Authority,
    id: InitIdV1,
    operation: Operation,
) -> Result<RoundAdmission> {
    todo!("核轮次和当前资格。Busy 不阻塞，确定结果可重放。需要调用时为唯一实际调用发令牌；尚未调用插件的 Prepared 轮次、明确 INIT_NOT_STARTED 或已成功关闭的轮次直接取得共同归还资格，跳过标准关闭。旧初始化回放不会改变真实进度。")
}

pub(super) fn authority_remember_progress(
    authority: &Authority,
    call: &RoundCall,
    progress: FfiProgress,
) {
    todo!("在实际输入和插件代码均仍保护时保存独立进度。记下已有效果不批准其它调用抢入。")
}

pub(super) fn authority_finish_round(
    authority: &Authority,
    call: RoundCall,
    result: Result<()>,
) -> Result<()> {
    todo!("在归还实际输入保护后归并本轮初始效果与诊断或归还问题，保留待处理状态并归还本次代码。处理外逃未确认按保守状态。")
}

pub(super) fn authority_prepare_unload(
    authority: &'static Authority,
    epoch: u64,
) -> Result<Option<SystemOperation>> {
    let fallback = Arc::new(module_error(
        ErrorKind::Internal,
        "native release interrupted before confirmation",
        None,
    ));
    let prepared = {
        let mut state = authority.state.lock();
        prepare_unload_record(authority, &mut state, epoch, &fallback)
    };
    prepared.map_err(|(kind, failure)| module_error(kind, "reserve native release", failure))
}

pub(super) fn authority_maintain(
    authority: &Authority,
    round: Option<InitIdV1>,
    budget: usize,
) -> Result<usize> {
    todo!("按既有记录有界推进。对象归还用预留编号，诊断归还取得单次资格。维护（maintenance）保留待办及在途编号；一次新推进换号。退役检查复用专用控制编号并独占来源。自动退役在独占准入成功的同一短锁下记录本次维护号。完整失败解除来源独占后，先复制诊断、提交原归还义务并结束调用。由本次维护锁内保存报告及清维护号，后续才派发实际诊断释放。其它维护不提前结束本轮失败。成功、Busy、有限或未确认结果也按实际进度结束本次维护号。锁外执行回调。已有明确定义的待系统释放可在锁外推进。先推进持有配置的插件诊断来源退出，再确认依赖的配置来源。确认原轮全部清理才结束版本占用。移出本轮发号登记，回收已结束调用回执。在锁外结束号码集合的持有，标准入口旧结果仍按原轮重放。保留不明释放及持有。")
}

pub(super) unsafe fn next_operation(context: *mut c_void, out: *mut OperationIdV1) -> u32 {
    todo!("本地保护有限状态回调。核真实作用域，同一短锁内分配非零不复用操作号并登记到本轮。完整号码记录实际领取方。只有登记成功才从输出槽交付，失败不写结果。")
}

pub(super) unsafe fn register_source(
    context: *mut c_void,
    operation: OperationIdV1,
    descriptor: SourceDescriptorV1,
    out: *mut u64,
) -> OutcomeV1 {
    todo!("保护完整回调。核操作号由真实同轮作用域领取，查本进程已接纳的相同号码及其用途和目标。核登记权限，已持调用保活时预留来源及自身代码持有。相同操作同输入回放，冲突失败，完成后才写来源号。")
}

pub(super) unsafe fn track_holding(context: *mut c_void, target: CallTargetV1) -> u32 {
    todo!("保护有限状态回调，按来源、实际轮次和 target 预留唯一释放义务、宿主操作编号及代码存续；重复登记同完整持有复用已有记录。独占时只允许本次检查线程和来源登记一个诊断，记录持有用于核返回。成功后才允许发布。")
}

pub(super) unsafe fn enter_call(
    context: *mut c_void,
    target: CallTargetV1,
    operation: OperationIdV1,
    rights: u64,
    out: *mut CallTokenV1,
) -> u32 {
    todo!("保护有限状态回调，核真实作用域领取的操作号、完整持有、来源和权限。已登记为其它动作或目标的同号返回 Conflict。按原操作、目标和权限重复确认同一逻辑调用，不新增令牌拥有者；改输入拒绝，已结束的请求不可重开。先登记活动资格再允许解引用。")
}

pub(super) unsafe fn leave_call(context: *mut c_void, token: CallTokenV1) -> u32 {
    todo!("保护有限状态回调，按原作用域、来源及令牌结束活动资格一次。已活动的归还仍可结束实际责任。完成回执在原有效作用域内重放，不持有代码。需要推进来源时保留待办；失败保留尚未确认为结束的保护。")
}

pub(super) unsafe fn request_release(context: *mut c_void, target: CallTargetV1) -> u32 {
    todo!(
        "保护有限状态回调，提交已有释放义务，重试不重复添加。无分配、无来源调用，无最终释放承诺。"
    )
}

pub(super) unsafe fn retire_source(
    context: *mut c_void,
    source: u64,
    operation: OperationIdV1,
) -> OutcomeV1 {
    todo!("保护完整回调，核原登记权威、真实领取该操作号的轮次、来源及同号的原动作。把同一号改投来源或改用途返回 Conflict。接纳时先记原请求和控制编号并关闭新业务。独占前保留合法读和清理。有其它活动调用或维护则 Busy。核实际结果与本次独占登记匹配，解除独占后再交合法诊断；终局失败由宿主记录重放，不借检查读取诊断；有限失败保留原类别，次级取首次有限结果的非零类别或 InvalidState，诊断字段全零。上下文归还仅一次，结果不明停用地址。")
}

pub(super) fn round_call_drop(call: &mut RoundCall) {
    todo!("尚未显式结束时保留已知进度或标明结果不明，归还已登记的适当令牌；不接触插件逻辑。")
}

pub(super) fn authority_validate_diagnostic(
    authority: &Authority,
    call: &RoundCall,
    outcome: &OutcomeV1,
) -> Result<()> {
    todo!("只接受本次真实模块和轮次已登记的诊断，核源、表、上下文和独立持有。未获准的地址不解引用。模块保活贯穿复制及归还。")
}

pub(super) fn authority_record_mapping(
    authority: &Authority,
    epoch: u64,
    mapped: MappedIdentity,
) -> Result<()> {
    let recorded = {
        let mut state = authority.state.lock();
        record_mapping(&mut state, epoch, mapped)
    };
    recorded.map_err(|(kind, failure)| module_error(kind, "record native mapping", failure))
}

pub(super) fn system_epoch(operation: &SystemOperation) -> u64 {
    operation.epoch
}

pub(super) fn system_open(operation: &mut SystemOperation) -> Result<()> {
    if !matches!(operation.phase, SystemPhase::Reserved) {
        return Err(module_error(
            ErrorKind::InvalidState,
            "open native reference",
            None,
        ));
    }
    operation.phase = SystemPhase::Opening;
    match loader::open(&operation.files) {
        Ok(module) => {
            operation.library = Some(module);
            operation.phase = SystemPhase::OwnsReference;
            Ok(())
        }
        Err(failure) => {
            let failure = Arc::new(failure);
            confirm_no_reference(operation, Some(Arc::clone(&failure)));
            Err(replay_module_failure(failure, "open native reference"))
        }
    }
}

pub(super) fn system_library(operation: &SystemOperation) -> Result<&NativeModule> {
    if let (SystemPhase::OwnsReference, Some(module)) = (&operation.phase, &operation.library) {
        return Ok(module);
    }
    Err(module_error(
        ErrorKind::InvalidState,
        "borrow native reference",
        None,
    ))
}

pub(super) fn system_confirm_loaded(
    operation: SystemOperation,
    image: VerifiedImage,
    metadata: Metadata,
) -> Result<ModuleView> {
    let mut operation = operation;
    let authority = operation.authority;
    let epoch = operation.epoch;
    let image = Arc::new(image);
    let metadata = Arc::new(metadata);
    let confirmation = {
        let mut state = operation.authority.state.lock();
        publish_loaded(&mut state, &mut operation, &image, &metadata)
    };
    match confirmation {
        Ok(LoadConfirmation::Published(obsolete_failure)) => {
            drop(obsolete_failure);
            Ok(ModuleView { epoch, metadata })
        }
        Ok(LoadConfirmation::Reuse(original)) => {
            if let Err(failure) = compare_reuse(&original, &operation.files) {
                return Err(operation.reject(failure));
            }
            operation.release()?;
            recheck_reuse(authority, &original)?;
            Ok(original.view)
        }
        Err((kind, failure)) => {
            Err(operation.reject(module_error(kind, "publish native module", failure)))
        }
    }
}

pub(super) fn system_reject(operation: SystemOperation, cause: Error) -> Error {
    let mut operation = operation;
    match operation.phase {
        SystemPhase::OwnsReference => {
            let original = Arc::new(cause);
            let failure = replay_module_failure(Arc::clone(&original), "reject native load");
            match release_reference(&mut operation, Some(original)) {
                Ok(()) => failure,
                Err(release_failure) => failure.attach(RecordedFailure(Arc::new(release_failure))),
            }
        }
        SystemPhase::Reserved => {
            let original = Arc::new(cause);
            confirm_no_reference(&mut operation, Some(Arc::clone(&original)));
            replay_module_failure(original, "reject native load")
        }
        SystemPhase::Opening | SystemPhase::ReleasingReference => {
            let original = Arc::new(cause);
            quarantine_mapping(&mut operation, Some(Arc::clone(&original)));
            replay_module_failure(original, "unconfirmed native result")
        }
        SystemPhase::Finished => cause,
    }
}

pub(super) fn system_release(operation: SystemOperation) -> Result<()> {
    let mut operation = operation;
    release_reference(&mut operation, None)
}

fn release_reference(
    operation: &mut SystemOperation,
    released_failure: Option<Arc<Error>>,
) -> Result<()> {
    if !matches!(operation.phase, SystemPhase::OwnsReference) {
        return Err(module_error(
            ErrorKind::InvalidState,
            "release native reference",
            None,
        ));
    }
    let Some(module) = operation.library.take() else {
        quarantine_mapping(operation, None);
        return Err(module_error(
            ErrorKind::Corrupted,
            "release native reference",
            None,
        ));
    };
    operation.phase = SystemPhase::ReleasingReference;
    match loader::close(module) {
        Ok(()) => {
            confirm_no_reference(operation, released_failure);
            Ok(())
        }
        Err(failure) => {
            let failure = Arc::new(failure);
            quarantine_mapping(operation, Some(Arc::clone(&failure)));
            Err(replay_module_failure(failure, "release native reference"))
        }
    }
}
pub(super) fn system_operation_drop(operation: &mut SystemOperation) {
    match operation.phase {
        SystemPhase::Finished => {}
        SystemPhase::Reserved => confirm_no_reference(operation, None),
        SystemPhase::Opening | SystemPhase::ReleasingReference => {
            quarantine_mapping(operation, None);
        }
        SystemPhase::OwnsReference => {
            let mut state = operation.authority.state.lock();
            if let Some(record) = state
                .modules
                .iter_mut()
                .find(|record| record.epoch == operation.epoch)
            {
                if record.library.is_none() && operation.library.is_some() {
                    record.library = operation.library.take();
                    if !matches!(record.phase, ModulePhase::Quarantined) {
                        record.phase = ModulePhase::AwaitingRelease;
                    }
                    operation.phase = SystemPhase::Finished;
                    return;
                }
            }
            drop(state);
            quarantine_mapping(operation, None);
        }
    }
}

type ModuleFailure = (ErrorKind, Option<Arc<Error>>);

fn module_error(kind: ErrorKind, operation: &'static str, failure: Option<Arc<Error>>) -> Error {
    let result = Error::new(kind).attach(ErrorOperation(operation));
    match failure {
        Some(previous) => {
            let copied = previous
                .downcast_ref::<Arc<pi_ffi_core::diagnostic::CopiedDiagnostic>>()
                .map(Arc::clone);
            let result = result.attach(RecordedFailure(previous));
            match copied {
                Some(copied) => result.attach(copied),
                None => result,
            }
        }
        None => result,
    }
}

#[derive(Debug)]
struct RecordedFailure(Arc<Error>);

impl std::fmt::Display for RecordedFailure {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.0.as_ref(), output)
    }
}

fn replay_module_failure(failure: Arc<Error>, operation: &'static str) -> Error {
    module_error(*failure.current_context(), operation, Some(failure))
}

fn issue_identifier(state: &mut State) -> std::result::Result<u64, ErrorKind> {
    let id = state.next_identifier;
    state.next_identifier = id.checked_add(1).ok_or(ErrorKind::ResourceExhausted)?;
    Ok(id)
}

fn module_unavailable(record: &ModuleRecord) -> ModuleFailure {
    match record.phase {
        ModulePhase::Loading | ModulePhase::Unloading | ModulePhase::AwaitingRelease => {
            (ErrorKind::Busy, None)
        }
        ModulePhase::Quarantined => {
            let kind = record
                .failure
                .as_ref()
                .map_or(ErrorKind::InvalidState, |failure| {
                    *failure.current_context()
                });
            (kind, record.failure.clone())
        }
        ModulePhase::Loaded | ModulePhase::Released => (ErrorKind::InvalidState, None),
    }
}

enum LoadSelection {
    New(SystemOperation),
    Reuse(ReuseSnapshot),
}

struct ReuseSnapshot {
    view: ModuleView,
    files: Arc<VerifiedFiles>,
    image: Arc<VerifiedImage>,
}

fn reuse_snapshot(record: &ModuleRecord) -> std::result::Result<ReuseSnapshot, ModuleFailure> {
    if !matches!(record.phase, ModulePhase::Loaded) {
        return Err(module_unavailable(record));
    }
    let (Some(files), Some(metadata), Some(mapped), Some(image), Some(_)) = (
        &record.files,
        &record.metadata,
        record.mapped,
        &record.image,
        &record.library,
    ) else {
        return Err((ErrorKind::Corrupted, None));
    };
    if mapped != image.identity {
        return Err((ErrorKind::Corrupted, None));
    }
    Ok(ReuseSnapshot {
        view: ModuleView {
            epoch: record.epoch,
            metadata: Arc::clone(metadata),
        },
        files: Arc::clone(files),
        image: Arc::clone(image),
    })
}

fn compare_reuse(original: &ReuseSnapshot, candidate: &VerifiedFiles) -> Result<()> {
    let actual = &original.view.metadata;
    let approved = candidate.manifest();
    if actual.uuid != approved.uuid || actual.version != approved.version {
        return Err(
            module_error(ErrorKind::Conflict, "match reused plugin metadata", None)
                .attach(format!("实际 UUID（actual UUID） {:02x?}", actual.uuid))
                .attach(format!("批准 UUID（approved UUID） {:02x?}", approved.uuid))
                .attach(format!("实际版本（actual version） {}", actual.version))
                .attach(format!("批准版本（approved version） {}", approved.version)),
        );
    }
    if !original.files.matches_reuse(candidate) {
        return Err(module_error(
            ErrorKind::Conflict,
            "match reused plugin deployment",
            None,
        ));
    }
    Ok(())
}

fn recheck_reuse(authority: &Authority, original: &ReuseSnapshot) -> Result<()> {
    let enrolled = {
        let state = authority.state.lock();
        state
            .modules
            .iter()
            .find(|record| record.epoch == original.view.epoch)
            .ok_or((ErrorKind::NotFound, None))
            .and_then(|record| {
                if !matches!(record.phase, ModulePhase::Loaded) {
                    return Err(module_unavailable(record));
                }
                if record.library.is_none()
                    || record.mapped != Some(original.image.identity)
                    || !record
                        .files
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &original.files))
                    || !record
                        .image
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &original.image))
                    || !record
                        .metadata
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &original.view.metadata))
                {
                    return Err((ErrorKind::InvalidState, None));
                }
                Ok(())
            })
    };
    enrolled.map_err(|(kind, failure)| module_error(kind, "recheck loaded plugin reuse", failure))
}

fn reserve_load_record(
    authority: &'static Authority,
    state: &mut State,
    files: &Arc<VerifiedFiles>,
    fallback: &Arc<Error>,
) -> std::result::Result<SystemOperation, ModuleFailure> {
    state
        .modules
        .try_reserve(1)
        .map_err(|_| (ErrorKind::ResourceExhausted, None))?;
    let epoch = issue_identifier(state).map_err(|kind| (kind, None))?;
    state.modules.push(ModuleRecord {
        epoch,
        files: Some(Arc::clone(files)),
        mapped: None,
        image: None,
        library: None,
        metadata: None,
        phase: ModulePhase::Loading,
        failure: Some(Arc::clone(fallback)),
    });
    Ok(SystemOperation {
        authority,
        epoch,
        files: Arc::clone(files),
        phase: SystemPhase::Reserved,
        library: None,
    })
}

fn record_mapping(
    state: &mut State,
    epoch: u64,
    mapped: MappedIdentity,
) -> std::result::Result<(), ModuleFailure> {
    let record = state
        .modules
        .iter_mut()
        .find(|record| record.epoch == epoch)
        .ok_or((ErrorKind::NotFound, None))?;
    if !matches!(record.phase, ModulePhase::Loading) {
        return Err(module_unavailable(record));
    }
    if record.mapped.is_some_and(|previous| previous != mapped) {
        return Err((ErrorKind::Corrupted, None));
    }
    record.mapped = Some(mapped);
    if let Some(other) = state.modules.iter().find(|record| {
        record.epoch != epoch
            && record.mapped == Some(mapped)
            && matches!(record.phase, ModulePhase::Quarantined)
    }) {
        return Err(module_unavailable(other));
    }
    Ok(())
}

enum LoadConfirmation {
    Published(Option<Arc<Error>>),
    Reuse(ReuseSnapshot),
}

fn publish_loaded(
    state: &mut State,
    operation: &mut SystemOperation,
    image: &Arc<VerifiedImage>,
    metadata: &Arc<Metadata>,
) -> std::result::Result<LoadConfirmation, ModuleFailure> {
    let epoch = operation.epoch;
    let index = state
        .modules
        .iter()
        .position(|record| record.epoch == epoch)
        .ok_or((ErrorKind::NotFound, None))?;
    let record = &state.modules[index];
    if !matches!(record.phase, ModulePhase::Loading) {
        return Err(module_unavailable(record));
    }
    if record.mapped != Some(image.identity)
        || !matches!(operation.phase, SystemPhase::OwnsReference)
    {
        return Err((ErrorKind::Corrupted, None));
    }
    if let Some(other) = state.modules.iter().find(|record| {
        record.epoch != epoch
            && record.mapped == Some(image.identity)
            && !matches!(record.phase, ModulePhase::Loading | ModulePhase::Released)
    }) {
        return reuse_snapshot(other).map(LoadConfirmation::Reuse);
    }
    let library = operation
        .library
        .take()
        .ok_or((ErrorKind::Corrupted, None))?;
    let record = &mut state.modules[index];
    record.image = Some(Arc::clone(image));
    record.metadata = Some(Arc::clone(metadata));
    record.library = Some(library);
    record.phase = ModulePhase::Loaded;
    operation.phase = SystemPhase::Finished;
    Ok(LoadConfirmation::Published(record.failure.take()))
}

fn prepare_unload_record(
    authority: &'static Authority,
    state: &mut State,
    epoch: u64,
    fallback: &Arc<Error>,
) -> std::result::Result<Option<SystemOperation>, ModuleFailure> {
    let record = state
        .modules
        .iter_mut()
        .find(|record| record.epoch == epoch)
        .ok_or((ErrorKind::NotFound, None))?;
    match record.phase {
        ModulePhase::Released => return Ok(None),
        ModulePhase::Loaded => {}
        _ => return Err(module_unavailable(record)),
    }
    if state
        .rounds
        .iter()
        .any(|round| round.id.module_epoch == epoch && !matches!(round.phase, RoundPhase::Closed))
        || state.sources.iter().any(|source| {
            source.id.module_epoch == epoch && !matches!(source.state, SourceState::Retired)
        })
        || state.holdings.iter().any(|holding| {
            holding.target.lease.initialization.module_epoch == epoch
                && !matches!(holding.phase, HoldingPhase::Returned)
        })
        || state.calls.iter().any(|call| {
            call.id.module_epoch == epoch
                && !matches!(call.token, CallIdentifier::Source { active: false, .. })
        })
    {
        return Err((ErrorKind::Busy, None));
    }
    if record.failure.is_some() {
        return Err((ErrorKind::Corrupted, None));
    }
    let Some(files) = record.files.clone() else {
        return Err((ErrorKind::Corrupted, None));
    };
    let Some(library) = record.library.take() else {
        return Err((ErrorKind::Corrupted, None));
    };
    record.phase = ModulePhase::Unloading;
    record.failure = Some(Arc::clone(fallback));
    Ok(Some(SystemOperation {
        authority,
        epoch,
        files,
        phase: SystemPhase::OwnsReference,
        library: Some(library),
    }))
}

fn confirm_no_reference(operation: &mut SystemOperation, result: Option<Arc<Error>>) {
    let files_and_image = {
        let mut state = operation.authority.state.lock();
        state
            .modules
            .iter_mut()
            .find(|record| record.epoch == operation.epoch)
            .map(|record| {
                record.phase = ModulePhase::Released;
                record.mapped = None;
                let previous = std::mem::replace(&mut record.failure, result);
                (record.files.take(), record.image.take(), previous)
            })
    };
    operation.phase = SystemPhase::Finished;
    drop(files_and_image);
}

fn quarantine_mapping(operation: &mut SystemOperation, failure: Option<Arc<Error>>) {
    let previous = {
        let mut state = operation.authority.state.lock();
        let Some(record) = state
            .modules
            .iter_mut()
            .find(|record| record.epoch == operation.epoch)
        else {
            operation.phase = SystemPhase::Finished;
            return;
        };
        record.phase = ModulePhase::Quarantined;
        let mapped = record.mapped;
        let previous = failure.map(|failure| record.failure.replace(failure));
        let retained = record.failure.clone();
        if let Some(mapped) = mapped {
            for other in &mut state.modules {
                if other.epoch != operation.epoch && other.mapped == Some(mapped) {
                    other.phase = ModulePhase::Quarantined;
                    if other.failure.is_none() {
                        other.failure = retained.clone();
                    }
                }
            }
        }
        previous
    };
    operation.phase = SystemPhase::Finished;
    drop(previous);
}
