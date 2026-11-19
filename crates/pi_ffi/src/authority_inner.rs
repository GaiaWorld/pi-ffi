use super::*;
use crate::loader;
use pi_ffi_abi::{
    InterfaceViewV1, MaintenanceV1, SourceControlApiV1, SOURCE_CONTROL_MAJOR, TARGET_DIAGNOSTIC,
    TARGET_OBJECT,
};
use pi_ffi_core::{boundary, diagnostic};
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
    let image = {
        let state = authority.state.lock();
        ready_for_prepare(&state, epoch).map(|(image, _)| Arc::clone(image))
    }
    .map_err(|(kind, failure)| {
        module_error(kind, "check initialization input libraries", failure)
    })?;
    loader::check_inputs(&image, has_runtime)?;

    let id = {
        let mut state = authority.state.lock();
        reserve_round_record(&mut state, epoch, &image)
    }
    .map_err(|(kind, failure)| module_error(kind, "reserve plugin initialization", failure))?;

    let opened = boundary::catch(
        boundary::Context {
            operation: "create initialization control scopes",
            module_epoch: id.module_epoch,
            round: id.round,
            source: 0,
        },
        || round_connection(authority, &id),
    );
    let connection = match opened {
        Ok(connection) => connection,
        Err(failure) => return Err(authority.fail_prepare(id, failure)),
    };

    let installed = {
        let mut state = authority.state.lock();
        state
            .rounds
            .iter_mut()
            .find(|round| same_round(&round.id, &id))
            .is_some_and(|round| {
                if !matches!(round.phase, RoundPhase::Preparing) || round.connection.is_some() {
                    return false;
                }
                round.connection = Some(connection.clone());
                true
            })
    };
    if !installed {
        return Err(authority.fail_prepare(
            id,
            module_error(
                ErrorKind::InvalidState,
                "publish initialization control scopes",
                None,
            ),
        ));
    }
    Ok((id, connection))
}

pub(super) fn authority_publish_inputs(
    authority: &Authority,
    id: InitIdV1,
    inputs: PreparedInputs,
) -> Result<()> {
    let inputs = Arc::new(inputs);
    let published = {
        let mut state = authority.state.lock();
        state
            .rounds
            .iter_mut()
            .find(|round| same_round(&round.id, &id))
            .is_some_and(|round| {
                if !matches!(round.phase, RoundPhase::Preparing)
                    || round.connection.is_none()
                    || round.inputs.is_some()
                {
                    return false;
                }
                round.inputs = Some(Arc::clone(&inputs));
                round.phase = RoundPhase::Prepared;
                true
            })
    };
    if published {
        Ok(())
    } else {
        Err(module_error(
            ErrorKind::InvalidState,
            "publish initialization inputs",
            None,
        ))
    }
}

pub(super) fn authority_fail_prepare(authority: &Authority, id: InitIdV1, cause: Error) -> Error {
    let cause = Arc::new(cause);
    {
        let mut state = authority.state.lock();
        if let Some(round) = state
            .rounds
            .iter_mut()
            .find(|round| same_round(&round.id, &id))
        {
            if matches!(round.phase, RoundPhase::Preparing) && round.init_failure.is_none() {
                round.init_failure = Some(Arc::clone(&cause));
                round.ffi_progress = Some(FfiProgress::InitializationNotStarted);
                round.phase = RoundPhase::Rejected;
            }
        }
    }
    replay_module_failure(cause, "prepare plugin initialization")
}

pub(super) fn authority_current_round(
    authority: &Authority,
    epoch: u64,
) -> Result<Option<InitIdV1>> {
    let current = {
        let state = authority.state.lock();
        if state.modules.iter().any(|module| module.epoch == epoch) {
            Ok(state.rounds.iter().find_map(|round| {
                (round.id.module_epoch == epoch && !matches!(round.phase, RoundPhase::Closed))
                    .then_some(InitIdV1 {
                        module_epoch: epoch,
                        round: round.id.round,
                    })
            }))
        } else {
            Err(ErrorKind::NotFound)
        }
    };
    current.map_err(|kind| module_error(kind, "read current initialization", None))
}

pub(super) fn authority_begin_round(
    authority: &'static Authority,
    id: InitIdV1,
    operation: Operation,
) -> Result<RoundAdmission> {
    let fallback = Arc::new(module_error(
        ErrorKind::Internal,
        "plugin standard call interrupted",
        None,
    ));
    let unstarted = Arc::new(module_error(
        ErrorKind::InvalidState,
        "initialization closed before starting",
        None,
    ));
    let admission = {
        let mut state = authority.state.lock();
        begin_round_call(&mut state, authority, &id, operation, &fallback, &unstarted)
    };
    let (admission, released) = admission
        .map_err(|(kind, failure)| module_error(kind, "reserve plugin standard call", failure))?;
    drop(released);
    Ok(admission)
}

pub(super) fn authority_remember_progress(
    authority: &Authority,
    call: &RoundCall,
    progress: FfiProgress,
) {
    let mut state = authority.state.lock();
    if active_standard_call(&state, call).is_some() {
        if let Some(round) = state
            .rounds
            .iter_mut()
            .find(|round| same_round(&round.id, &call.id))
        {
            round.ffi_progress = Some(progress);
        }
    }
}

pub(super) fn authority_finish_round(
    authority: &Authority,
    call: RoundCall,
    result: Result<()>,
) -> Result<()> {
    let mut call = call;
    let failure = result.err().map(Arc::new);
    let finished = {
        let mut state = authority.state.lock();
        finish_standard_call(&mut state, &mut call, &failure)
    };
    let (old, inputs) =
        finished.map_err(|kind| module_error(kind, "settle plugin standard call", None))?;
    drop(old);
    drop(inputs);
    match failure {
        Some(failure) => Err(replay_module_failure(failure, "plugin standard call")),
        None => Ok(()),
    }
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
    if !std::ptr::eq(authority, &AUTHORITY) {
        return Err(module_error(
            ErrorKind::Conflict,
            "maintain plugin authority",
            None,
        ));
    }
    let authority = &AUTHORITY;
    let interruption = Arc::new(module_error(
        ErrorKind::Internal,
        "source maintenance interrupted",
        None,
    ));
    let mut visited = Vec::new();
    for processed in 0..budget {
        visited.try_reserve(1).map_err(|_| {
            module_error(
                ErrorKind::ResourceExhausted,
                "record maintenance work",
                None,
            )
        })?;
        let task = {
            let mut state = authority.state.lock();
            next_maintenance(authority, &mut state, round.as_ref(), &visited)
        }
        .map_err(|(kind, failure)| module_error(kind, "reserve plugin maintenance", failure))?;
        let Some(task) = task else {
            return Ok(processed);
        };
        if let Some(key) = task.source_step() {
            visited.push(key);
        }
        run_maintenance(authority, task, &interruption)?;
    }
    Ok(budget)
}

pub(super) unsafe fn next_operation(context: *mut c_void, out: *mut OperationIdV1) -> u32 {
    diagnostic::protect_status(callback_context("issue host operation"), || {
        callback_status((|| {
            if out.is_null() || !out.is_aligned() {
                return Err(ErrorKind::InvalidInput);
            }
            // 安全前提（safety）。管理连接保护本次回调的真实上下文及输出槽。
            let scope = unsafe { callback_scope(context) }?;
            let operation = {
                let mut state = scope.authority.state.lock();
                let index = live_round_index(&state, scope)?;
                state.rounds[index]
                    .issued_operations
                    .try_reserve(1)
                    .map_err(|_| ErrorKind::ResourceExhausted)?;
                let operation = OperationIdV1 {
                    actor: scope.actor,
                    sequence: issue_identifier(&mut state)?,
                };
                state.rounds[index]
                    .issued_operations
                    .push(copy_operation(&operation));
                operation
            };
            // 安全前提（safety）。调用方提供独占输出槽（output slot），号码已登记到本轮。
            unsafe { out.write(operation) };
            Ok(())
        })())
    })
}

pub(super) unsafe fn register_source(
    context: *mut c_void,
    operation: OperationIdV1,
    descriptor: SourceDescriptorV1,
    out: *mut u64,
) -> OutcomeV1 {
    diagnostic::protect_outcome(callback_context("register native source"), || {
        let result = (|| {
            if out.is_null() || !out.is_aligned() || !valid_descriptor(&descriptor) {
                return Err(ErrorKind::InvalidInput);
            }
            // 安全前提（safety）。传入上下文由当前管理连接保活。
            let scope = unsafe { callback_scope(context) }?;
            let source = {
                let mut state = scope.authority.state.lock();
                register_source_record(&mut state, scope, operation, descriptor)?
            };
            // 安全前提（safety）。登记已确认，输出槽由调用方独占。
            unsafe { out.write(source) };
            Ok(())
        })();
        match result {
            Ok(()) => diagnostic::success_outcome(),
            Err(kind) => diagnostic::limited_outcome(kind, ErrorKind::InvalidState),
        }
    })
}

pub(super) unsafe fn track_holding(context: *mut c_void, target: CallTargetV1) -> u32 {
    diagnostic::protect_status(callback_context("track source holding"), || {
        // 安全前提（safety）。管理连接保持作用域，目标本身只按记录比较。
        callback_status(unsafe { callback_scope(context) }.and_then(|scope| {
            let mut state = scope.authority.state.lock();
            track_holding_record(&mut state, scope, target)
        }))
    })
}

