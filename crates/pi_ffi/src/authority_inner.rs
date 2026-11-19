use super::*;
use crate::{loader, package::UnloadPolicy};
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
        if let Some(record) = record_for_file(&state, identity) {
            reuse_snapshot(record).map(LoadSelection::Reuse)
        } else if let Some(retained) = state
            .retained_images
            .iter()
            .find(|retained| retained.deployment.identity() == identity)
        {
            match retained.phase {
                RetentionPhase::Held(_) => {
                    if let Some(other) = quarantined_mapping(&state, retained.mapped, 0) {
                        Err(module_unavailable(other))
                    } else {
                        Ok(LoadSelection::Retained(RetainedSnapshot {
                            retired_epoch: retained.retired_epoch,
                            mapped: retained.mapped,
                            deployment: Arc::clone(&retained.deployment),
                        }))
                    }
                }
                RetentionPhase::Retiring => {
                    let failure = state
                        .modules
                        .iter()
                        .find(|record| {
                            record.epoch == retained.retired_epoch
                                && matches!(record.phase, ModulePhase::Quarantined)
                        })
                        .map_or((ErrorKind::Busy, None), module_unavailable);
                    Err(failure)
                }
                RetentionPhase::Revalidating { .. } => Err((ErrorKind::Busy, None)),
            }
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
        LoadSelection::Retained(original) => {
            if !original.deployment.matches_reuse(&files) {
                return Err(module_error(
                    ErrorKind::Conflict,
                    "match retained plugin deployment",
                    None,
                ));
            }
            let operation = {
                let mut state = authority.state.lock();
                reserve_retained_load(authority, &mut state, &files, &original, &fallback)
            };
            operation
                .map(LoadAdmission::New)
                .map_err(|(kind, failure)| {
                    module_error(kind, "reserve retained plugin reuse", failure)
                })
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
        ready_for_prepare(&state, epoch).and_then(|(image, metadata)| {
            check_quarantined_version(&state, metadata)?;
            Ok(Arc::clone(image))
        })
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
    if matches!(operation.purpose, SystemPurpose::Revalidate { .. })
        && operation.library.is_some()
        && operation.files.is_some()
    {
        // 保留映像（retained image）已有同一系统引用，只开始当前核验。
        operation.phase = SystemPhase::OwnsReference;
        return Ok(());
    }
    let Some(files) = operation.files.as_deref().filter(|_| {
        matches!(operation.purpose, SystemPurpose::Load) && operation.library.is_none()
    }) else {
        return Err(module_error(
            ErrorKind::Corrupted,
            "read reserved module files",
            None,
        ));
    };
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
    if !matches!(
        operation.purpose,
        SystemPurpose::Load | SystemPurpose::Revalidate { .. }
    ) {
        return Err(module_error(
            ErrorKind::InvalidState,
            "record mapped metadata",
            None,
        ));
    }
    let mapped = loader::mapped_identity(operation.library()?)?;
    let metadata = Arc::new(metadata.clone());
    let saved = {
        let mut state = operation.authority.state.lock();
        save_loaded_metadata(&mut state, operation, mapped, &metadata)
    };
    saved.map_err(|(kind, failure)| module_error(kind, "record mapped metadata", failure))
}

pub(super) fn system_confirm_loaded(
    operation: SystemOperation,
    image: VerifiedImage,
    metadata: Metadata,
) -> Result<ModuleView> {
    let mut operation = operation;
    let authority = operation.authority;
    let image = Arc::new(image);
    let confirmation = {
        let mut state = operation.authority.state.lock();
        publish_loaded(&mut state, &mut operation, &image, &metadata)
    };
    match confirmation {
        Ok(LoadConfirmation::Published {
            view,
            fallback,
            retained,
        }) => {
            drop((fallback, retained));
            Ok(view)
        }
        Ok(LoadConfirmation::Reuse(original)) => {
            let Some(files) = operation.files.as_deref() else {
                let failure = module_error(ErrorKind::Corrupted, "read reserved module files", None);
                return Err(operation.reject(failure));
            };
            if let Err(failure) =
                compare_reuse(&original, files).and_then(|()| recheck_reuse(authority, &original))
            {
                return Err(operation.reject(failure));
            }
            // 原代的映像（mapped image）和文件借用须持续到额外引用实际归还并记录结果。
            operation.purpose = SystemPurpose::ReturnDuplicate {
                original_epoch: original.view.epoch,
            };
            drop(image);
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
        SystemPhase::Reserved
            if matches!(operation.purpose, SystemPurpose::Load) && operation.library.is_none() =>
        {
            let original = Arc::new(cause);
            confirm_no_reference(&mut operation, Some(Arc::clone(&original)));
            replay_module_failure(original, "reject native load")
        }
        SystemPhase::ReleaseConfirmed => match operation.finish_confirmed_release() {
            Ok(()) => cause,
            Err(failure) => cause.attach(RecordedFailure(Arc::new(failure))),
        },
        SystemPhase::Finished => cause,
        _ => {
            let original = Arc::new(cause);
            quarantine_mapping(&mut operation, Some(Arc::clone(&original)));
            replay_module_failure(original, "reject native load")
        }
    }
}

pub(super) fn system_release(operation: SystemOperation) -> Result<()> {
    let mut operation = operation;
    match operation.phase {
        SystemPhase::ReleaseConfirmed => return operation.finish_confirmed_release(),
        SystemPhase::OwnsReference => {}
        _ => {
            return Err(module_error(
                ErrorKind::InvalidState,
                "release native reference",
                None,
            ))
        }
    }
    if matches!(operation.purpose, SystemPurpose::Retire)
        && operation
            .files
            .as_ref()
            .is_some_and(|files| files.unload_policy() == UnloadPolicy::RetainUntilExit)
    {
        retain_reference(&mut operation)
    } else {
        release_reference(&mut operation)
    }
}

pub(super) fn system_finish_confirmed_release(operation: &mut SystemOperation) -> Result<()> {
    if !matches!(operation.phase, SystemPhase::ReleaseConfirmed) {
        return Err(module_error(
            ErrorKind::InvalidState,
            "finish confirmed native release",
            None,
        ));
    }
    let materials = {
        let mut state = operation.authority.state.lock();
        confirmed_release_record(&mut state, operation)
            .map(|record| take_verification(record, operation))
    };
    match materials {
        Ok(materials) => drop(materials),
        Err(failure) => return Err(keep_confirmed_release_failure(operation, failure)),
    }

    let complete = {
        let mut state = operation.authority.state.lock();
        confirmed_release_record(&mut state, operation).and_then(|record| {
            if record.files.is_some() || record.image.is_some() || operation.files.is_some() {
                return Err((ErrorKind::Corrupted, None));
            }
            record.phase = ModulePhase::Released;
            operation.phase = SystemPhase::Finished;
            Ok(record.failure.take())
        })
    };
    match complete {
        Ok(fallback) => {
            drop(fallback);
            Ok(())
        }
        Err(failure) => Err(keep_confirmed_release_failure(operation, failure)),
    }
}

fn release_reference(operation: &mut SystemOperation) -> Result<()> {
    let started = {
        let mut state = operation.authority.state.lock();
        start_reference_release(&mut state, operation)
    };
    let module = match started {
        Ok(module) => module,
        Err((kind, previous)) => {
            let failure = Arc::new(module_error(
                kind,
                "reserve system reference release",
                previous,
            ));
            quarantine_mapping(operation, Some(Arc::clone(&failure)));
            return Err(replay_module_failure(failure, "release native reference"));
        }
    };
    match loader::close(module) {
        Ok(()) => {
            operation.phase = SystemPhase::ReleaseConfirmed;
            operation.finish_confirmed_release()
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
        SystemPhase::Reserved
            if matches!(operation.purpose, SystemPurpose::Load) && operation.library.is_none() =>
        {
            confirm_no_reference(operation, None)
        }
        SystemPhase::ReleaseConfirmed => {
            // 原记录仍保存归还确认。补完资料及记录步骤，失败也保存在原记录。
            drop(operation.finish_confirmed_release());
        }
        SystemPhase::OwnsReference if matches!(operation.purpose, SystemPurpose::Retire) => {
            if operation.files.is_none() {
                // 核验资料（verification material）是退役函数的局部值，先于系统操作析构。
                drop(finish_retention(operation));
            } else if !restore_retirement(operation, ModulePhase::AwaitingRelease) {
                quarantine_mapping(operation, None);
            }
        }
        _ => quarantine_mapping(operation, None),
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
    Retained(RetainedSnapshot),
}

struct RetainedSnapshot {
    retired_epoch: u64,
    mapped: MappedIdentity,
    deployment: Arc<VerifiedDeployment>,
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
    if mapped != image.identity || !image.shares_files(files) {
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

fn record_for_file(state: &State, identity: loader::FileIdentity) -> Option<&ModuleRecord> {
    state
        .modules
        .iter()
        .filter(|record| {
            record
                .files
                .as_ref()
                .is_some_and(|files| files.identity() == identity)
        })
        .min_by_key(|record| !matches!(record.phase, ModulePhase::Quarantined))
}

fn quarantined_mapping(state: &State, mapped: MappedIdentity, epoch: u64) -> Option<&ModuleRecord> {
    state.modules.iter().find(|record| {
        record.epoch != epoch
            && matches!(record.phase, ModulePhase::Quarantined)
            && record.library.is_some()
            && record.mapped == Some(mapped)
    })
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

fn reserve_retained_load(
    authority: &'static Authority,
    state: &mut State,
    files: &Arc<VerifiedFiles>,
    original: &RetainedSnapshot,
    fallback: &Arc<Error>,
) -> std::result::Result<SystemOperation, ModuleFailure> {
    if let Some(other) = record_for_file(state, files.identity()) {
        let failure = if matches!(other.phase, ModulePhase::Quarantined) {
            module_unavailable(other)
        } else {
            (ErrorKind::Busy, None)
        };
        return Err(failure);
    }
    let position = state
        .retained_images
        .iter()
        .position(|retained| {
            retained.retired_epoch == original.retired_epoch
                && retained.mapped == original.mapped
                && Arc::ptr_eq(&retained.deployment, &original.deployment)
                && matches!(retained.phase, RetentionPhase::Held(_))
        })
        .ok_or((ErrorKind::Busy, None))?;
    if let Some(other) = quarantined_mapping(state, original.mapped, 0) {
        return Err(module_unavailable(other));
    }

    let mut operation = reserve_load_record(authority, state, files, fallback)?;
    let retained = &mut state.retained_images[position];
    // 已在同一短锁（short lock）确认 Held，同时预留了新代和文件持有。
    let RetentionPhase::Held(library) = std::mem::replace(
        &mut retained.phase,
        RetentionPhase::Revalidating {
            epoch: operation.epoch,
        },
    ) else {
        unreachable!();
    };
    operation.purpose = SystemPurpose::Revalidate {
        retired_epoch: original.retired_epoch,
    };
    operation.library = Some(library);
    Ok(operation)
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
    if let Some(other) = quarantined_mapping(state, mapped, epoch) {
        return Err(module_unavailable(other));
    }
    for retained in &state.retained_images {
        if matches!(retained.phase, RetentionPhase::Revalidating { epoch: new } if new == epoch) {
            if retained.mapped != mapped {
                return Err((ErrorKind::Conflict, None));
            }
        } else if retained.mapped == mapped {
            return Err((ErrorKind::Busy, None));
        }
    }
    Ok(())
}

fn save_loaded_metadata(
    state: &mut State,
    operation: &SystemOperation,
    mapped: MappedIdentity,
    metadata: &Arc<Metadata>,
) -> std::result::Result<(), ModuleFailure> {
    let record = state
        .modules
        .iter_mut()
        .find(|record| record.epoch == operation.epoch)
        .ok_or((ErrorKind::NotFound, None))?;
    if !matches!(record.phase, ModulePhase::Loading) {
        return Err(module_unavailable(record));
    }
    if record.mapped != Some(mapped)
        || !record
            .files
            .as_ref()
            .zip(operation.files.as_ref())
            .is_some_and(|(record, operation)| Arc::ptr_eq(record, operation))
    {
        return Err((ErrorKind::Corrupted, None));
    }
    match &record.metadata {
        Some(previous) if **previous != **metadata => Err((ErrorKind::Conflict, None)),
        Some(_) => Ok(()),
        None => {
            record.metadata = Some(Arc::clone(metadata));
            Ok(())
        }
    }
}

enum LoadConfirmation {
    Published {
        view: ModuleView,
        fallback: Option<Arc<Error>>,
        retained: Option<RetainedImage>,
    },
    Reuse(ReuseSnapshot),
}

fn publish_loaded(
    state: &mut State,
    operation: &mut SystemOperation,
    image: &Arc<VerifiedImage>,
    metadata: &Metadata,
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
        || !record
            .files
            .as_ref()
            .zip(operation.files.as_ref())
            .is_some_and(|(record, operation)| {
                Arc::ptr_eq(record, operation) && image.shares_files(record)
            })
    {
        return Err((ErrorKind::Corrupted, None));
    }
    let Some(actual) = &record.metadata else {
        return Err((ErrorKind::Corrupted, None));
    };
    if actual.as_ref() != metadata {
        return Err((ErrorKind::Conflict, None));
    }
    let retained = match operation.purpose {
        SystemPurpose::Load => None,
        SystemPurpose::Revalidate { retired_epoch } => {
            let index = revalidation_position(state, retired_epoch, epoch)
                .ok_or((ErrorKind::Corrupted, None))?;
            if state.retained_images[index].mapped != image.identity {
                return Err((ErrorKind::Conflict, None));
            }
            Some(index)
        }
        _ => return Err((ErrorKind::InvalidState, None)),
    };
    if let Some(other) = state.modules.iter().find(|record| {
        record.epoch != epoch
            && record.mapped == Some(image.identity)
            && !matches!(record.phase, ModulePhase::Loading | ModulePhase::Released)
    }) {
        return if retained.is_none() {
            reuse_snapshot(other).map(LoadConfirmation::Reuse)
        } else {
            Err((ErrorKind::Conflict, None))
        };
    }
    let library = operation
        .library
        .take()
        .ok_or((ErrorKind::Corrupted, None))?;
    let metadata = Arc::clone(actual);
    let record = &mut state.modules[index];
    record.image = Some(Arc::clone(image));
    record.library = Some(library);
    record.phase = ModulePhase::Loaded;
    operation.phase = SystemPhase::Finished;
    Ok(LoadConfirmation::Published {
        view: ModuleView { epoch, metadata },
        fallback: record.failure.take(),
        retained: retained.map(|index| state.retained_images.swap_remove(index)),
    })
}

fn revalidation_position(state: &State, retired_epoch: u64, epoch: u64) -> Option<usize> {
    state.retained_images.iter().position(|retained| {
        retained.retired_epoch == retired_epoch
            && matches!(retained.phase, RetentionPhase::Revalidating { epoch: current } if current == epoch)
    })
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
    if record.failure.is_some() || record.library.is_none() {
        return Err((ErrorKind::Corrupted, None));
    }
    let (Some(files), Some(image), Some(mapped)) = (&record.files, &record.image, record.mapped)
    else {
        return Err((ErrorKind::Corrupted, None));
    };
    if !image.shares_files(files) || image.identity != mapped {
        return Err((ErrorKind::Corrupted, None));
    }
    // 此锁也守所有借用（borrow）的取得。唯一映像和两份文件持有属于本代。
    if Arc::strong_count(image) != 1 || Arc::strong_count(files) != 2 {
        return Err((ErrorKind::Busy, None));
    }
    let Some(library) = record.library.take() else {
        return Err((ErrorKind::Corrupted, None));
    };
    record.phase = ModulePhase::Unloading;
    record.failure = Some(Arc::clone(fallback));
    Ok(Some(SystemOperation {
        authority,
        epoch,
        files: Some(Arc::clone(files)),
        purpose: SystemPurpose::Retire,
        phase: SystemPhase::OwnsReference,
        library: Some(library),
    }))
}

fn start_reference_release(
    state: &mut State,
    operation: &mut SystemOperation,
) -> std::result::Result<NativeModule, ModuleFailure> {
    if !matches!(operation.phase, SystemPhase::OwnsReference) {
        return Err((ErrorKind::InvalidState, None));
    }
    let index = state
        .modules
        .iter()
        .position(|record| record.epoch == operation.epoch)
        .ok_or((ErrorKind::NotFound, None))?;
    let record = &state.modules[index];
    let mapped = record.mapped.ok_or((ErrorKind::Corrupted, None))?;
    if record.library.is_some() || !operation_shares_files(record, operation) {
        return Err((ErrorKind::Corrupted, None));
    }
    match operation.purpose {
        SystemPurpose::Retire => {
            if !matches!(record.phase, ModulePhase::Unloading) {
                return Err(module_unavailable(record));
            }
            if !keeps_verified_image(record, mapped) {
                return Err((ErrorKind::Corrupted, None));
            }
        }
        SystemPurpose::ReturnDuplicate { original_epoch } => {
            if !matches!(record.phase, ModulePhase::Loading) {
                return Err(module_unavailable(record));
            }
            let protected = state.modules.iter().any(|original| {
                original.epoch == original_epoch
                    && matches!(original.phase, ModulePhase::Loaded)
                    && original.library.is_some()
                    && original.mapped == Some(mapped)
                    && keeps_verified_image(original, mapped)
                    && original
                        .image
                        .as_ref()
                        .is_some_and(|image| Arc::strong_count(image) > 1)
                    && original
                        .files
                        .as_ref()
                        .is_some_and(|files| Arc::strong_count(files) > 2)
            });
            if !protected {
                return Err((ErrorKind::InvalidState, None));
            }
        }
        _ => return Err((ErrorKind::InvalidState, None)),
    }
    let library = operation
        .library
        .take()
        .ok_or((ErrorKind::Corrupted, None))?;
    // 从系统归还（system release）开始，后续只按已知代或文件关联。
    state.modules[index].mapped = None;
    operation.phase = SystemPhase::ReleasingReference;
    Ok(library)
}

fn keeps_verified_image(record: &ModuleRecord, mapped: MappedIdentity) -> bool {
    record
        .image
        .as_ref()
        .zip(record.files.as_ref())
        .is_some_and(|(image, files)| image.identity == mapped && image.shares_files(files))
}

fn operation_shares_files(record: &ModuleRecord, operation: &SystemOperation) -> bool {
    record
        .files
        .as_ref()
        .zip(operation.files.as_ref())
        .is_some_and(|(known, current)| Arc::ptr_eq(known, current))
}

fn confirmed_release_record<'a>(
    state: &'a mut State,
    operation: &SystemOperation,
) -> std::result::Result<&'a mut ModuleRecord, ModuleFailure> {
    let record = state
        .modules
        .iter_mut()
        .find(|record| record.epoch == operation.epoch)
        .ok_or((ErrorKind::NotFound, None))?;
    if !matches!(
        (&operation.purpose, &record.phase),
        (SystemPurpose::Retire, ModulePhase::Unloading)
            | (SystemPurpose::ReturnDuplicate { .. }, ModulePhase::Loading),
    ) {
        return Err(module_unavailable(record));
    }
    let materials_moved =
        record.files.is_none() && record.image.is_none() && operation.files.is_none();
    if record.mapped.is_some()
        || record.library.is_some()
        || operation.library.is_some()
        || (!materials_moved && !operation_shares_files(record, operation))
    {
        return Err((ErrorKind::Corrupted, None));
    }
    Ok(record)
}

fn keep_confirmed_release_failure(
    operation: &mut SystemOperation,
    (kind, previous): ModuleFailure,
) -> Error {
    let failure = Arc::new(
        module_error(kind, "finish confirmed native release", previous)
            .attach("操作系统已确认归还（system release confirmed）"),
    );
    quarantine_mapping(operation, Some(Arc::clone(&failure)));
    replay_module_failure(failure, "finish confirmed native release")
}

type VerificationMaterials = (
    Option<Arc<VerifiedFiles>>,
    Option<Arc<VerifiedFiles>>,
    Option<Arc<VerifiedImage>>,
);

fn take_verification(
    record: &mut ModuleRecord,
    operation: &mut SystemOperation,
) -> VerificationMaterials {
    record.mapped = None;
    (
        operation.files.take(),
        record.files.take(),
        record.image.take(),
    )
}

fn retain_reference(operation: &mut SystemOperation) -> Result<()> {
    let files = operation
        .files
        .as_ref()
        .ok_or_else(|| module_error(ErrorKind::Corrupted, "read retiring plugin files", None))?;
    let materials = files.deployment().and_then(|deployment| {
        let reserved = {
            let mut state = operation.authority.state.lock();
            reserve_retention(&mut state, operation, &deployment)
                .map(|index| take_verification(&mut state.modules[index], operation))
        };
        reserved.map_err(|(kind, failure)| module_error(kind, "reserve retained plugin", failure))
    });
    match materials {
        Ok(materials) => drop(materials),
        Err(failure) => {
            if restore_retirement(operation, ModulePhase::Loaded) {
                return Err(failure);
            }
            let failure = Arc::new(failure);
            quarantine_mapping(operation, Some(Arc::clone(&failure)));
            return Err(replay_module_failure(failure, "retain plugin reference"));
        }
    }
    finish_retention(operation)
}

fn reserve_retention(
    state: &mut State,
    operation: &SystemOperation,
    deployment: &Arc<VerifiedDeployment>,
) -> std::result::Result<usize, ModuleFailure> {
    let index = state
        .modules
        .iter()
        .position(|record| record.epoch == operation.epoch)
        .ok_or((ErrorKind::NotFound, None))?;
    let record = &state.modules[index];
    if !matches!(record.phase, ModulePhase::Unloading) {
        return Err(module_unavailable(record));
    }
    let mapped = record.mapped.ok_or((ErrorKind::Corrupted, None))?;
    if record.library.is_some()
        || operation.library.is_none()
        || !operation_shares_files(record, operation)
        || !keeps_verified_image(record, mapped)
    {
        return Err((ErrorKind::Corrupted, None));
    }
    if state
        .retained_images
        .iter()
        .any(|other| other.mapped == mapped || other.deployment.identity() == deployment.identity())
    {
        return Err((ErrorKind::Conflict, None));
    }
    state
        .retained_images
        .try_reserve(1)
        .map_err(|_| (ErrorKind::ResourceExhausted, None))?;
    state.retained_images.push(RetainedImage {
        retired_epoch: operation.epoch,
        deployment: Arc::clone(deployment),
        mapped,
        phase: RetentionPhase::Retiring,
    });
    Ok(index)
}

fn retiring_position(state: &State, epoch: u64) -> Option<usize> {
    state.retained_images.iter().position(|retained| {
        retained.retired_epoch == epoch && matches!(retained.phase, RetentionPhase::Retiring)
    })
}

fn finish_retention(operation: &mut SystemOperation) -> Result<()> {
    let finished = {
        let mut state = operation.authority.state.lock();
        publish_retention(&mut state, operation)
    };
    match finished {
        Ok(fallback) => {
            drop(fallback);
            Ok(())
        }
        Err((kind, previous)) => {
            let failure = Arc::new(module_error(kind, "publish retained plugin", previous));
            quarantine_mapping(operation, Some(Arc::clone(&failure)));
            Err(replay_module_failure(failure, "retain plugin reference"))
        }
    }
}

fn publish_retention(
    state: &mut State,
    operation: &mut SystemOperation,
) -> std::result::Result<Option<Arc<Error>>, ModuleFailure> {
    let retained = retiring_position(state, operation.epoch).ok_or((ErrorKind::Corrupted, None))?;
    let record = state
        .modules
        .iter_mut()
        .find(|record| record.epoch == operation.epoch)
        .ok_or((ErrorKind::NotFound, None))?;
    if !matches!(record.phase, ModulePhase::Unloading) {
        return Err(module_unavailable(record));
    }
    if record.files.is_some()
        || record.image.is_some()
        || record.library.is_some()
        || record.mapped.is_some()
        || operation.files.is_some()
    {
        return Err((ErrorKind::Corrupted, None));
    }
    let library = operation
        .library
        .take()
        .ok_or((ErrorKind::Corrupted, None))?;
    state.retained_images[retained].phase = RetentionPhase::Held(library);
    record.phase = ModulePhase::Released;
    operation.phase = SystemPhase::Finished;
    Ok(record.failure.take())
}

fn restore_retirement(operation: &mut SystemOperation, phase: ModulePhase) -> bool {
    let (fallback, reservation) = {
        let mut state = operation.authority.state.lock();
        let retired = retiring_position(&state, operation.epoch);
        let Some(record) = state
            .modules
            .iter_mut()
            .find(|record| record.epoch == operation.epoch)
        else {
            return false;
        };
        if !matches!(record.phase, ModulePhase::Unloading)
            || record.library.is_some()
            || !operation_shares_files(record, operation)
            || !record
                .mapped
                .is_some_and(|mapped| keeps_verified_image(record, mapped))
        {
            return false;
        }
        let Some(library) = operation.library.take() else {
            return false;
        };
        let fallback = if matches!(phase, ModulePhase::Loaded) {
            record.failure.take()
        } else {
            None
        };
        record.library = Some(library);
        record.phase = phase;
        operation.phase = SystemPhase::Finished;
        (
            fallback,
            retired.map(|index| state.retained_images.swap_remove(index)),
        )
    };
    drop((fallback, reservation));
    true
}

fn confirm_no_reference(operation: &mut SystemOperation, mut result: Option<Arc<Error>>) {
    let files_and_image = {
        let mut state = operation.authority.state.lock();
        state
            .modules
            .iter_mut()
            .find(|record| record.epoch == operation.epoch)
            .map(|record| {
                record.phase = ModulePhase::Released;
                record.mapped = None;
                let previous = std::mem::replace(&mut record.failure, result.take());
                (record.files.take(), record.image.take(), previous)
            })
    };
    operation.phase = SystemPhase::Finished;
    drop((files_and_image, operation.files.take()));
}

fn quarantine_mapping(operation: &mut SystemOperation, mut failure: Option<Arc<Error>>) {
    let cleanup = {
        let mut state = operation.authority.state.lock();
        let revalidating = match operation.purpose {
            SystemPurpose::Revalidate { retired_epoch } => {
                revalidation_position(&state, retired_epoch, operation.epoch)
            }
            _ => None,
        };
        let last_held_mapping = revalidating.map(|index| state.retained_images[index].mapped);
        let Some(record) = state
            .modules
            .iter_mut()
            .find(|record| record.epoch == operation.epoch)
        else {
            operation.phase = SystemPhase::Finished;
            return;
        };
        record.phase = ModulePhase::Quarantined;
        if record.library.is_none() {
            record.library = operation.library.take();
        }
        // 重核开始前中断时，原映像仍由刚接管的同一系统引用（system reference）保护。
        record.mapped = if record.library.is_some() {
            record.mapped.or(last_held_mapping)
        } else {
            None
        };
        let mapped = record.mapped;
        let previous = failure
            .take()
            .and_then(|failure| record.failure.replace(failure));
        let retained = record.failure.clone();
        let still_has_files = record.files.is_some();
        let paired_epoch = match operation.purpose {
            SystemPurpose::ReturnDuplicate { original_epoch } => Some(original_epoch),
            _ => None,
        };
        for other in &mut state.modules {
            if other.epoch != operation.epoch
                && !matches!(other.phase, ModulePhase::Released)
                && (mapped.is_some_and(|mapped| other.mapped == Some(mapped))
                    || paired_epoch == Some(other.epoch))
            {
                other.phase = ModulePhase::Quarantined;
                if other.failure.is_none() {
                    other.failure = retained.clone();
                }
            }
        }
        let retired = match operation.purpose {
            SystemPurpose::Revalidate { .. } => revalidating,
            SystemPurpose::Retire if still_has_files => retiring_position(&state, operation.epoch),
            _ => None,
        };
        (
            previous,
            retained,
            retired.map(|index| state.retained_images.swap_remove(index)),
        )
    };
    operation.phase = SystemPhase::Finished;
    drop(cleanup);
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

fn check_quarantined_version(
    state: &State,
    identity: &Metadata,
) -> std::result::Result<(), ModuleFailure> {
    let occupied = state.modules.iter().find(|record| {
        matches!(record.phase, ModulePhase::Quarantined)
            && (record.metadata.as_deref() == Some(identity)
                || record.files.as_ref().is_some_and(|files| {
                    let approved = files.manifest();
                    approved.uuid == identity.uuid && approved.version == identity.version
                }))
    });
    match occupied {
        Some(record) => Err(module_unavailable(record)),
        None => Ok(()),
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
    check_quarantined_version(state, metadata)?;
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

    let (image, metadata) = ready_for_prepare(state, id.module_epoch)?;
    if matches!(operation, Operation::Initialize) {
        // 已准备轮次（prepared round）的首次派发也在占用检查与调用登记的同一短锁内。
        check_quarantined_version(state, metadata)?;
    }
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

#[cfg(all(test, target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
mod residency_tests {
    //! 用本地 Rust 共享库（shared library）核对装入、引用与退役状态。

    use super::*;
    use crate::{
        host::Manager,
        package::{
            BuildDependency, BuildFile, BuildManifest, DependencyUse, PluginPackage, PluginRequest,
            StaticDeclarations, Target,
        },
    };
    use sha2::{Digest, Sha256};
    use std::{
        fs,
        path::{Path, PathBuf},
        process::Command,
        sync::{
            atomic::{AtomicUsize, Ordering},
            OnceLock,
        },
    };

    const ACTUAL_UUID: [u8; 16] = [23; 16];
    const VERSION: &str = "1.2.3+residency";
    const FIXTURE: &str = r#"
use pi_ffi_abi::management::*;

static VERSION: &[u8] = b"1.2.3+residency";

fn outcome(kind: u32, diagnostic_failure_kind: u32) -> OutcomeV1 {
    OutcomeV1 {
        kind,
        diagnostic_failure_kind,
        // SAFETY: 共同诊断表示（diagnostic representation）的整数和裸指针均可为零。
        diagnostic: unsafe { std::mem::zeroed() },
    }
}

#[no_mangle]
unsafe extern "C" fn pi_ffi_metadata_v1(out: *mut MetadataV1) -> OutcomeV1 {
    // SAFETY: 宿主提供独占输出槽（output slot），版本字节具有静态存续。
    unsafe {
        out.write(MetadataV1 {
            uuid: UuidV1 { bytes: [23; 16] },
            version: VERSION.as_ptr(),
            version_len: VERSION.len() as u64,
        });
    }
    outcome(0, 0)
}

#[no_mangle]
extern "C" fn pi_ffi_initialize_v1(
    _: *const ObjectRefV1,
    _: *const ObjectRefV1,
    _: *const ObjectRefV1,
) -> InitReplyV1 {
    InitReplyV1 { progress: INIT_NOT_STARTED, outcome: outcome(1, 1) }
}

#[no_mangle]
extern "C" fn pi_ffi_close_v1(_: InitIdV1) -> OutcomeV1 {
    outcome(1, 1)
}
"#;

    /// 本测试进程内唯一临时目录（temporary directory），析构时清理创建的文件。
    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            loop {
                let id = NEXT.fetch_add(1, Ordering::Relaxed);
                let path =
                    std::env::temp_dir().join(format!("pi-ffi-t24-{}-{id}", std::process::id()));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(failure) if failure.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(failure) => panic!("{failure}"),
                }
            }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// 使用仓库的共同应用二进制接口（ABI），只编译当前单元用例的装入输入。
    fn fixture_binary() -> &'static [u8] {
        static BINARY: OnceLock<Vec<u8>> = OnceLock::new();
        BINARY.get_or_init(|| compile_fixture(FIXTURE))
    }

    fn compile_fixture(text: &str) -> Vec<u8> {
        let dir = TestDirectory::new();
        let abi_source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../pi_ffi_abi/src/lib.rs");
        let abi = dir.0.join("libpi_ffi_abi.rlib");
        let source = dir.0.join("fixture.rs");
        let library = dir.0.join("fixture.so");
        fs::write(&source, text).unwrap();
        let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        compile(
            Command::new(&compiler)
                .args([
                    "--crate-name",
                    "pi_ffi_abi",
                    "--crate-type=rlib",
                    "--edition=2021",
                ])
                .arg(abi_source)
                .arg("-o")
                .arg(&abi),
        );
        compile(
            Command::new(&compiler)
                .args([
                    "--crate-name",
                    "t24_fixture",
                    "--crate-type=cdylib",
                    "--edition=2021",
                ])
                .arg("--extern")
                .arg(format!("pi_ffi_abi={}", abi.display()))
                .arg(source)
                .arg("-o")
                .arg(&library),
        );
        fs::read(library).unwrap()
    }

    /// 编译失败时保留原命令与编译器诊断（compiler diagnostic）。
    fn compile(command: &mut Command) {
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{command:?}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// 每份制品（artifact）有独立实际文件身份，仅用于共同元信息边界。
    struct Artifact {
        dir: TestDirectory,
        path: PathBuf,
        digest: [u8; 32],
        version: &'static str,
    }

    impl Artifact {
        fn new() -> Self {
            Self::from_binary(fixture_binary(), VERSION)
        }

        fn from_binary(binary: &[u8], version: &'static str) -> Self {
            let dir = TestDirectory::new();
            let path = dir.0.join("fixture.so");
            fs::write(&path, binary).unwrap();
            Self {
                dir,
                path,
                digest: Sha256::digest(binary).into(),
                version,
            }
        }

        fn package(&self, uuid: [u8; 16]) -> PluginPackage {
            self.package_from(self.manifest(uuid))
        }

        fn package_from(&self, manifest: BuildManifest) -> PluginPackage {
            // 安全前提（safety）。本测试编译并保持制品（artifact），仅使用共同元信息边界。
            unsafe { PluginPackage::from_verified_build(self.dir.0.clone(), manifest) }.unwrap()
        }

        fn unloadable(&self) -> PluginPackage {
            // 安全前提（safety）。本制品只读取静态元信息（metadata），无业务、任务或退出回调。
            unsafe {
                PluginPackage::from_verified_unloadable_build(
                    self.dir.0.clone(),
                    self.manifest(ACTUAL_UUID),
                )
            }
            .unwrap()
        }

        fn manifest(&self, uuid: [u8; 16]) -> BuildManifest {
            let host = build();
            BuildManifest {
                target: Target::LinuxX86_64Gnu,
                uuid,
                version: self.version.parse().unwrap(),
                library: BuildFile {
                    path: self.path.clone(),
                    sha256: self.digest,
                },
                dependencies: Vec::new(),
                rust_build_sha256: host.rust_build_sha256,
                allocator: host.allocator,
                static_declarations: StaticDeclarations::Empty,
            }
        }

        fn mapped(&self) -> bool {
            // 读取进程映射（process mappings），不增加额外系统引用（system reference）。
            let path = self.path.canonicalize().unwrap();
            fs::read_to_string("/proc/self/maps")
                .unwrap()
                .lines()
                .any(|line| line.ends_with(path.to_str().unwrap()))
        }
    }

    fn build() -> HostBuild {
        HostBuild {
            rust_build_sha256: [42; 32],
            allocator: "unit test".into(),
        }
    }

    fn request(uuid: [u8; 16]) -> PluginRequest {
        PluginRequest {
            uuid,
            version: "=1.2.3".parse().unwrap(),
        }
    }

    fn loaded_epoch(package: &PluginPackage) -> u64 {
        let files = loader::verify_files(package, &build()).unwrap();
        let LoadAdmission::Reuse(view) = AUTHORITY.reserve_load(files).unwrap() else {
            panic!("等待已活动的管理代（active generation）");
        };
        view.epoch
    }

    fn new_operation(package: &PluginPackage) -> SystemOperation {
        let files = loader::verify_files(package, &build()).unwrap();
        let LoadAdmission::New(operation) = AUTHORITY.reserve_load(files).unwrap() else {
            panic!("等待新装入操作（new load operation）");
        };
        operation
    }

    /// 默认退役（default retirement）应结束旧文件持有和原代，保留实际映像供新代接入。
    #[test]
    fn test_t24_default_retirement_retains_mapping() {
        let artifact = Artifact::new();
        let manager = Manager::process(build()).unwrap();
        let package = artifact.package(ACTUAL_UUID);
        let request = request(ACTUAL_UUID);
        let original = manager.load(&package, &request).unwrap();
        let epoch = loaded_epoch(&package);
        let files = {
            let state = AUTHORITY.state.lock();
            let record = state
                .modules
                .iter()
                .find(|record| record.epoch == epoch)
                .unwrap();
            Arc::downgrade(record.files.as_ref().unwrap())
        };
        assert!(artifact.mapped());
        original.unload().unwrap();
        assert!(
            artifact.mapped(),
            "默认退役仍须持有实际映像（mapped image）"
        );
        assert!(files.upgrade().is_none());
        original.unload().unwrap();
        assert_eq!(original.metadata().version.to_string(), VERSION);
        let reloaded = manager.load(&package, &request).unwrap();
        let reloaded_epoch = loaded_epoch(&package);
        assert_ne!(epoch, reloaded_epoch);
        original.unload().unwrap();
        assert_eq!(reloaded.metadata(), original.metadata());
        reloaded.unload().unwrap();
        let retained = {
            let mut state = AUTHORITY.state.lock();
            assert!(state
                .retained_images
                .iter()
                .all(|record| record.retired_epoch != epoch));
            let index = state
                .retained_images
                .iter()
                .position(|record| record.retired_epoch == reloaded_epoch)
                .unwrap();
            state.retained_images.swap_remove(index)
        };
        let RetentionPhase::Held(reference) = retained.phase else {
            panic!("保留记录（retention record）应已持有实际引用");
        };
        // 清理本单元制品并观察单次系统归还（system release）是否足以解除映像。
        loader::close(reference).unwrap();
        assert!(!artifact.mapped());
    }

    /// 卸载（unload）遇到活动复用（active reuse）的借用时返回 Busy，借用结束后可重试。
    #[test]
    fn test_t24_retirement_checks_active_borrows() {
        let artifact = Artifact::new();
        let manager = Manager::process(build()).unwrap();
        let package = artifact.package(ACTUAL_UUID);
        let plugin = manager.load(&package, &request(ACTUAL_UUID)).unwrap();
        let epoch = loaded_epoch(&package);
        let borrowed = {
            let state = AUTHORITY.state.lock();
            let record = state
                .modules
                .iter()
                .find(|record| record.epoch == epoch)
                .unwrap();
            reuse_snapshot(record).unwrap()
        };
        let failure = plugin
            .unload()
            .expect_err("活动借用（active borrow）仍在时不得退役");
        assert_eq!(*failure.current_context(), ErrorKind::Busy);
        assert!(artifact.mapped());
        drop(borrowed);
        plugin.unload().unwrap();
    }

    /// 装入（load）在实际元信息不匹配时保留原诊断及两个完整身份。
    #[test]
    fn test_t24_rejected_mapping_retains_reference_and_identity() {
        let artifact = Artifact::new();
        let manager = Manager::process(build()).unwrap();
        let approved = [24; 16];
        let package = artifact.package(approved);
        let request = request(approved);
        let rejected = manager.load(&package, &request).err().unwrap();
        assert_eq!(*rejected.current_context(), ErrorKind::Conflict);
        assert!(
            artifact.mapped(),
            "装入后拒绝仍须保全实际引用（system reference）"
        );
        let replay = manager.load(&package, &request).err().unwrap();
        assert_eq!(*replay.current_context(), ErrorKind::Conflict);
        assert!(pi_result::debug_message(&replay).contains("实际 UUID"));
        let identity = loader::file_identity(&package).unwrap();
        let state = AUTHORITY.state.lock();
        let record = state
            .modules
            .iter()
            .find(|record| {
                record
                    .files
                    .as_ref()
                    .is_some_and(|files| files.identity() == identity)
            })
            .unwrap();
        assert!(matches!(record.phase, ModulePhase::Quarantined));
        assert!(record.library.is_some());
        assert_eq!(record.metadata.as_ref().unwrap().uuid, ACTUAL_UUID);
        assert_eq!(record.files.as_ref().unwrap().manifest().uuid, approved);
    }

    /// 装入操作（load operation）中断后仍持一份真实引用，维护不取得退役资格。
    #[test]
    fn test_t24_interrupted_load_is_not_scheduled_for_release() {
        let artifact = Artifact::new();
        let package = artifact.package(ACTUAL_UUID);
        let mut operation = new_operation(&package);
        let epoch = operation.epoch();
        operation.open().unwrap();
        let mapped = loader::mapped_identity(operation.library().unwrap()).unwrap();
        AUTHORITY.record_mapping(epoch, mapped).unwrap();
        drop(operation);
        assert!(artifact.mapped());
        let state = AUTHORITY.state.lock();
        let record = state
            .modules
            .iter()
            .find(|record| record.epoch == epoch)
            .unwrap();
        assert!(
            matches!(record.phase, ModulePhase::Quarantined),
            "装入中断须隔离（quarantine）"
        );
        assert!(record.library.is_some());
    }

    /// 活动复用和保留复用（retained reuse）核对策略、实际依赖身份与同文件别名。
    #[test]
    fn test_t24_load_checks_policy_and_dependency_identity() {
        let artifact = Artifact::new();
        let manager = Manager::process(build()).unwrap();
        let request = request(ACTUAL_UUID);
        let plain = artifact.package(ACTUAL_UUID);
        let unloadable = artifact.unloadable();
        assert!(unloadable.clone().unload_policy() == UnloadPolicy::ReleaseWhenQuiescent);
        assert_eq!(
            *manager
                .find(&[plain, unloadable.clone()], &request)
                .err()
                .unwrap()
                .current_context(),
            ErrorKind::Conflict,
        );

        let alias = artifact.dir.0.join("alias.so");
        fs::hard_link(&artifact.path, &alias).unwrap();
        let companion = artifact.dir.0.join("companion.so");
        let different = artifact.dir.0.join("different.so");
        fs::write(&companion, fixture_binary()).unwrap();
        fs::write(&different, fixture_binary()).unwrap();
        let mut manifest = artifact.manifest(ACTUAL_UUID);
        manifest.dependencies.push(BuildDependency {
            load_name: "companion.so".into(),
            file: BuildFile {
                path: companion,
                sha256: manifest.library.sha256,
            },
            usage: DependencyUse::RuntimeInput,
        });
        let package = artifact.package_from(manifest.clone());
        // 安全前提（safety）。同一制品仍只经历元信息检查和已核依赖的路径核对。
        let strategy_changed = unsafe {
            PluginPackage::from_verified_unloadable_build(artifact.dir.0.clone(), manifest.clone())
        }
        .unwrap();
        manifest.library.path = alias;
        let same = artifact.package_from(manifest.clone());
        manifest.dependencies[0].file.path = different;
        let changed = artifact.package_from(manifest);

        let plugin = manager.load(&package, &request).unwrap();
        assert_eq!(
            manager.load(&same, &request).unwrap().metadata(),
            plugin.metadata()
        );
        for candidate in [&strategy_changed, &changed] {
            let rejection = manager.load(candidate, &request).err().unwrap();
            assert_eq!(*rejection.current_context(), ErrorKind::Conflict);
        }
        plugin.unload().unwrap();
        for candidate in [&strategy_changed, &changed] {
            let rejection = manager.load(candidate, &request).err().unwrap();
            assert_eq!(*rejection.current_context(), ErrorKind::Conflict);
        }
        manager.load(&same, &request).unwrap().unload().unwrap();
    }

    /// 两次重新接入（revalidation）交错时只交出原引用一次，原代继续重放。
    #[test]
    fn test_t24_retained_admission_is_exclusive() {
        let artifact = Artifact::new();
        let package = artifact.package(ACTUAL_UUID);
        let request = request(ACTUAL_UUID);
        let manager = Manager::process(build()).unwrap();
        let original = manager.load(&package, &request).unwrap();
        let old_epoch = loaded_epoch(&package);
        original.unload().unwrap();
        let pending = new_operation(&package);
        let epoch = pending.epoch();
        assert_ne!(old_epoch, epoch);
        let mapped = loader::mapped_identity(pending.library.as_ref().unwrap()).unwrap();
        let racing = manager.load(&package, &request).err().unwrap();
        assert_eq!(*racing.current_context(), ErrorKind::Busy);
        original.unload().unwrap();
        assert!(artifact.mapped());
        drop(pending);
        let replay = manager.load(&package, &request).err().unwrap();
        assert_eq!(*replay.current_context(), ErrorKind::Internal);
        assert!(pi_result::debug_message(&replay).contains("native load interrupted"));
        original.unload().unwrap();
        let state = AUTHORITY.state.lock();
        assert!(state
            .retained_images
            .iter()
            .all(|record| record.retired_epoch != old_epoch));
        let record = state
            .modules
            .iter()
            .find(|record| record.epoch == epoch)
            .unwrap();
        assert!(matches!(record.phase, ModulePhase::Quarantined));
        assert!(record.library.is_some());
        assert!(record.mapped == Some(mapped));
    }

    /// 原加载路径消失时实际映像重核（image revalidation）失败，新代保存原文件错误。
    #[test]
    fn test_t24_failed_revalidation_replays_native_diagnostic() {
        let artifact = Artifact::new();
        let alias = artifact.dir.0.join("alias.so");
        fs::hard_link(&artifact.path, &alias).unwrap();
        let package = artifact.package(ACTUAL_UUID);
        let mut manifest = artifact.manifest(ACTUAL_UUID);
        manifest.library.path = alias;
        let alternative = artifact.package_from(manifest);
        let request = request(ACTUAL_UUID);
        let manager = Manager::process(build()).unwrap();
        let original = manager.load(&package, &request).unwrap();
        let epoch = loaded_epoch(&package);
        original.unload().unwrap();

        let renamed = artifact.dir.0.join("renamed.so");
        fs::rename(&artifact.path, &renamed).unwrap();
        for _ in 0..2 {
            let native = manager.load(&alternative, &request).err().unwrap();
            assert_eq!(*native.current_context(), ErrorKind::Io);
            assert!(pi_result::debug_message(&native).contains(artifact.path.to_str().unwrap()));
        }
        original.unload().unwrap();
        assert_eq!(original.metadata().version.to_string(), VERSION);
        let state = AUTHORITY.state.lock();
        assert!(state
            .retained_images
            .iter()
            .all(|retained| retained.retired_epoch != epoch));
    }

    /// 在原文件移出前后中断保留交接（retention handoff），分别由维护和析构完成。
    #[test]
    fn test_t24_retirement_recovers_both_handoff_windows() {
        let manager = Manager::process(build()).unwrap();
        let request = request(ACTUAL_UUID);
        for moved in [false, true] {
            let artifact = Artifact::new();
            let package = artifact.package(ACTUAL_UUID);
            let plugin = manager.load(&package, &request).unwrap();
            let epoch = loaded_epoch(&package);
            let mut operation = AUTHORITY.prepare_unload(epoch).unwrap().unwrap();
            let deployment = operation.files.as_ref().unwrap().deployment().unwrap();
            let materials = {
                let mut state = AUTHORITY.state.lock();
                let index = reserve_retention(&mut state, &operation, &deployment).unwrap();
                moved.then(|| take_verification(&mut state.modules[index], &mut operation))
            };
            drop(materials);
            let loading = manager.load(&package, &request).err().unwrap();
            assert_eq!(*loading.current_context(), ErrorKind::Busy);
            drop(operation);

            if !moved {
                assert_eq!(
                    *plugin.unload().unwrap_err().current_context(),
                    ErrorKind::Busy
                );
                assert!(artifact.mapped());
                assert_eq!(manager.maintain(1).unwrap(), 1);
            }
            plugin.unload().unwrap();
            assert!(artifact.mapped());
            let state = AUTHORITY.state.lock();
            let record = state
                .modules
                .iter()
                .find(|record| record.epoch == epoch)
                .unwrap();
            assert!(matches!(record.phase, ModulePhase::Released));
            assert!(record.files.is_none() && record.image.is_none() && record.library.is_none());
            assert!(state.retained_images.iter().any(|retained| {
                retained.retired_epoch == epoch && matches!(retained.phase, RetentionPhase::Held(_))
            }));
        }
    }

    /// 实际归还（system release）成功后中断确认，析构仅结清资料和旧代。
    #[test]
    fn test_t24_confirmed_release_recovers_without_returning_again() {
        let manager = Manager::process(build()).unwrap();
        let request = request(ACTUAL_UUID);
        let artifact = Artifact::new();
        let package = artifact.unloadable();
        let plugin = manager.load(&package, &request).unwrap();
        let epoch = loaded_epoch(&package);
        let mut operation = AUTHORITY.prepare_unload(epoch).unwrap().unwrap();
        let reference = {
            let mut state = AUTHORITY.state.lock();
            start_reference_release(&mut state, &mut operation).unwrap()
        };
        assert!(artifact.mapped());
        loader::close(reference).unwrap();
        operation.phase = SystemPhase::ReleaseConfirmed;
        assert!(!artifact.mapped());
        assert_eq!(
            *plugin.unload().unwrap_err().current_context(),
            ErrorKind::Busy
        );
        drop(operation);
        plugin.unload().unwrap();
        assert!(!artifact.mapped());
        let reloaded = manager.load(&package, &request).unwrap();
        assert_ne!(loaded_epoch(&package), epoch);
        reloaded.unload().unwrap();
        assert!(!artifact.mapped());
    }

    /// 系统归还结果（release result）尚未登记时中断，同文件重放原错误。
    #[test]
    fn test_t24_unconfirmed_release_drops_stale_address() {
        let artifact = Artifact::new();
        let manager = Manager::process(build()).unwrap();
        let package = artifact.unloadable();
        let request = request(ACTUAL_UUID);
        let plugin = manager.load(&package, &request).unwrap();
        let epoch = loaded_epoch(&package);
        let mut operation = AUTHORITY.prepare_unload(epoch).unwrap().unwrap();
        let reference = {
            let mut state = AUTHORITY.state.lock();
            let reference = start_reference_release(&mut state, &mut operation).unwrap();
            let record = state
                .modules
                .iter()
                .find(|record| record.epoch == epoch)
                .unwrap();
            assert!(record.mapped.is_none());
            reference
        };
        loader::close(reference).unwrap();
        drop(operation);
        let original = plugin.unload().unwrap_err();
        let replay = manager.load(&package, &request).err().unwrap();
        assert_eq!(*original.current_context(), ErrorKind::Internal);
        assert_eq!(*replay.current_context(), ErrorKind::Internal);
        assert!(pi_result::debug_message(&replay).contains("native release interrupted"));
        assert!(!artifact.mapped());
        let other = Artifact::new();
        manager
            .load(&other.package(ACTUAL_UUID), &request)
            .unwrap()
            .unload()
            .unwrap();
    }

    /// 同一映像的额外系统引用（duplicate reference）须在原引用受保护时归还。
    #[test]
    fn test_t24_duplicate_reference_requires_original_protection() {
        let manager = Manager::process(build()).unwrap();
        let request = request(ACTUAL_UUID);
        for protected in [true, false] {
            let artifact = Artifact::new();
            let package = artifact.unloadable();
            let plugin = manager.load(&package, &request).unwrap();
            let epoch = loaded_epoch(&package);
            let files = loader::verify_files(&package, &build()).unwrap();
            let interruption = Arc::new(Error::new(ErrorKind::Internal));
            let mut duplicate = {
                let mut state = AUTHORITY.state.lock();
                // 单独预留第二份实际装入（native load），以触达系统已返回额外引用的分支。
                reserve_load_record(&AUTHORITY, &mut state, &files, &interruption).unwrap()
            };
            duplicate.open().unwrap();
            let image = loader::verify_image(duplicate.library().unwrap(), files).unwrap();
            AUTHORITY
                .record_mapping(duplicate.epoch(), image.identity)
                .unwrap();
            let mut raw = std::mem::MaybeUninit::uninit();
            // 安全前提（safety）。当前实际引用保活已核标准入口，输出槽独占可写。
            let outcome = unsafe { (image.entries.metadata)(raw.as_mut_ptr()) };
            assert_eq!(outcome.kind, 0);
            // 安全前提（safety）。成功写入后从仍受保护的映像复制数据。
            let metadata = unsafe { loader::copy_metadata(&image, raw.assume_init()) }.unwrap();
            duplicate.record_metadata(&metadata).unwrap();
            duplicate.record_metadata(&metadata).unwrap();
            let changed = Metadata {
                uuid: [99; 16],
                version: metadata.version.clone(),
            };
            assert_eq!(
                *duplicate
                    .record_metadata(&changed)
                    .unwrap_err()
                    .current_context(),
                ErrorKind::Conflict,
            );

            if protected {
                let reused = duplicate.confirm_loaded(image, metadata).unwrap();
                assert_eq!(reused.epoch, epoch);
                assert!(artifact.mapped());
                plugin.unload().unwrap();
                assert!(!artifact.mapped());
            } else {
                let duplicate_epoch = duplicate.epoch();
                let original = {
                    let state = AUTHORITY.state.lock();
                    let record = state
                        .modules
                        .iter()
                        .find(|record| record.epoch == epoch)
                        .unwrap();
                    reuse_snapshot(record).unwrap()
                };
                compare_reuse(&original, duplicate.files.as_ref().unwrap()).unwrap();
                recheck_reuse(&AUTHORITY, &original).unwrap();
                duplicate.purpose = SystemPurpose::ReturnDuplicate {
                    original_epoch: epoch,
                };
                drop(original);
                drop(image);
                assert_eq!(
                    *duplicate.release().unwrap_err().current_context(),
                    ErrorKind::InvalidState
                );
                assert!(artifact.mapped());
                let state = AUTHORITY.state.lock();
                let record = state
                    .modules
                    .iter()
                    .find(|record| record.epoch == duplicate_epoch)
                    .unwrap();
                assert!(matches!(record.phase, ModulePhase::Quarantined));
                assert!(record.library.is_some());
            }
        }
    }

    /// 没有系统引用（system reference）的拒绝结束装入预留，同文件可以再次申请。
    #[test]
    fn test_t24_rejected_reservation_without_reference_can_retry() {
        let artifact = Artifact::new();
        let package = artifact.package(ACTUAL_UUID);
        let reserved = new_operation(&package);
        let epoch = reserved.epoch();
        assert!(matches!(reserved.phase, SystemPhase::Reserved));
        let rejected =
            reserved.reject(Error::new(ErrorKind::Conflict).attach("pre-open unit input"));
        assert_eq!(*rejected.current_context(), ErrorKind::Conflict);
        assert!(pi_result::debug_message(&rejected).contains("pre-open unit input"));
        assert!(!artifact.mapped());
        let next = new_operation(&package);
        assert_ne!(next.epoch(), epoch);
        let state = AUTHORITY.state.lock();
        let record = state
            .modules
            .iter()
            .find(|record| record.epoch == epoch)
            .unwrap();
        assert!(matches!(record.phase, ModulePhase::Released));
        assert!(record.library.is_none() && record.files.is_none());
    }

    /// 用实际已核身份检查同一完整版本的两处准入，并允许另一构建版本准备。
    #[test]
    fn test_t24_quarantine_blocks_full_identity_at_both_round_gates() {
        const OCCUPIED: &str = "1.2.3+occupied";
        const INDEPENDENT: &str = "1.2.3+independent";
        let manager = Manager::process(build()).unwrap();
        let native = compile_fixture(&FIXTURE.replace(VERSION, OCCUPIED));
        let artifact = Artifact::from_binary(&native, OCCUPIED);
        let package = artifact.package(ACTUAL_UUID);
        let actual_request = request(ACTUAL_UUID);
        let plugin = manager.load(&package, &actual_request).unwrap();
        let epoch = loaded_epoch(&package);
        let prepared = {
            let mut state = AUTHORITY.state.lock();
            let image = Arc::clone(ready_for_prepare(&state, epoch).unwrap().0);
            let prepared = reserve_round_record(&mut state, epoch, &image).unwrap();
            // 只给首次派发（first dispatch）的准入分支建立已准备状态。
            state
                .rounds
                .iter_mut()
                .find(|round| same_round(&round.id, &prepared))
                .unwrap()
                .phase = RoundPhase::Prepared;
            prepared
        };

        let approved_uuid = [81; 16];
        let rejected = Artifact::from_binary(&native, "1.2.3+approved");
        let failure = manager
            .load(&rejected.package(approved_uuid), &request(approved_uuid))
            .err()
            .unwrap();
        assert_eq!(*failure.current_context(), ErrorKind::Conflict);
        assert!(rejected.mapped());
        let preparing = AUTHORITY.reserve_round(epoch, false).err().unwrap();
        let starting = AUTHORITY
            .begin_round(round_id(&prepared), Operation::Initialize)
            .err()
            .unwrap();
        for failure in [preparing, starting] {
            assert_eq!(*failure.current_context(), ErrorKind::Conflict);
            assert!(pi_result::debug_message(&failure).contains("批准版本"));
        }
        {
            let state = AUTHORITY.state.lock();
            let approved = Metadata {
                uuid: approved_uuid,
                version: rejected.version.parse().unwrap(),
            };
            assert_eq!(
                check_quarantined_version(&state, &approved).unwrap_err().0,
                ErrorKind::Conflict
            );
            assert!(state.rounds.iter().any(|round| {
                same_round(&round.id, &prepared) && matches!(round.phase, RoundPhase::Prepared)
            }));
            assert!(state
                .calls
                .iter()
                .all(|call| !same_round(&call.id, &prepared)));
        }
        assert!(matches!(
            AUTHORITY
                .begin_round(round_id(&prepared), Operation::Close)
                .unwrap(),
            RoundAdmission::ReturnHoldings
        ));
        AUTHORITY.maintain(Some(prepared), 1).unwrap();
        plugin.unload().unwrap();

        let independent = compile_fixture(&FIXTURE.replace(VERSION, INDEPENDENT));
        let other = Artifact::from_binary(&independent, INDEPENDENT);
        let other_package = other.package(ACTUAL_UUID);
        let other_plugin = manager.load(&other_package, &actual_request).unwrap();
        let other_epoch = loaded_epoch(&other_package);
        let other_round = {
            let mut state = AUTHORITY.state.lock();
            let image = Arc::clone(ready_for_prepare(&state, other_epoch).unwrap().0);
            reserve_round_record(&mut state, other_epoch, &image).unwrap()
        };
        let cancelled =
            AUTHORITY.fail_prepare(round_id(&other_round), Error::new(ErrorKind::InvalidInput));
        assert_eq!(*cancelled.current_context(), ErrorKind::InvalidInput);
        AUTHORITY.maintain(Some(other_round), 2).unwrap();
        other_plugin.unload().unwrap();
    }
}