pub(super) unsafe fn enter_call(
    context: *mut c_void,
    target: CallTargetV1,
    operation: OperationIdV1,
    rights: u64,
    out: *mut CallTokenV1,
) -> u32 {
    diagnostic::protect_status(callback_context("enter source call"), || {
        callback_status((|| {
            if out.is_null() || !out.is_aligned() || rights & !7 != 0 {
                return Err(ErrorKind::InvalidInput);
            }
            // 安全前提（safety）。连接保持上下文，调用方提供独占输出槽。
            let scope = unsafe { callback_scope(context) }?;
            let token = {
                let mut state = scope.authority.state.lock();
                enter_source_call(&mut state, scope, target, operation, rights)?
            };
            // 安全前提（safety）。已取得完整令牌，输出槽仍在调用方保护中。
            unsafe { out.write(token) };
            Ok(())
        })())
    })
}

pub(super) unsafe fn leave_call(context: *mut c_void, token: CallTokenV1) -> u32 {
    diagnostic::protect_status(callback_context("leave source call"), || {
        // 安全前提（safety）。连接保持原作用域到已登记调用归还，令牌不解引用。
        callback_status(unsafe { callback_scope(context) }.and_then(|scope| {
            let mut state = scope.authority.state.lock();
            leave_source_call(&mut state, scope, token)
        }))
    })
}

pub(super) unsafe fn request_release(context: *mut c_void, target: CallTargetV1) -> u32 {
    diagnostic::protect_status(callback_context("request source holding release"), || {
        // 安全前提（safety）。管理连接保护当前作用域，目标只按真实记录核对。
        callback_status(unsafe { callback_scope(context) }.and_then(|scope| {
            let mut state = scope.authority.state.lock();
            let index = target_source_index(&state, &scope.id, &target)?;
            let holding = state
                .holdings
                .iter_mut()
                .find(|holding| {
                    holding.target.kind == target.kind
                        && same_lease(&holding.target.lease, &target.lease)
                })
                .ok_or(ErrorKind::NotFound)?;
            match holding.phase {
                HoldingPhase::Tracked => {
                    holding.phase = HoldingPhase::Requested;
                    state.sources[index].maintenance_pending = true;
                    Ok(())
                }
                HoldingPhase::Requested | HoldingPhase::Releasing | HoldingPhase::Returned => {
                    Ok(())
                }
                HoldingPhase::Quarantined => Err(holding
                    .failure
                    .as_ref()
                    .map_or(ErrorKind::InvalidState, |failure| {
                        *failure.current_context()
                    })),
            }
        }))
    })
}

pub(super) unsafe fn retire_source(
    context: *mut c_void,
    source: u64,
    operation: OperationIdV1,
) -> OutcomeV1 {
    diagnostic::protect_outcome(callback_context("retire registered source"), || {
        // 安全前提（safety）。来源和管理连接共同保护本次调用的稳定作用域。
        let scope = match unsafe { callback_scope(context) } {
            Ok(scope) => scope,
            Err(kind) => return diagnostic::limited_outcome(kind, ErrorKind::InvalidState),
        };
        let failed = Arc::new(module_error(
            ErrorKind::Internal,
            "source retirement interrupted",
            None,
        ));
        let admission = {
            let mut state = scope.authority.state.lock();
            admit_retirement(&mut state, source, Some((scope, operation)))
        };
        finish_retirement_admission(scope.authority, admission, failed)
    })
}

pub(super) fn round_call_drop(call: &mut RoundCall) {
    if call.finished {
        return;
    }
    let released = {
        let mut state = call.authority.state.lock();
        let Some(index) = active_standard_call(&state, call) else {
            return;
        };
        state.calls.swap_remove(index);
        let Some(round) = state
            .rounds
            .iter_mut()
            .find(|round| same_round(&round.id, &call.id))
        else {
            return;
        };
        match round.phase {
            RoundPhase::Initializing => {
                round.phase = if matches!(
                    round.ffi_progress,
                    None | Some(FfiProgress::InitializationNotStarted)
                ) {
                    RoundPhase::Rejected
                } else {
                    RoundPhase::NeedsClose
                };
                (None, None)
            }
            RoundPhase::Closing => match round.ffi_progress {
                Some(FfiProgress::CloseCompleted) => {
                    round.phase = RoundPhase::ReturningHoldings;
                    (None, round.inputs.take())
                }
                None => {
                    round.phase = RoundPhase::NeedsClose;
                    (round.close_failure.take(), None)
                }
                _ => {
                    round.phase = RoundPhase::CleanupFailed;
                    (None, None)
                }
            },
            _ => (None, None),
        }
    };
    call.finished = true;
    drop(released);
}

pub(super) fn authority_validate_diagnostic(
    authority: &Authority,
    call: &RoundCall,
    outcome: &OutcomeV1,
) -> Result<()> {
    if outcome.kind == 0 || outcome.diagnostic_failure_kind != 0 {
        return Ok(());
    }
    let reference = &outcome.diagnostic;
    let target = CallTargetV1 {
        context: reference.context,
        api: reference.api.cast(),
        lease: holding_lease(&reference.lease),
        kind: TARGET_DIAGNOSTIC,
    };
    let valid = {
        let state = authority.state.lock();
        active_standard_call(&state, call).is_some()
            && target_source_index(&state, &call.id, &target).is_ok_and(|source| {
                state.sources[source].owner
                    == (CodeOwner::Plugin {
                        epoch: call.id.module_epoch,
                    })
                    && matches!(
                        state.sources[source].state,
                        SourceState::Registered
                            | SourceState::Retiring
                            | SourceState::RetirementFailed { .. },
                    )
                    && state.holdings.iter().any(|holding| {
                        holding.target.kind == TARGET_DIAGNOSTIC
                            && same_lease(&holding.target.lease, &target.lease)
                            && matches!(holding.phase, HoldingPhase::Tracked)
                    })
            })
    };
    if valid {
        Ok(())
    } else {
        Err(module_error(
            ErrorKind::Corrupted,
            "validate plugin diagnostic ownership",
            None,
        ))
    }
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
    let files = operation.files.as_deref().ok_or_else(|| {
        module_error(ErrorKind::Corrupted, "read reserved module files", None)
    })?;
    operation.phase = SystemPhase::Opening;
    match loader::open(files) {
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

pub(super) fn system_record_metadata(
    operation: &SystemOperation,
    metadata: &Metadata,
) -> Result<()> {
    todo!("只接受拥有实际引用且映像归属已经核实的本次装入操作。在锁外准备宿主元信息副本，短锁内校验原代仍在装入中且映像匹配，再保存完整 UUID 和版本。接受相同值的重放；不同值报告 Conflict。锁外结束未采用副本。此记录不授予调用许可。")
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
            let Some(files) = operation.files.as_deref() else {
                let failure = module_error(ErrorKind::Corrupted, "read reserved module files", None);
                return Err(operation.reject(failure));
            };
            if let Err(failure) = compare_reuse(&original, files) {
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
        SystemPhase::ReleaseConfirmed => match operation.finish_confirmed_release() {
            Ok(()) => cause,
            Err(failure) => cause.attach(RecordedFailure(Arc::new(failure))),
        },
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

pub(super) fn system_finish_confirmed_release(operation: &mut SystemOperation) -> Result<()> {
    todo!("仅处理原 SystemOperation 的 ReleaseConfirmed。确认归还事实和原用途，不再调用加载器。在短锁内移出本操作及对应记录的核验文件和映像资料，锁外归还所有持有，回锁确认该代结束。若原登记不相符，保存已确认的系统结果和本地失败，停用该记录的地址。成功仅发布一次，重放保持结果。")
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
        SystemPhase::ReleaseConfirmed => {
            // 原记录仍保存归还确认。补完资料及记录步骤，失败也保存在原记录。
            drop(operation.finish_confirmed_release());
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
        files: Some(Arc::clone(files)),
        purpose: SystemPurpose::Load,
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
        files: Some(files),
        purpose: SystemPurpose::Retire,
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

fn same_round(left: &InitIdV1, right: &InitIdV1) -> bool {
    left.module_epoch == right.module_epoch && left.round == right.round
}

fn round_id(id: &InitIdV1) -> InitIdV1 {
    InitIdV1 {
        module_epoch: id.module_epoch,
        round: id.round,
    }
}

fn ready_for_prepare(
    state: &State,
    epoch: u64,
) -> std::result::Result<(&Arc<VerifiedImage>, &Metadata), ModuleFailure> {
    let record = state
        .modules
        .iter()
        .find(|record| record.epoch == epoch)
        .ok_or((ErrorKind::NotFound, None))?;
    if !matches!(record.phase, ModulePhase::Loaded) {
        return Err(module_unavailable(record));
    }
    match (&record.image, &record.metadata) {
        (Some(image), Some(metadata)) => Ok((image, metadata)),
        _ => Err((ErrorKind::Corrupted, None)),
    }
}

fn reserve_round_record(
    state: &mut State,
    epoch: u64,
    checked: &Arc<VerifiedImage>,
) -> std::result::Result<InitIdV1, ModuleFailure> {
    let (image, metadata) = ready_for_prepare(state, epoch)?;
    if !Arc::ptr_eq(image, checked) {
        return Err((ErrorKind::InvalidState, None));
    }
    let occupied = state
        .rounds
        .iter()
        .filter(|round| !matches!(round.phase, RoundPhase::Closed))
        .any(|round| {
            state
                .modules
                .iter()
                .find(|record| record.epoch == round.id.module_epoch)
                .and_then(|record| record.metadata.as_deref())
                .is_some_and(|current| current == metadata)
        });
    if occupied {
        return Err((ErrorKind::Busy, None));
    }
    state
        .rounds
        .try_reserve(1)
        .map_err(|_| (ErrorKind::ResourceExhausted, None))?;
    let id = InitIdV1 {
        module_epoch: epoch,
        round: issue_identifier(state).map_err(|kind| (kind, None))?,
    };
    state.rounds.push(RoundRecord {
        id: round_id(&id),
        issued_operations: Vec::new(),
        phase: RoundPhase::Preparing,
        inputs: None,
        connection: None,
        ffi_progress: None,
        init_failure: None,
        close_failure: None,
    });
    Ok(id)
}

fn round_connection(authority: &'static Authority, id: &InitIdV1) -> Result<Connection> {
    let scopes = Arc::new(CallbackScopes {
        host: CallbackScope {
            authority,
            actor: 0,
            id: round_id(id),
            owner: CodeOwner::Host,
        },
        plugin: CallbackScope {
            authority,
            actor: id.module_epoch,
            id: round_id(id),
            owner: CodeOwner::Plugin {
                epoch: id.module_epoch,
            },
        },
    });
    let table = std::ptr::from_ref(&CONTROL).cast();
    let host_view = InterfaceViewV1 {
        context: std::ptr::from_ref(&scopes.host).cast_mut().cast(),
        table,
    };
    let plugin_view = InterfaceViewV1 {
        context: std::ptr::from_ref(&scopes.plugin).cast_mut().cast(),
        table,
    };
    // 安全前提（safety）。进程静态表只解释由 scopes 保活的两个稳定上下文。
    // 能力分别绑定宿主和本轮实际插件，连接的原生持有一直保护两者。
    unsafe { Connection::from_native(host_view, plugin_view, round_id(id), scopes) }
}

fn callback_context(operation: &'static str) -> boundary::Context {
    boundary::Context {
        operation,
        module_epoch: 0,
        round: 0,
        source: 0,
    }
}

fn callback_status(result: std::result::Result<(), ErrorKind>) -> u32 {
    result.err().map_or(0, diagnostic::status_code)
}

unsafe fn callback_scope<'call>(
    context: *mut c_void,
) -> std::result::Result<&'call CallbackScope, ErrorKind> {
    let context = context.cast::<CallbackScope>();
    if context.is_null() || !context.is_aligned() {
        return Err(ErrorKind::InvalidInput);
    }
    // 安全前提（safety）。管理表只接受现有 Connection 保活的 CallbackScope。
    // 此借用不会离开当前同步的管理回调。
    let scope = unsafe { &*context };
    let valid_actor = match scope.owner {
        CodeOwner::Host => scope.actor == 0,
        CodeOwner::Plugin { epoch } => scope.actor == epoch && scope.id.module_epoch == epoch,
    };
    if !std::ptr::eq(scope.authority, &AUTHORITY) || !valid_actor {
        return Err(ErrorKind::Conflict);
    }
    Ok(scope)
}

fn live_round_index(
    state: &State,
    scope: &CallbackScope,
) -> std::result::Result<usize, ErrorKind> {
    let index = state
        .rounds
        .iter()
        .position(|round| same_round(&round.id, &scope.id))
        .ok_or(ErrorKind::NotFound)?;
    if matches!(state.rounds[index].phase, RoundPhase::Closed) {
        return Err(ErrorKind::InvalidState);
    }
    if !state.modules.iter().any(|module| {
        module.epoch == scope.id.module_epoch && matches!(module.phase, ModulePhase::Loaded)
    }) {
        return Err(ErrorKind::InvalidState);
    }
    Ok(index)
}

fn valid_descriptor(descriptor: &SourceDescriptorV1) -> bool {
    !descriptor.context.is_null()
        && !descriptor.control.is_null()
        && descriptor.control.is_aligned()
        && (descriptor.objects.is_aligned() && descriptor.diagnostics.is_aligned())
        && !(descriptor.objects.is_null() && descriptor.diagnostics.is_null())
}

fn same_descriptor(left: &SourceDescriptorV1, right: &SourceDescriptorV1) -> bool {
    left.context == right.context
        && left.objects == right.objects
        && left.diagnostics == right.diagnostics
        && left.control == right.control
}

fn same_operation(left: &OperationIdV1, right: &OperationIdV1) -> bool {
    left.actor == right.actor && left.sequence == right.sequence
}

fn verify_operation(
    round: &RoundRecord,
    scope: &CallbackScope,
    operation: &OperationIdV1,
) -> std::result::Result<(), ErrorKind> {
    // 本轮号码在发出时按全局顺序追加（append），所以可用序号查找完整标识。
    let issued_here = round
        .issued_operations
        .binary_search_by_key(&operation.sequence, |issued| issued.sequence)
        .is_ok_and(|index| same_operation(&round.issued_operations[index], operation));
    if operation.actor == scope.actor && issued_here {
        Ok(())
    } else {
        Err(ErrorKind::Conflict)
    }
}

fn operation_claimed(state: &State, round: &InitIdV1, operation: &OperationIdV1) -> bool {
    state
        .sources
        .iter()
        .filter(|record| same_round(&record.id, round))
        .any(|record| {
            same_operation(&record.registration_operation, operation)
                || record
                    .retirement
                    .as_ref()
                    .and_then(|retirement| retirement.request.as_ref())
                    .is_some_and(|request| same_operation(request, operation))
        })
        || state.calls.iter().any(|record| {
            same_round(&record.id, round)
                && matches!(&record.token, CallIdentifier::Source { operation: previous, .. }
                    if same_operation(previous, operation))
        })
}

fn register_source_record(
    state: &mut State,
    scope: &CallbackScope,
    operation: OperationIdV1,
    descriptor: SourceDescriptorV1,
) -> std::result::Result<u64, ErrorKind> {
    let round = &state.rounds[live_round_index(state, scope)?];
    verify_operation(round, scope, &operation)?;
    if let Some(previous) = state.sources.iter().find(|source| {
        same_round(&source.id, &scope.id)
            && same_operation(&source.registration_operation, &operation)
    })
    {
        if previous.owner == scope.owner && same_descriptor(&previous.descriptor, &descriptor) {
            return Ok(previous.source);
        }
        return Err(ErrorKind::Conflict);
    }
    if operation_claimed(state, &scope.id, &operation) {
        return Err(ErrorKind::Conflict);
    }
    let allowed = match scope.owner {
        CodeOwner::Host => matches!(round.phase, RoundPhase::Preparing),
        CodeOwner::Plugin { epoch } => {
            epoch == scope.id.module_epoch
                && matches!(
                    round.phase,
                    RoundPhase::Initializing | RoundPhase::Initialized
                )
        }
    };
    if !allowed {
        return Err(ErrorKind::InvalidState);
    }
    if state.sources.iter().any(|other| {
        !matches!(other.state, SourceState::Retired)
            && other.descriptor.context == descriptor.context
    }) {
        return Err(ErrorKind::Conflict);
    }
    state
        .sources
        .try_reserve(1)
        .map_err(|_| ErrorKind::ResourceExhausted)?;
    let source = issue_identifier(state)?;
    state.sources.push(RegisteredSource {
        owner: scope.owner,
        id: round_id(&scope.id),
        source,
        descriptor,
        registration_operation: operation,
        retirement: None,
        maintenance_pending: false,
        maintenance_operation: None,
        state: SourceState::Registered,
        failure: None,
    });
    Ok(source)
}

fn holding_lease(lease: &LeaseV1) -> LeaseV1 {
    LeaseV1 {
        object: pi_ffi_abi::ObjectKeyV1 {
            source: lease.object.source,
            slot: lease.object.slot,
            generation: lease.object.generation,
        },
        initialization: round_id(&lease.initialization),
        holding: lease.holding,
    }
}

fn same_lease(left: &LeaseV1, right: &LeaseV1) -> bool {
    left.object.source == right.object.source
        && left.object.slot == right.object.slot
        && left.object.generation == right.object.generation
        && left.holding == right.holding
        && same_round(&left.initialization, &right.initialization)
}

fn target_source_index(
    state: &State,
    id: &InitIdV1,
    target: &CallTargetV1,
) -> std::result::Result<usize, ErrorKind> {
    let lease = &target.lease;
    if lease.object.source == 0
        || lease.object.slot == 0
        || lease.object.generation == 0
        || lease.holding == 0
        || target.context.is_null()
        || target.api.is_null()
    {
        return Err(ErrorKind::InvalidInput);
    }
    if !same_round(id, &lease.initialization) {
        return Err(ErrorKind::Conflict);
    }
    let index = state
        .sources
        .iter()
        .position(|source| source.source == lease.object.source)
        .ok_or(ErrorKind::NotFound)?;
    let source = &state.sources[index];
    let table = match target.kind {
        TARGET_OBJECT => source.descriptor.objects.cast::<c_void>(),
        TARGET_DIAGNOSTIC => source.descriptor.diagnostics.cast::<c_void>(),
        _ => return Err(ErrorKind::InvalidInput),
    };
    if source.descriptor.context != target.context
        || table != target.api
        || !same_round(id, &source.id)
    {
        return Err(ErrorKind::Conflict);
    }
    Ok(index)
}

fn track_holding_record(
    state: &mut State,
    scope: &CallbackScope,
    target: CallTargetV1,
) -> std::result::Result<(), ErrorKind> {
    live_round_index(state, scope)?;
    let source_index = target_source_index(state, &scope.id, &target)?;
    let source = &state.sources[source_index];
    if source.owner != scope.owner {
        return Err(ErrorKind::PermissionDenied);
    }
    if let Some(previous) = state
        .holdings
        .iter()
        .find(|record| same_lease(&record.target.lease, &target.lease))
    {
        return if previous.target.kind == target.kind {
            Ok(())
        } else {
            Err(ErrorKind::Conflict)
        };
    }
    let exclusive = match &source.state {
        SourceState::Registered => false,
        SourceState::Retiring | SourceState::RetirementFailed { .. }
            if target.kind == TARGET_DIAGNOSTIC =>
        {
            false
        }
        SourceState::CheckingRetirement {
            thread,
            diagnostic: None,
        } if target.kind == TARGET_DIAGNOSTIC && *thread == std::thread::current().id() => true,
        _ => return Err(ErrorKind::InvalidState),
    };
    state
        .holdings
        .try_reserve(1)
        .map_err(|_| ErrorKind::ResourceExhausted)?;
    let release_operation = OperationIdV1 {
        actor: 0,
        sequence: issue_identifier(state)?,
    };
    if exclusive {
        if let SourceState::CheckingRetirement { diagnostic, .. } =
            &mut state.sources[source_index].state
        {
            *diagnostic = Some(HoldingKey {
                lease: holding_lease(&target.lease),
                kind: target.kind,
            });
        }
    }
    state.holdings.push(HoldingRecord {
        target: HoldingKey {
            lease: target.lease,
            kind: target.kind,
        },
        release_operation,
        phase: HoldingPhase::Tracked,
        failure: None,
    });
    Ok(())
}

fn enter_source_call(
    state: &mut State,
    scope: &CallbackScope,
    target: CallTargetV1,
    operation: OperationIdV1,
    rights: u64,
) -> std::result::Result<CallTokenV1, ErrorKind> {
    let round = &state.rounds[live_round_index(state, scope)?];
    verify_operation(round, scope, &operation)?;
    if let Some(record) = state.calls.iter().find(|call| {
        same_round(&call.id, &scope.id)
            && matches!(&call.token, CallIdentifier::Source { operation: previous, .. }
                if same_operation(previous, &operation))
    }) {
        if record.actor != scope.actor {
            return Err(ErrorKind::Conflict);
        }
        target_source_index(state, &scope.id, &target).map_err(|_| ErrorKind::Conflict)?;
        if let (CallIdentifier::Source { token, rights: known_rights, active, .. }, Some(known)) =
            (&record.token, &record.target)
        {
            if known.kind != target.kind || !same_lease(&known.lease, &target.lease) || *known_rights != rights {
                return Err(ErrorKind::Conflict);
            }
            return if *active { Ok(*token) } else { Err(ErrorKind::InvalidState) };
        }
        return Err(ErrorKind::Corrupted);
    }

    if operation_claimed(state, &scope.id, &operation) {
        return Err(ErrorKind::Conflict);
    }

    let source_index = target_source_index(state, &scope.id, &target)?;
    let source = &state.sources[source_index];
    if source.failure.is_some() && rights & 2 != 0 {
        return Err(ErrorKind::InvalidState);
    }
    match source.state {
        SourceState::Registered => {}
        SourceState::Retiring | SourceState::RetirementFailed { .. } if rights & 2 == 0 => {}
        SourceState::CheckingRetirement { .. } => return Err(ErrorKind::Busy),
        _ => return Err(ErrorKind::InvalidState),
    }
    let active_holding = state.holdings.iter().any(|record| {
        record.target.kind == target.kind
            && same_lease(&record.target.lease, &target.lease)
            && matches!(record.phase, HoldingPhase::Tracked)
    });
    if !active_holding {
        return Err(ErrorKind::InvalidState);
    }
    let owner = source.owner;
    state
        .calls
        .try_reserve(1)
        .map_err(|_| ErrorKind::ResourceExhausted)?;
    let token = CallTokenV1 {
        source: target.lease.object.source,
        call: issue_identifier(state)?,
    };
    state.calls.push(CallRecord {
        token: CallIdentifier::Source {
            token,
            operation,
            rights,
            active: true,
        },
        actor: scope.actor,
        owner,
        id: round_id(&scope.id),
        target: Some(HoldingKey {
            lease: target.lease,
            kind: target.kind,
        }),
    });
    Ok(token)
}

fn leave_source_call(
    state: &mut State,
    scope: &CallbackScope,
    token: CallTokenV1,
) -> std::result::Result<(), ErrorKind> {
    let record = state
        .calls
        .iter_mut()
        .find(|call| {
            matches!(&call.token, CallIdentifier::Source { token: registered, .. }
            if registered.source == token.source && registered.call == token.call)
        })
        .ok_or(ErrorKind::NotFound)?;
    if record.actor != scope.actor || !same_round(&record.id, &scope.id) {
        return Err(ErrorKind::Conflict);
    }
    if let CallIdentifier::Source { active, .. } = &mut record.token {
        if !*active {
            live_round_index(state, scope)?;
            return Ok(());
        }
        *active = false;
        if let Some(source) = state
            .sources
            .iter_mut()
            .find(|source| source.source == token.source)
        {
            source.maintenance_pending = true;
        }
    }
    Ok(())
}

type RoundAction = (RoundAdmission, Option<Arc<PreparedInputs>>);

fn begin_round_call(
    state: &mut State,
    authority: &'static Authority,
    id: &InitIdV1,
    operation: Operation,
    fallback: &Arc<Error>,
    unstarted: &Arc<Error>,
) -> std::result::Result<RoundAction, ModuleFailure> {
    let index = state
        .rounds
        .iter()
        .position(|round| same_round(&round.id, id))
        .ok_or((ErrorKind::NotFound, None))?;
    let round = &mut state.rounds[index];
    match (&round.phase, operation) {
        (RoundPhase::Preparing | RoundPhase::Initializing | RoundPhase::Closing, _) => {
            return Err((ErrorKind::Busy, None));
        }
        (RoundPhase::Prepared, Operation::Initialize) => {}
        (_, Operation::Initialize) => {
            return Ok((
                RoundAdmission::Replay {
                    error: round.init_failure.clone(),
                },
                None,
            ));
        }
        (RoundPhase::Prepared | RoundPhase::Rejected, Operation::Close) => {
            if matches!(round.phase, RoundPhase::Prepared) {
                round.init_failure = Some(Arc::clone(unstarted));
                round.ffi_progress = Some(FfiProgress::InitializationNotStarted);
            }
            round.phase = RoundPhase::ReturningHoldings;
            return Ok((RoundAdmission::ReturnHoldings, round.inputs.take()));
        }
        (RoundPhase::ReturningHoldings, Operation::Close) => {
            return Ok((RoundAdmission::ReturnHoldings, None));
        }
        (RoundPhase::Closed | RoundPhase::CleanupFailed, Operation::Close) => {
            return Ok((
                RoundAdmission::Replay {
                    error: round.close_failure.clone(),
                },
                None,
            ));
        }
        (RoundPhase::Initialized | RoundPhase::NeedsClose, Operation::Close) => {}
    }

    let (image, _) = ready_for_prepare(state, id.module_epoch)?;
    let image = Arc::clone(image);
    let round = &state.rounds[index];
    let connection = round
        .connection
        .clone()
        .ok_or((ErrorKind::Corrupted, None))?;
    let inputs = match operation {
        Operation::Initialize => Some(round.inputs.clone().ok_or((ErrorKind::Corrupted, None))?),
        Operation::Close => None,
    };
    state
        .calls
        .try_reserve(1)
        .map_err(|_| (ErrorKind::ResourceExhausted, None))?;
    let token = issue_identifier(state).map_err(|kind| (kind, None))?;
    let round = &mut state.rounds[index];
    round.ffi_progress = None;
    match operation {
        Operation::Initialize => {
            round.phase = RoundPhase::Initializing;
            round.init_failure = Some(Arc::clone(fallback));
        }
        Operation::Close => {
            round.phase = RoundPhase::Closing;
            round.close_failure = Some(Arc::clone(fallback));
        }
    }
    state.calls.push(CallRecord {
        token: CallIdentifier::Standard(token),
        actor: 0,
        owner: CodeOwner::Plugin {
            epoch: id.module_epoch,
        },
        id: round_id(id),
        target: None,
    });
    Ok((
        RoundAdmission::Call(RoundCall {
            authority,
            token,
            id: round_id(id),
            finished: false,
            entries: image.entries,
            image,
            connection,
            inputs,
        }),
        None,
    ))
}

fn active_standard_call(state: &State, call: &RoundCall) -> Option<usize> {
    state.calls.iter().position(|record| {
        matches!(record.token, CallIdentifier::Standard(token) if token == call.token)
            && record.owner
                == (CodeOwner::Plugin {
                    epoch: call.id.module_epoch,
                })
            && record.actor == 0
            && same_round(&record.id, &call.id)
    })
}

type StandardRelease = (Option<Arc<Error>>, Option<Arc<PreparedInputs>>);

fn finish_standard_call(
    state: &mut State,
    call: &mut RoundCall,
    failure: &Option<Arc<Error>>,
) -> std::result::Result<StandardRelease, ErrorKind> {
    let index = active_standard_call(state, call).ok_or(ErrorKind::InvalidState)?;
    let round = state
        .rounds
        .iter_mut()
        .find(|round| same_round(&round.id, &call.id))
        .ok_or(ErrorKind::NotFound)?;
    let released = match round.phase {
        RoundPhase::Initializing => {
            let next = match round.ffi_progress {
                Some(FfiProgress::InitializationCompleted) if failure.is_none() => {
                    RoundPhase::Initialized
                }
                None | Some(FfiProgress::InitializationNotStarted) => RoundPhase::Rejected,
                _ => RoundPhase::NeedsClose,
            };
            round.phase = next;
            (
                std::mem::replace(&mut round.init_failure, failure.clone()),
                None,
            )
        }
        RoundPhase::Closing => match round.ffi_progress {
            Some(FfiProgress::CloseCompleted) => {
                round.phase = RoundPhase::ReturningHoldings;
                (
                    std::mem::replace(&mut round.close_failure, failure.clone()),
                    round.inputs.take(),
                )
            }
            Some(FfiProgress::ClosePending)
                if failure
                    .as_ref()
                    .is_some_and(|failure| *failure.current_context() == ErrorKind::Busy) =>
            {
                round.phase = RoundPhase::NeedsClose;
                (round.close_failure.take(), None)
            }
            _ => {
                round.phase = RoundPhase::CleanupFailed;
                let old = failure
                    .as_ref()
                    .and_then(|error| round.close_failure.replace(Arc::clone(error)));
                (old, None)
            }
        },
        _ => return Err(ErrorKind::InvalidState),
    };
    state.calls.swap_remove(index);
    call.finished = true;
    Ok(released)
}

struct SourceWork {
    source: u64,
    id: InitIdV1,
    descriptor: SourceDescriptorV1,
    operation: OperationIdV1,
}

enum RetirementAdmission {
    Execute(SourceWork),
    Replay(OutcomeV1),
}

fn admit_retirement(
    state: &mut State,
    source: u64,
    request: Option<(&CallbackScope, OperationIdV1)>,
) -> std::result::Result<RetirementAdmission, ErrorKind> {
    let index = state
        .sources
        .iter()
        .position(|record| record.source == source);
    if let Some((scope, operation)) = &request {
        let round = &state.rounds[live_round_index(state, scope)?];
        verify_operation(round, scope, operation)?;
        let previous = match index {
            Some(index) => {
                let record = &state.sources[index];
                if record.owner != scope.owner || !same_round(&record.id, &scope.id) {
                    return Err(ErrorKind::Conflict);
                }
                record
                    .retirement
                    .as_ref()
                    .and_then(|retirement| retirement.request.as_ref())
            }
            None => None,
        };
        let changed = previous.map_or_else(
            || operation_claimed(state, &scope.id, operation),
            |previous| !same_operation(previous, operation),
        );
        if changed {
            return Err(ErrorKind::Conflict);
        }
    }
    let index = index.ok_or(ErrorKind::NotFound)?;

    if let Some((_, operation)) = &request {
        if let Some(retirement) = &mut state.sources[index].retirement {
            if retirement.request.is_none() {
                retirement.request = Some(copy_operation(operation));
            }
        }
    }
    let record = &state.sources[index];
    match record.state {
        SourceState::Retired => {
            return Ok(RetirementAdmission::Replay(diagnostic::success_outcome()));
        }
        SourceState::RetirementFailed {
            kind,
            diagnostic_failure_kind,
        } => {
            return Ok(RetirementAdmission::Replay(finite_result(
                kind,
                diagnostic_failure_kind,
            )));
        }
        SourceState::Quarantined => {
            return Ok(RetirementAdmission::Replay(retirement_quarantine_reply(
                record,
            )));
        }
        _ => {}
    }
    if !state.modules.iter().any(|module| {
        module.epoch == record.id.module_epoch && matches!(module.phase, ModulePhase::Loaded)
    }) {
        return Err(ErrorKind::InvalidState);
    }

    if state.sources[index].retirement.is_none() {
        let control = OperationIdV1 {
            actor: 0,
            sequence: issue_identifier(state)?,
        };
        state.sources[index].retirement = Some(SourceRetirement {
            request: None,
            control,
        });
    }
    let record = &mut state.sources[index];
    let Some(retirement) = &mut record.retirement else {
        return Err(ErrorKind::Corrupted);
    };
    if retirement.request.is_none() {
        retirement.request = request.map(|(_, operation)| operation);
    }
    if matches!(record.state, SourceState::Registered) {
        record.state = SourceState::Retiring;
    }
    if !matches!(record.state, SourceState::Retiring)
        || record.maintenance_operation.is_some()
        || active_source_calls(state, source)
        || outstanding_source_holdings(state, source)
    {
        return Err(ErrorKind::Busy);
    }

    let record = &mut state.sources[index];
    let Some(retirement) = &record.retirement else {
        return Err(ErrorKind::Corrupted);
    };
    let task = SourceWork {
        source,
        id: round_id(&record.id),
        descriptor: record.descriptor,
        operation: copy_operation(&retirement.control),
    };
    record.state = SourceState::CheckingRetirement {
        thread: std::thread::current().id(),
        diagnostic: None,
    };
    Ok(RetirementAdmission::Execute(task))
}

fn copy_operation(operation: &OperationIdV1) -> OperationIdV1 {
    OperationIdV1 {
        actor: operation.actor,
        sequence: operation.sequence,
    }
}

fn active_source_calls(state: &State, source: u64) -> bool {
    state.calls.iter().any(|record| {
        matches!(record.token, CallIdentifier::Source { token, active: true, .. } if token.source == source)
    })
}

fn outstanding_source_holdings(state: &State, source: u64) -> bool {
    state.holdings.iter().any(|holding| {
        holding.target.lease.object.source == source
            && !matches!(holding.phase, HoldingPhase::Returned)
    })
}

fn empty_diagnostic(reference: &pi_ffi_abi::DiagnosticRefV1) -> bool {
    let lease = &reference.lease;
    reference.context.is_null()
        && reference.api.is_null()
        && lease.object.source == 0
        && lease.object.slot == 0
        && lease.object.generation == 0
        && lease.initialization.module_epoch == 0
        && lease.initialization.round == 0
        && lease.holding == 0
}

fn finite_result(kind: u32, diagnostic_failure_kind: u32) -> OutcomeV1 {
    let mut outcome = diagnostic::success_outcome();
    outcome.kind = kind;
    outcome.diagnostic_failure_kind = diagnostic_failure_kind;
    outcome
}

fn retirement_quarantine_reply(source: &RegisteredSource) -> OutcomeV1 {
    if let Some(raw) = source
        .failure
        .as_ref()
        .and_then(|failure| failure.downcast_ref::<SourceStatus>())
    {
        return finite_result(raw.0, diagnostic::status_code(ErrorKind::InvalidState));
    }
    let kind = source
        .failure
        .as_ref()
        .map_or(ErrorKind::InvalidState, |failure| {
            *failure.current_context()
        });
    diagnostic::limited_outcome(kind, ErrorKind::InvalidState)
}

#[derive(Debug)]
struct SourceStatus(u32);

impl std::fmt::Display for SourceStatus {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "来源状态（source status） {}", self.0)
    }
}

fn source_status_error(status: u32, operation: &'static str) -> Error {
    source_result_error(status, diagnostic::status_code(ErrorKind::InvalidState), operation)
}

fn source_result_error(status: u32, diagnostic_failure: u32, operation: &'static str) -> Error {
    // 安全前提（safety）。从原主次分类构造的有限结果（limited outcome）没有来源指针。
    let decoded = unsafe {
        diagnostic::decode_management(None, finite_result(status, diagnostic_failure))
    };
    decoded
        .err()
        .unwrap_or_else(|| module_error(ErrorKind::Corrupted, operation, None))
        .attach(ErrorOperation(operation))
        .attach(SourceStatus(status))
}

fn finish_retirement_admission(
    authority: &Authority,
    admission: std::result::Result<RetirementAdmission, ErrorKind>,
    failed: Arc<Error>,
) -> OutcomeV1 {
    match admission {
        Ok(RetirementAdmission::Replay(outcome)) => outcome,
        Ok(RetirementAdmission::Execute(task)) => {
            let mut attempt = SourceAttempt {
                authority,
                source: task.source,
                maintenance: None,
                fallback: failed,
                finished: false,
            };
            let outcome = match run_retirement(authority, &task) {
                RetirementReply::Verified(outcome) => outcome,
                RetirementReply::Unconfirmed { outcome, failure } => {
                    record_source_quarantine(authority, task.source, None, &failure);
                    outcome
                }
            };
            attempt.finished = true;
            outcome
        }
        Err(kind) => diagnostic::limited_outcome(kind, ErrorKind::InvalidState),
    }
}

struct SourceAttempt<'a> {
    authority: &'a Authority,
    source: u64,
    maintenance: Option<&'a OperationIdV1>,
    fallback: Arc<Error>,
    finished: bool,
}

impl Drop for SourceAttempt<'_> {
    fn drop(&mut self) {
        if !self.finished {
            record_source_quarantine(
                self.authority,
                self.source,
                self.maintenance,
                &self.fallback,
            );
        }
    }
}

fn record_source_quarantine(
    authority: &Authority,
    source: u64,
    maintenance: Option<&OperationIdV1>,
    failure: &Arc<Error>,
) {
    let mut state = authority.state.lock();
    if let Some(record) = state.sources.iter_mut().find(|record| {
        record.source == source
            && match (record.maintenance_operation.as_ref(), maintenance) {
                (Some(current), Some(original)) => same_operation(current, original),
                (None, None) => true,
                _ => false,
            }
    }) {
        record.state = SourceState::Quarantined;
        if record.failure.is_none() {
            record.failure = Some(Arc::clone(failure));
        }
        record.maintenance_operation = None;
    }
}

unsafe fn checked_source_control(descriptor: &SourceDescriptorV1) -> Option<&SourceControlApiV1> {
    // 安全前提（safety）。仅在宿主已登记、受本次独占工作保护的实际来源读取其表。
    let table = unsafe { descriptor.control.as_ref() }?;
    if table.header.major == SOURCE_CONTROL_MAJOR
        && table.header.size as usize == size_of::<SourceControlApiV1>()
        && table.try_retire.is_some()
        && table.release_context.is_some()
        && table.maintain.is_some()
    {
        Some(table)
    } else {
        None
    }
}

enum RetirementReply {
    Verified(OutcomeV1),
    Unconfirmed {
        outcome: OutcomeV1,
        failure: Arc<Error>,
    },
}

fn run_retirement(authority: &Authority, task: &SourceWork) -> RetirementReply {
    // 安全前提（safety）。本来源检查已登记活动线程；上下文和表在检查与归还后才结束持有。
    let operations = unsafe { checked_source_control(&task.descriptor) }
        .and_then(|table| table.try_retire.zip(table.release_context));
    let Some((check, release)) = operations else {
        let kind = ErrorKind::Corrupted;
        return RetirementReply::Unconfirmed {
            outcome: diagnostic::limited_outcome(kind, ErrorKind::InvalidState),
            failure: Arc::new(module_error(kind, "validate source retirement table", None)),
        };
    };
    let outcome = unsafe {
        check(
            task.descriptor.context,
            task.source,
            copy_operation(&task.operation),
        )
    };
    let verified = {
        let mut state = authority.state.lock();
        record_retirement_check(&mut state, task, &outcome)
    };
    match verified {
        Ok(false) => RetirementReply::Verified(outcome),
        Ok(true) => {
            // 安全前提（safety）。独占检查和已登记持有均已确认结束，当前独占消费上下文。
            let status = unsafe { release(task.descriptor.context, task.source) };
            if status != 0 {
                return RetirementReply::Unconfirmed {
                    outcome: finite_result(status, diagnostic::status_code(ErrorKind::InvalidState)),
                    failure: Arc::new(source_status_error(status, "release source context")),
                };
            }
            {
                let mut state = authority.state.lock();
                if let Some(source) = state
                    .sources
                    .iter_mut()
                    .find(|record| record.source == task.source)
                {
                    source.state = SourceState::Retired;
                    source.maintenance_pending = false;
                }
            }
            RetirementReply::Verified(diagnostic::success_outcome())
        }
        Err(kind) => RetirementReply::Unconfirmed {
            outcome: diagnostic::limited_outcome(kind, ErrorKind::InvalidState),
            failure: Arc::new(module_error(kind, "verify source retirement result", None)),
        },
    }
}

fn record_retirement_check(
    state: &mut State,
    work: &SourceWork,
    outcome: &OutcomeV1,
) -> std::result::Result<bool, ErrorKind> {
    let index = state
        .sources
        .iter()
        .position(|record| record.source == work.source)
        .ok_or(ErrorKind::NotFound)?;
    let source = &state.sources[index];
    let SourceState::CheckingRetirement {
        thread,
        diagnostic: registered,
    } = &source.state
    else {
        return Err(ErrorKind::InvalidState);
    };
    if *thread != std::thread::current().id()
        || !source
            .retirement
            .as_ref()
            .is_some_and(|record| same_operation(&record.control, &work.operation))
    {
        return Err(ErrorKind::Conflict);
    }
    let transferred = outcome.kind != 0 && outcome.diagnostic_failure_kind == 0;
    if transferred {
        if !registered.as_ref().is_some_and(|known| {
            known.kind == TARGET_DIAGNOSTIC && same_lease(&known.lease, &outcome.diagnostic.lease)
        }) || !registered_source_diagnostic(state, work, outcome)
        {
            return Err(ErrorKind::Corrupted);
        }
    } else if !empty_diagnostic(&outcome.diagnostic)
        || registered.is_some()
        || (outcome.kind == 0 && outcome.diagnostic_failure_kind != 0)
    {
        return Err(ErrorKind::Corrupted);
    }

    if outcome.kind == 0 {
        if active_source_calls(state, work.source)
            || outstanding_source_holdings(state, work.source)
        {
            return Err(ErrorKind::Corrupted);
        }
        state.sources[index].state = SourceState::ReleasingContext;
        return Ok(true);
    }

    let source = &mut state.sources[index];
    if outcome.kind == diagnostic::status_code(ErrorKind::Busy) {
        source.state = SourceState::Retiring;
        source.maintenance_pending = true;
    } else {
        let limited = if transferred {
            diagnostic::status_code(ErrorKind::InvalidState)
        } else {
            outcome.diagnostic_failure_kind
        };
        source.state = SourceState::RetirementFailed {
            kind: outcome.kind,
            diagnostic_failure_kind: limited,
        };
    }
    Ok(false)
}

fn registered_source_diagnostic(state: &State, work: &SourceWork, outcome: &OutcomeV1) -> bool {
    let reference = &outcome.diagnostic;
    let target = CallTargetV1 {
        context: reference.context,
        api: reference.api.cast(),
        lease: holding_lease(&reference.lease),
        kind: TARGET_DIAGNOSTIC,
    };
    target.lease.object.source == work.source
        && target_source_index(state, &work.id, &target).is_ok()
        && state.holdings.iter().any(|holding| {
            holding.target.kind == TARGET_DIAGNOSTIC
                && same_lease(&holding.target.lease, &target.lease)
                && matches!(holding.phase, HoldingPhase::Tracked)
        })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceStep {
    Maintain(u64),
    Retire(u64),
}

enum MaintenanceWork {
    DiscardInputs(Option<Arc<PreparedInputs>>),
    DiscardRound {
        connection: Option<Connection>,
        issued_operations: Vec<OperationIdV1>,
    },
    ReleaseModule(SystemOperation),
    ReleaseHolding {
        source: SourceWork,
        holding: HoldingKey,
        connection: Connection,
    },
    MaintainSource {
        source: SourceWork,
        connection: Connection,
    },
    RetireSource {
        source: SourceWork,
        connection: Connection,
        maintenance: OperationIdV1,
    },
    FinishFailedRound {
        id: InitIdV1,
        result: RoundCleanupFailure,
    },
}

impl MaintenanceWork {
    fn source_step(&self) -> Option<SourceStep> {
        match self {
            Self::MaintainSource { source, .. } => Some(SourceStep::Maintain(source.source)),
            Self::RetireSource { source, .. } => Some(SourceStep::Retire(source.source)),
            _ => None,
        }
    }
}

fn next_maintenance(
    authority: &'static Authority,
    state: &mut State,
    round: Option<&InitIdV1>,
    visited: &[SourceStep],
) -> std::result::Result<Option<MaintenanceWork>, ModuleFailure> {
    let selected = |id: &InitIdV1| round.is_none_or(|requested| same_round(requested, id));
    if round.is_none() {
        if let Some(record) = state
            .modules
            .iter_mut()
            .find(|module| matches!(module.phase, ModulePhase::AwaitingRelease))
        {
            let files = record.files.clone().ok_or((ErrorKind::Corrupted, None))?;
            let library = record.library.take().ok_or((ErrorKind::Corrupted, None))?;
            record.phase = ModulePhase::Unloading;
            return Ok(Some(MaintenanceWork::ReleaseModule(SystemOperation {
                authority,
                epoch: record.epoch,
                files: Some(files),
                purpose: SystemPurpose::Retire,
                phase: SystemPhase::OwnsReference,
                library: Some(library),
            })));
        }
    }
    if let Some(record) = state
        .rounds
        .iter_mut()
        .find(|record| selected(&record.id) && matches!(record.phase, RoundPhase::Rejected))
    {
        record.phase = RoundPhase::ReturningHoldings;
        return Ok(Some(MaintenanceWork::DiscardInputs(record.inputs.take())));
    }
    if let Some(index) = state.holdings.iter().position(|holding| {
        let id = &holding.target.lease.initialization;
        selected(id)
            && matches!(holding.phase, HoldingPhase::Requested)
            && source_for_maintenance(state, holding.target.lease.object.source).is_some()
            && !state.calls.iter().any(|call| {
                matches!(call.token, CallIdentifier::Source { active: true, .. })
                    && call.target.as_ref().is_some_and(|target| {
                        target.kind == holding.target.kind
                            && same_lease(&target.lease, &holding.target.lease)
                    })
            })
    }) {
        let holding = &state.holdings[index];
        let source = source_for_maintenance(state, holding.target.lease.object.source)
            .ok_or((ErrorKind::Corrupted, None))?;
        let source = source_work(source, copy_operation(&holding.release_operation));
        let connection = source_connection(state, &source.id)?;
        let holding_key = HoldingKey {
            lease: holding_lease(&holding.target.lease),
            kind: holding.target.kind,
        };
        state.holdings[index].phase = HoldingPhase::Releasing;
        return Ok(Some(MaintenanceWork::ReleaseHolding {
            source,
            holding: holding_key,
            connection,
        }));
    }
    if let Some(index) = state.sources.iter().position(|source| {
        selected(&source.id)
            && source.maintenance_pending
            && source.failure.is_none()
            && matches!(
                source.state,
                SourceState::Registered | SourceState::Retiring
            )
            && !visited.contains(&SourceStep::Maintain(source.source))
            && source_for_maintenance(state, source.source).is_some()
            && !active_source_calls(state, source.source)
            && !state.holdings.iter().any(|holding| {
                holding.target.lease.object.source == source.source
                    && matches!(holding.phase, HoldingPhase::Releasing)
            })
    }) {
        let connection = source_connection(state, &state.sources[index].id)?;
        let operation = OperationIdV1 {
            actor: 0,
            sequence: issue_identifier(state).map_err(|kind| (kind, None))?,
        };
        let source = &mut state.sources[index];
        let work = source_work(source, copy_operation(&operation));
        source.maintenance_pending = false;
        source.maintenance_operation = Some(operation);
        return Ok(Some(MaintenanceWork::MaintainSource {
            source: work,
            connection,
        }));
    }
    if let Some(index) = state.sources.iter().position(|source| {
        if !selected(&source.id)
            || visited.contains(&SourceStep::Retire(source.source))
            || !matches!(
                source.state,
                SourceState::Registered | SourceState::Retiring
            )
            || source_for_maintenance(state, source.source).is_none()
            || active_source_calls(state, source.source)
            || outstanding_source_holdings(state, source.source)
        {
            return false;
        }
        if source.retirement.is_none()
            && !state.rounds.iter().any(|round| {
                same_round(&round.id, &source.id)
                    && matches!(round.phase, RoundPhase::ReturningHoldings)
            })
        {
            return false;
        }
        if source.owner == CodeOwner::Host
            && state.sources.iter().any(|other| {
                same_round(&other.id, &source.id)
                    && matches!(other.owner, CodeOwner::Plugin { .. })
                    && !matches!(other.state, SourceState::Retired)
            })
        {
            return false;
        }
        true
    }) {
        let source = state.sources[index].source;
        let connection = source_connection(state, &state.sources[index].id)?;
        let maintenance = OperationIdV1 {
            actor: 0,
            sequence: issue_identifier(state).map_err(|kind| (kind, None))?,
        };
        let result = admit_retirement(state, source, None).map_err(|kind| (kind, None))?;
        if let RetirementAdmission::Execute(source) = result {
            state.sources[index].maintenance_operation = Some(copy_operation(&maintenance));
            return Ok(Some(MaintenanceWork::RetireSource {
                source,
                connection,
                maintenance,
            }));
        }
    }
    for index in 0..state.rounds.len() {
        let record = &state.rounds[index];
        if !selected(&record.id) || !matches!(record.phase, RoundPhase::ReturningHoldings) {
            continue;
        }
        if let Some(task) = failed_round_work(state, &record.id) {
            return Ok(Some(task));
        }
        let pending = state.sources.iter().any(|source| {
            same_round(&source.id, &record.id)
                && (!matches!(source.state, SourceState::Retired)
                    || source.maintenance_operation.is_some())
        }) || state.holdings.iter().any(|holding| {
            same_round(&holding.target.lease.initialization, &record.id)
                && !matches!(holding.phase, HoldingPhase::Returned)
        }) || state.calls.iter().any(|call| {
            same_round(&call.id, &record.id)
                && !matches!(call.token, CallIdentifier::Source { active: false, .. })
        });
        if !pending {
            let record = &mut state.rounds[index];
            record.phase = RoundPhase::Closed;
            state.calls.retain(|call| !same_round(&call.id, &record.id));
            return Ok(Some(MaintenanceWork::DiscardRound {
                connection: record.connection.take(),
                issued_operations: std::mem::take(&mut record.issued_operations),
            }));
        }
    }
    Ok(None)
}

fn source_for_maintenance(state: &State, source: u64) -> Option<&RegisteredSource> {
    state.sources.iter().find(|record| {
        record.source == source
            && record.maintenance_operation.is_none()
            && matches!(
                record.state,
                SourceState::Registered
                    | SourceState::Retiring
                    | SourceState::RetirementFailed { .. }
            )
            && state.modules.iter().any(|module| {
                module.epoch == record.id.module_epoch
                    && matches!(module.phase, ModulePhase::Loaded)
            })
    })
}

fn source_work(record: &RegisteredSource, operation: OperationIdV1) -> SourceWork {
    SourceWork {
        source: record.source,
        id: round_id(&record.id),
        descriptor: record.descriptor,
        operation,
    }
}

fn source_connection(
    state: &State,
    id: &InitIdV1,
) -> std::result::Result<Connection, ModuleFailure> {
    state
        .rounds
        .iter()
        .find(|record| same_round(&record.id, id))
        .and_then(|record| record.connection.clone())
        .ok_or((ErrorKind::Corrupted, None))
}

enum RoundCleanupFailure {
    Report(Arc<Error>),
    Limited(u32, u32),
    Unconfirmed,
}

impl RoundCleanupFailure {
    fn matches(&self, current: &Self) -> bool {
        match (self, current) {
            (Self::Report(original), Self::Report(current)) => Arc::ptr_eq(original, current),
            (Self::Limited(original, diagnostic), Self::Limited(current, current_diagnostic)) => {
                original == current && diagnostic == current_diagnostic
            }
            (Self::Unconfirmed, Self::Unconfirmed) => true,
            _ => false,
        }
    }

    fn report(&self) -> Arc<Error> {
        let operation = "return initialization sources";
        match self {
            Self::Report(failure) => Arc::clone(failure),
            Self::Limited(kind, diagnostic) => {
                Arc::new(source_result_error(*kind, *diagnostic, operation))
            }
            Self::Unconfirmed => Arc::new(module_error(ErrorKind::InvalidState, operation, None)),
        }
    }
}

fn failed_round_work(state: &State, id: &InitIdV1) -> Option<MaintenanceWork> {
    failed_round_result(state, id).map(|result| MaintenanceWork::FinishFailedRound {
        id: round_id(id),
        result,
    })
}

fn failed_round_result(state: &State, id: &InitIdV1) -> Option<RoundCleanupFailure> {
    if !state.rounds.iter().any(|round| {
        same_round(id, &round.id) && matches!(round.phase, RoundPhase::ReturningHoldings)
    }) || pending_retirement_report(state, id).is_some()
    {
        return None;
    }
    if let Some(holding) = state.holdings.iter().find(|holding| {
        same_round(id, &holding.target.lease.initialization)
            && matches!(holding.phase, HoldingPhase::Quarantined)
    }) {
        return Some(holding.failure.as_ref().map_or(
            RoundCleanupFailure::Unconfirmed,
            |failure| RoundCleanupFailure::Report(Arc::clone(failure)),
        ));
    }
    for source in state.sources.iter().filter(|source| {
        same_round(id, &source.id) && source.maintenance_operation.is_none()
    }) {
        if let Some(failure) = &source.failure {
            return Some(RoundCleanupFailure::Report(Arc::clone(failure)));
        }
        match source.state {
            SourceState::RetirementFailed { kind, diagnostic_failure_kind } => {
                return Some(RoundCleanupFailure::Limited(kind, diagnostic_failure_kind));
            }
            SourceState::Quarantined => return Some(RoundCleanupFailure::Unconfirmed),
            _ => {}
        }
    }
    None
}

fn run_maintenance(
    authority: &Authority,
    task: MaintenanceWork,
    interruption: &Arc<Error>,
) -> Result<()> {
    match task {
        MaintenanceWork::DiscardInputs(inputs) => drop(inputs),
        MaintenanceWork::DiscardRound {
            connection,
            issued_operations,
        } => drop((connection, issued_operations)),
        MaintenanceWork::ReleaseModule(operation) => operation.release()?,
        MaintenanceWork::ReleaseHolding {
            source,
            holding,
            connection,
        } => {
            run_holding_release(authority, &source, &holding, &connection, interruption)?;
        }
        MaintenanceWork::MaintainSource { source, connection } => {
            run_source_maintenance(authority, &source, &connection, interruption)?;
        }
        MaintenanceWork::RetireSource {
            source,
            connection,
            maintenance,
        } => {
            advance_automatic_retirement(
                authority,
                &source,
                &connection,
                &maintenance,
                interruption,
            )?;
        }
        MaintenanceWork::FinishFailedRound { id, result } => {
            let failure = result.report();
            let saved = {
                let mut state = authority.state.lock();
                failed_round_result(&state, &id)
                    .is_some_and(|current| result.matches(&current))
                    && save_round_cleanup_failure(&mut state, &id, &failure, None)
            };
            if saved {
                return Err(replay_module_failure(
                    failure,
                    "return initialization sources",
                ));
            }
        }
    }
    Ok(())
}

fn advance_automatic_retirement(
    authority: &Authority,
    source: &SourceWork,
    connection: &Connection,
    maintenance: &OperationIdV1,
    interruption: &Arc<Error>,
) -> Result<()> {
    let mut handoff = SourceAttempt {
        authority,
        source: source.source,
        maintenance: Some(maintenance),
        fallback: Arc::clone(interruption),
        finished: false,
    };
    let (failure, unconfirmed) = match run_retirement(authority, source) {
        RetirementReply::Verified(outcome) => (
            decode_source_reply(authority, source, connection, outcome)
                .err()
                .filter(|error| *error.current_context() != ErrorKind::Busy)
                .map(Arc::new),
            false,
        ),
        RetirementReply::Unconfirmed { failure, .. } => (Some(failure), true),
    };
    let settled = {
        let mut state = authority.state.lock();
        if let Some(index) = state.sources.iter().position(|record| {
            record.source == source.source
                && same_round(&record.id, &source.id)
                && record
                    .maintenance_operation
                    .as_ref()
                    .is_some_and(|operation| same_operation(operation, maintenance))
        }) {
            let record = &mut state.sources[index];
            if unconfirmed {
                record.state = SourceState::Quarantined;
            }
            let deliver_failure = if let Some(failure) = &failure {
                if record.failure.is_none() {
                    record.failure = Some(Arc::clone(failure));
                }
                save_round_cleanup_failure(&mut state, &source.id, failure, Some(maintenance))
            } else {
                true
            };
            state.sources[index].maintenance_operation = None;
            Some(deliver_failure)
        } else {
            None
        }
    };
    let Some(deliver_failure) = settled else {
        return Err(module_error(
            ErrorKind::Conflict,
            "complete automatic source retirement",
            None,
        ));
    };
    handoff.finished = true;
    if let Some(failure) = failure.filter(|_| deliver_failure) {
        return Err(replay_module_failure(
            failure,
            "retire source during maintenance",
        ));
    }
    Ok(())
}

fn pending_retirement_report<'a>(state: &'a State, id: &InitIdV1) -> Option<&'a OperationIdV1> {
    state
        .sources
        .iter()
        .filter(|source| {
            same_round(id, &source.id)
                && matches!(source.state, SourceState::RetirementFailed { .. })
        })
        .filter_map(|source| {
            let operation = source.maintenance_operation.as_ref()?;
            let owns_diagnostic = state.holdings.iter().any(|holding| {
                holding.target.lease.object.source == source.source
                    && holding.target.kind == TARGET_DIAGNOSTIC
                    && matches!(holding.phase, HoldingPhase::Tracked | HoldingPhase::Requested)
            });
            owns_diagnostic.then_some(operation)
        })
        .min_by_key(|operation| operation.sequence)
}

fn fail_returning_round(authority: &Authority, id: &InitIdV1, failure: &Arc<Error>) -> bool {
    let mut state = authority.state.lock();
    save_round_cleanup_failure(&mut state, id, failure, None)
}

fn save_round_cleanup_failure(
    state: &mut State,
    id: &InitIdV1,
    failure: &Arc<Error>,
    retirement: Option<&OperationIdV1>,
) -> bool {
    let Some(index) = state.rounds.iter().position(|round| same_round(&round.id, id)) else {
        return true;
    };
    match state.rounds[index].phase {
        RoundPhase::ReturningHoldings => {
            // 等待已登记的完整退役诊断（retirement diagnostic）。同轮并行交接时由较早的维护保存整轮结果。
            if pending_retirement_report(state, id).is_some_and(|pending| {
                retirement.is_none_or(|current| !same_operation(pending, current))
            }) {
                return false;
            }
            let round = &mut state.rounds[index];
            if round.close_failure.is_none() {
                round.close_failure = Some(Arc::clone(failure));
            }
            round.phase = RoundPhase::CleanupFailed;
            true
        }
        RoundPhase::CleanupFailed => state.rounds[index]
            .close_failure
            .as_deref()
            .and_then(diagnostic::copied_diagnostic)
            .is_none(),
        _ => true,
    }
}

fn save_source_failure(authority: &Authority, source: u64, failure: &Arc<Error>) {
    let mut state = authority.state.lock();
    if let Some(record) = state
        .sources
        .iter_mut()
        .find(|record| record.source == source)
    {
        if record.failure.is_none() {
            record.failure = Some(Arc::clone(failure));
        }
    }
}

fn decode_source_reply(
    authority: &Authority,
    source: &SourceWork,
    connection: &Connection,
    outcome: OutcomeV1,
) -> Result<()> {
    if outcome.kind != 0 && outcome.diagnostic_failure_kind == 0 {
        let verified = {
            let state = authority.state.lock();
            registered_source_diagnostic(&state, source, &outcome)
        };
        if !verified {
            return Err(module_error(
                ErrorKind::Corrupted,
                "validate source maintenance diagnostic",
                None,
            ));
        }
    }
    // 安全前提（safety）。维持本次源操作时实际保护了已核定的诊断及独立持有。
    unsafe { diagnostic::decode_management(Some(connection), outcome) }
}

struct HoldingRelease<'a> {
    authority: &'a Authority,
    holding: &'a HoldingKey,
    fallback: &'a Arc<Error>,
    finished: bool,
}

impl Drop for HoldingRelease<'_> {
    fn drop(&mut self) {
        if !self.finished {
            settle_holding_release(self.authority, self.holding, Some(self.fallback));
        }
    }
}

fn run_holding_release(
    authority: &Authority,
    source: &SourceWork,
    holding: &HoldingKey,
    connection: &Connection,
    interruption: &Arc<Error>,
) -> Result<()> {
    let mut release = HoldingRelease {
        authority,
        holding,
        fallback: interruption,
        finished: false,
    };
    let result = match holding.kind {
        TARGET_OBJECT => {
            // 安全前提（safety）。本条持有仍在归还阶段，宿主记录保活实际来源和其已登记表。
            let table = unsafe { source.descriptor.objects.as_ref() }.filter(|table| {
                table.header.major == 1
                    && table.header.size as usize == size_of::<pi_ffi_abi::SourceApiV1>()
            });
            match table.and_then(|table| table.release) {
                Some(callback) => {
                    let outcome = unsafe {
                        callback(
                            source.descriptor.context,
                            holding_lease(&holding.lease),
                            copy_operation(&source.operation),
                        )
                    };
                    decode_source_reply(authority, source, connection, outcome)
                }
                None => Err(module_error(
                    ErrorKind::Corrupted,
                    "validate object release table",
                    None,
                )),
            }
        }
        TARGET_DIAGNOSTIC => {
            // 安全前提（safety）。本条登记的诊断归还保持同一次真实来源保护。
            let table = unsafe { source.descriptor.diagnostics.as_ref() }.filter(|table| {
                table.header.major == 1
                    && table.header.size as usize == size_of::<pi_ffi_abi::DiagnosticApiV1>()
            });
            match table.and_then(|table| table.release) {
                Some(callback) => match unsafe {
                    callback(source.descriptor.context, holding_lease(&holding.lease))
                } {
                    0 => Ok(()),
                    status => Err(source_status_error(status, "release source diagnostic")),
                },
                None => Err(module_error(
                    ErrorKind::Corrupted,
                    "validate diagnostic release table",
                    None,
                )),
            }
        }
        _ => Err(module_error(
            ErrorKind::Corrupted,
            "validate source holding kind",
            None,
        )),
    };
    let failure = result.err().map(Arc::new);
    settle_holding_release(authority, holding, failure.as_ref());
    release.finished = true;
    match failure {
        Some(failure) if fail_returning_round(authority, &source.id, &failure) => {
            Err(replay_module_failure(
                failure,
                "release registered source holding",
            ))
        }
        _ => Ok(()),
    }
}

fn settle_holding_release(
    authority: &Authority,
    holding: &HoldingKey,
    failure: Option<&Arc<Error>>,
) {
    let mut state = authority.state.lock();
    if let Some(record) = state.holdings.iter_mut().find(|record| {
        record.target.kind == holding.kind && same_lease(&record.target.lease, &holding.lease)
    }) {
        if matches!(record.phase, HoldingPhase::Releasing) {
            if let Some(failure) = failure {
                record.phase = HoldingPhase::Quarantined;
                if record.failure.is_none() {
                    record.failure = Some(Arc::clone(failure));
                }
            } else {
                record.phase = HoldingPhase::Returned;
            }
        }
    }
    if let Some(source) = state
        .sources
        .iter_mut()
        .find(|source| source.source == holding.lease.object.source)
    {
        source.maintenance_pending = true;
    }
}

fn run_source_maintenance(
    authority: &Authority,
    source: &SourceWork,
    connection: &Connection,
    interruption: &Arc<Error>,
) -> Result<()> {
    let mut attempt = SourceAttempt {
        authority,
        source: source.source,
        maintenance: Some(&source.operation),
        fallback: Arc::clone(interruption),
        finished: false,
    };
    // 安全前提（safety）。专用的在途记录禁止并发退役及释放本次挂接的来源。
    let maintain =
        unsafe { checked_source_control(&source.descriptor) }.and_then(|table| table.maintain);
    let Some(maintain) = maintain else {
        let failure = Arc::new(module_error(
            ErrorKind::Corrupted,
            "validate source maintenance table",
            None,
        ));
        record_source_quarantine(authority, source.source, Some(&source.operation), &failure);
        attempt.finished = true;
        return if fail_returning_round(authority, &source.id, &failure) {
            Err(replay_module_failure(failure, "advance source maintenance"))
        } else {
            Ok(())
        };
    };
    let mut snapshot = std::mem::MaybeUninit::<MaintenanceV1>::uninit();
    // 安全前提（safety）。真实来源、固定调用编号和独占可写输出槽均在有效期内。
    let outcome = unsafe {
        maintain(
            source.descriptor.context,
            source.source,
            copy_operation(&source.operation),
            snapshot.as_mut_ptr(),
        )
    };
    match decode_source_reply(authority, source, connection, outcome) {
        Ok(()) => {
            // 安全前提（safety）。来源维护成功完整写回独占槽位。
            let remaining = unsafe { snapshot.assume_init() }.remaining;
            finish_source_maintenance(authority, source, remaining != 0);
            attempt.finished = true;
            Ok(())
        }
        Err(error) if *error.current_context() == ErrorKind::Busy => {
            finish_source_maintenance(authority, source, true);
            attempt.finished = true;
            Ok(())
        }
        Err(error) => {
            let failure = Arc::new(error);
            save_source_failure(authority, source.source, &failure);
            finish_source_maintenance(authority, source, false);
            attempt.finished = true;
            if fail_returning_round(authority, &source.id, &failure) {
                Err(replay_module_failure(
                    failure,
                    "advance registered source maintenance",
                ))
            } else {
                Ok(())
            }
        }
    }
}

fn finish_source_maintenance(authority: &Authority, source: &SourceWork, pending: bool) {
    let mut state = authority.state.lock();
    if let Some(record) = state
        .sources
        .iter_mut()
        .find(|record| record.source == source.source)
    {
        if record
            .maintenance_operation
            .as_ref()
            .is_some_and(|operation| same_operation(operation, &source.operation))
        {
            record.maintenance_operation = None;
            record.maintenance_pending |= pending;
        }
    }
}
