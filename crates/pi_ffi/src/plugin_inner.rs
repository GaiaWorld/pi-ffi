use super::*;
use pi_ffi_abi::management::{INIT_COMPLETED, INIT_NEEDS_CLOSE, INIT_NOT_STARTED};
use pi_ffi_adapters::config::ConfigInput;
#[cfg(feature = "init_v2")]
use pi_ffi_adapters::data::DataInput;
use pi_ffi_core::{
    boundary::{self, Context},
    control::Connection,
    diagnostic,
};
use pi_result::{attachment::Operation, Error, ErrorKind};

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

#[cfg(feature = "init_v2")]
pub(super) unsafe fn export_initialize<S, T, R, D>(
    exports: &Exports<S>,
    runtime: *const ObjectRefV1,
    terminal: *const ObjectRefV1,
    config: *const ObjectRefV1,
    data: *const ObjectRefV1,
    initialize: impl FnOnce(Inputs<T, R, D>, &mut Option<S>) -> Result<()>,
) -> InitReplyV1
where
    S: Send + 'static,
    T: Send + Sync + 'static,
    R: IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static,
    D: Send + Sync + 'static,
{
    initialize_export(
        exports,
        runtime,
        terminal,
        config,
        data,
        initialize,
        |connection, runtime| {
            // 安全前提（safety）。本次仍由原宿主同步保活所传输入，并已核同轮连接。
            unsafe { Runtime::<R>::from_optional_input(connection, runtime) }
        },
    )
}

#[cfg(feature = "init_v2")]
pub(super) unsafe fn export_initialize_without_runtime<
    S: Send + 'static,
    T: Send + Sync + 'static,
    D: Send + Sync + 'static,
>(
    exports: &Exports<S>,
    runtime: *const ObjectRefV1,
    terminal: *const ObjectRefV1,
    config: *const ObjectRefV1,
    data: *const ObjectRefV1,
    initialize: impl FnOnce(Inputs<T, (), D>, &mut Option<S>) -> Result<()>,
) -> InitReplyV1 {
    if !runtime.is_null() {
        let outcome = diagnostic::protect_outcome(
            call_context("reject runtime for plugin export", None),
            move || {
                drop(initialize);
                limited(ErrorKind::InvalidInput)
            },
        );
        return InitReplyV1 {
            progress: INIT_NOT_STARTED,
            outcome,
        };
    }
    initialize_export(exports, runtime, terminal, config, data, initialize, |_, _| {
        Ok(None)
    })
}

pub(super) unsafe fn export_close<S: Send + 'static>(
    exports: &Exports<S>,
    id: InitIdV1,
    close: impl FnOnce(&mut Option<S>) -> Result<()>,
) -> OutcomeV1 {
    diagnostic::protect_outcome(call_context("close plugin export", Some(&id)), || {
        close_export(exports, &id, close)
    })
}

fn call_context(operation: &'static str, id: Option<&InitIdV1>) -> Context {
    Context {
        operation,
        module_epoch: id.map_or(0, |id| id.module_epoch),
        round: id.map_or(0, |id| id.round),
        source: 0,
    }
}

fn plugin_error(kind: ErrorKind, operation: &'static str) -> Error {
    Error::new(kind).attach(Operation(operation))
}

fn same_id(left: &InitIdV1, right: &InitIdV1) -> bool {
    left.module_epoch == right.module_epoch && left.round == right.round
}

fn copy_id(id: &InitIdV1) -> InitIdV1 {
    InitIdV1 {
        module_epoch: id.module_epoch,
        round: id.round,
    }
}

fn copy_lease(lease: &LeaseV1) -> LeaseV1 {
    LeaseV1 {
        object: pi_ffi_abi::ObjectKeyV1 {
            source: lease.object.source,
            slot: lease.object.slot,
            generation: lease.object.generation,
        },
        initialization: copy_id(&lease.initialization),
        holding: lease.holding,
    }
}

fn same_lease(left: &LeaseV1, right: &LeaseV1) -> bool {
    left.object.source == right.object.source
        && left.object.slot == right.object.slot
        && left.object.generation == right.object.generation
        && left.holding == right.holding
        && same_id(&left.initialization, &right.initialization)
}

fn same_inputs(left: &BoundInputs, right: &BoundInputs) -> bool {
    match (left, right) {
        #[cfg(feature = "init_v1")]
        (BoundInputs::V1 { config: lc, terminal: lt, runtime: lr }, BoundInputs::V1 { config: rc, terminal: rt, runtime: rr }) => same_lease(lc, rc) && same_lease(lt, rt) && match (lr, rr) {
                (None, None) => true,
                (Some(left), Some(right)) => same_lease(left, right),
                _ => false,
            },
        #[cfg(feature = "init_v2")]
        (BoundInputs::V2 { config: lc, terminal: lt, runtime: lr, data: ld }, BoundInputs::V2 { config: rc, terminal: rt, runtime: rr, data: rd }) => same_lease(ld, rd) && (same_lease(lc, rc) && same_lease(lt, rt) && match (lr, rr) {
                (None, None) => true,
                (Some(left), Some(right)) => same_lease(left, right),
                _ => false,
            }),
        #[cfg(all(feature = "init_v1", feature = "init_v2"))]
        _ => false,
    }
}

unsafe fn protected_input<'call>(input: *const ObjectRefV1) -> Result<&'call ObjectRefV1> {
    if input.is_null() || !input.is_aligned() {
        return Err(plugin_error(
            ErrorKind::InvalidInput,
            "read plugin initialization input",
        ));
    }
    // 安全前提（safety）。标准宿主已具备完整的同步借用，地址仅在本入口使用。
    let input = unsafe { &*input };
    let lease = &input.lease;
    if input.context.is_null()
        || input.api.is_null()
        || lease.object.source == 0
        || lease.object.slot == 0
        || lease.object.generation == 0
        || lease.holding == 0
        || lease.initialization.module_epoch == 0
        || lease.initialization.round == 0
    {
        return Err(plugin_error(
            ErrorKind::InvalidInput,
            "check plugin initialization input",
        ));
    }
    Ok(input)
}

#[cfg(feature = "init_v2")]
fn initialize_export<S, T, R, D>(
    exports: &Exports<S>,
    runtime: *const ObjectRefV1,
    terminal: *const ObjectRefV1,
    config: *const ObjectRefV1,
    data: *const ObjectRefV1,
    initialize: impl FnOnce(Inputs<T, R, D>, &mut Option<S>) -> Result<()>,
    take_runtime: impl FnOnce(&Connection, Option<&ObjectRefV1>) -> Result<Option<Runtime<R>>>,
) -> InitReplyV1
where
    S: Send + 'static,
    T: Send + Sync + 'static,
    D: Send + Sync + 'static,
{
    let mut progress = INIT_NOT_STARTED;
    let outcome =
        diagnostic::protect_outcome(call_context("initialize plugin export", None), || {
            let inputs = (|| {
                // 安全前提（safety）。这里只借用由遵约宿主明确保活的原始同步参数。
                let config = unsafe { protected_input(config) }?;
                let terminal = unsafe { protected_input(terminal) }?;
                let data = unsafe { protected_input(data) }?;
                let runtime = if runtime.is_null() {
                    None
                } else {
                    Some(unsafe { protected_input(runtime) }?)
                };
                let id = &config.lease.initialization;
                if !same_id(id, &terminal.lease.initialization)
                    || !same_id(id, &data.lease.initialization)
                    || runtime.is_some_and(|runtime| !same_id(id, &runtime.lease.initialization))
                {
                    return Err(plugin_error(
                        ErrorKind::Conflict,
                        "match plugin input round",
                    ));
                }
                Ok((config, terminal, runtime, data))
            })();
            let (config, terminal, runtime, data) = match inputs {
                Ok(inputs) => inputs,
                Err(error) => {
                    return diagnostic::limited_outcome(
                        *error.current_context(),
                        ErrorKind::InvalidState,
                    )
                }
            };
            run_initialization(
                exports,
                config,
                terminal,
                runtime,
                data,
                initialize,
                take_runtime,
                &mut progress,
            )
        });
    InitReplyV1 { progress, outcome }
}

enum InitAdmission<S> {
    Start(Option<PluginRound<S>>),
    Replay {
        progress: u32,
        failure: Option<Arc<ManagementFailure>>,
        source: Option<Arc<Registry>>,
    },
}

fn admit_initialization<S: Send + 'static>(
    exports: &Exports<S>,
    id: &InitIdV1,
    bound: BoundInputs,
    fallback: &Arc<ManagementFailure>,
) -> std::result::Result<InitAdmission<S>, ErrorKind> {
    let mut state = exports.state.lock();
    if state.closed.iter().any(|closed| same_id(closed, id)) {
        return Err(ErrorKind::InvalidState);
    }
    if let Some(current) = &state.current {
        if same_id(&current.id, id) {
            if !same_inputs(&current.bound, &bound) {
                return Err(ErrorKind::Conflict);
            }
            let progress = match current.phase {
                PluginPhase::Initializing | PluginPhase::Closing => return Err(ErrorKind::Busy),
                PluginPhase::Initialized => INIT_COMPLETED,
                PluginPhase::Rejected => INIT_NOT_STARTED,
                PluginPhase::NeedsClose | PluginPhase::CleanupFailed => INIT_NEEDS_CLOSE,
                PluginPhase::Closed => return Err(ErrorKind::InvalidState),
            };
            return Ok(InitAdmission::Replay {
                progress,
                failure: current.init_failure.clone(),
                source: current.source.clone(),
            });
        }
        if !matches!(current.phase, PluginPhase::Closed | PluginPhase::Rejected) {
            return Err(ErrorKind::Busy);
        }
        let finished = copy_id(&current.id);
        state
            .closed
            .try_reserve(1)
            .map_err(|_| ErrorKind::ResourceExhausted)?;
        state.closed.push(finished);
    }
    let previous = state.current.replace(PluginRound {
        id: copy_id(id),
        bound,
        phase: PluginPhase::Initializing,
        state: None,
        source: None,
        init_failure: Some(Arc::clone(fallback)),
        close_failure: None,
    });
    Ok(InitAdmission::Start(previous))
}

fn limited(kind: ErrorKind) -> OutcomeV1 {
    diagnostic::limited_outcome(kind, ErrorKind::InvalidState)
}

fn publish_result(source: Option<&Registry>, failure: Option<&ManagementFailure>) -> OutcomeV1 {
    match (source, failure) {
        (_, None) => diagnostic::success_outcome(),
        (Some(source), Some(failure)) => source.publish_management_failure(failure),
        (None, Some(failure)) => failure.without_source(),
    }
}

fn capture_report(error: Error, id: &InitIdV1) -> (Arc<ManagementFailure>, bool) {
    let kind = *error.current_context();
    let captured = Arc::new(ManagementFailure::capture(&error));
    let released = dispose_report(error, id);
    match released {
        Ok(()) => (captured, false),
        Err(cleanup) => {
            let category = if kind == ErrorKind::Busy {
                *cleanup.current_context()
            } else {
                kind
            };
            let combined = plugin_error(category, "dispose plugin operation report")
                .attach(format!("原操作分类（original operation kind） {kind:?}"))
                .attach(pi_result::debug_message(&cleanup));
            (Arc::new(ManagementFailure::capture(&combined)), true)
        }
    }
}

fn capture_close_report(error: Error, id: &InitIdV1) -> (Arc<ManagementFailure>, bool) {
    let captured = ManagementFailure::capture(&error);
    let disposal = dispose_report(error, id).err().map(|cleanup| {
        let kind = *cleanup.current_context();
        (kind, pi_result::debug_message(&cleanup))
    });
    let (finished, unconfirmed) = captured.finish_report_cleanup(disposal);
    (Arc::new(finished), unconfirmed)
}

fn dispose_report(error: Error, id: &InitIdV1) -> Result<()> {
    boundary::catch(
        call_context("dispose plugin operation report", Some(id)),
        || {
            drop(error);
            Ok(())
        },
    )
}

struct ExportAttempt<'plugin, S: Send + 'static> {
    exports: &'plugin Exports<S>,
    id: InitIdV1,
    state: Option<S>,
    entered: bool,
    finished: bool,
}

impl<S: Send + 'static> Drop for ExportAttempt<'_, S> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let detached = {
            let mut state = self.exports.state.lock();
            let Some(round) = state
                .current
                .as_mut()
                .filter(|round| same_id(&round.id, &self.id))
            else {
                return;
            };
            if round.state.is_none() {
                round.state = self.state.take();
            }
            match round.phase {
                PluginPhase::Initializing if !self.entered => {
                    round.phase = PluginPhase::Rejected;
                    round.source.take()
                }
                PluginPhase::Initializing => {
                    round.phase = PluginPhase::NeedsClose;
                    None
                }
                PluginPhase::Closing => {
                    round.phase = PluginPhase::CleanupFailed;
                    None
                }
                _ => None,
            }
        };
        drop(detached);
    }
}

#[cfg(feature = "init_v2")]
fn run_initialization<S, T, R, D>(
    exports: &Exports<S>,
    config: &ObjectRefV1,
    terminal: &ObjectRefV1,
    runtime: Option<&ObjectRefV1>,
    data: &ObjectRefV1,
    initialize: impl FnOnce(Inputs<T, R, D>, &mut Option<S>) -> Result<()>,
    take_runtime: impl FnOnce(&Connection, Option<&ObjectRefV1>) -> Result<Option<Runtime<R>>>,
    progress: &mut u32,
) -> OutcomeV1
where
    S: Send + 'static,
    T: Send + Sync + 'static,
    D: Send + Sync + 'static,
{
    let id = &config.lease.initialization;
    let fallback = Arc::new(ManagementFailure::capture(&plugin_error(
        ErrorKind::Internal,
        "plugin initialization interrupted",
    )));
    let bound = BoundInputs::V2 {
        config: copy_lease(&config.lease),
        terminal: copy_lease(&terminal.lease),
        runtime: runtime.map(|runtime| copy_lease(&runtime.lease)),
        data: copy_lease(&data.lease),
    };
    match admit_initialization(exports, id, bound, &fallback) {
        Ok(InitAdmission::Start(previous)) => drop(previous),
        Ok(InitAdmission::Replay {
            progress: known,
            failure,
            source,
        }) => {
            *progress = known;
            return publish_result(source.as_deref(), failure.as_deref());
        }
        Err(kind) => return limited(kind),
    }
    let mut attempt = ExportAttempt {
        exports,
        id: copy_id(id),
        state: None,
        entered: false,
        finished: false,
    };
    let mut source = None;
    let mut received_data = None;
    let initialized = boundary::catch(call_context("run plugin initialization", Some(id)), || {
        // 安全前提（safety）。config 是宿主仍保活的原始同轮同步输入。
        let config = unsafe { ConfigInput::from_input(config) }?;
        let registry = Arc::new(Registry::attach(config.connection().clone())?);
        {
            let mut state = exports.state.lock();
            let round = state
                .current
                .as_mut()
                .filter(|round| same_id(&round.id, id))
                .ok_or_else(|| {
                    plugin_error(ErrorKind::Corrupted, "save plugin diagnostic source")
                })?;
            round.source = Some(Arc::clone(&registry));
        }
        source = Some(registry);
        // 安全前提（safety）。已由配置输入构造同轮连接，原终端仍由宿主同步保活。
        let terminal = unsafe { Terminal::from_input(config.connection(), terminal) }?;
        let runtime = take_runtime(config.connection(), runtime)?;
        // SAFETY: 第四输入来自同一受保护调用，真实类型在消费前预核。
        let data = unsafe { DataInput::<D>::from_input(config.connection(), data) }?;
        attempt.entered = true;
        *progress = INIT_NEEDS_CLOSE;
        data.take_into(&mut received_data)?;
        let config = config.take()?;
        let data = received_data.take().ok_or_else(|| plugin_error(ErrorKind::Corrupted, "receive initialization data"))?;
        initialize(
            Inputs {
                terminal,
                config,
                runtime,
                data,
            },
            &mut attempt.state,
        )
    });

    let data_cleanup = boundary::catch(call_context("dispose undelivered initialization data", Some(id)), || {
        // 先置空再析构（Drop）；即使析构失败也不盲目重跑。
        drop(received_data.take());
        Ok(())
    });
    if let Err(error) = data_cleanup {
        *progress = INIT_NEEDS_CLOSE;
        {
            let mut state = exports.state.lock();
            if let Some(record) = state.current.as_mut().filter(|record| same_id(&record.id, id)) {
                record.phase = PluginPhase::CleanupFailed;
                record.close_failure = Some(Arc::clone(&fallback));
            }
        }
        // 固定失败责任之后才采集详细诊断（diagnostic），其间展开仍保留原失败。
        let (failure, _) = capture_close_report(error, id);
        let previous = {
            let mut state = exports.state.lock();
            state.current.as_mut().filter(|record| same_id(&record.id, id))
                .and_then(|record| record.close_failure.replace(failure))
        };
        drop(previous);
    }

    let (failure, unconfirmed) = match initialized {
        Ok(()) => (None, false),
        Err(error) => {
            let (snapshot, unconfirmed) = capture_report(error, id);
            (Some(snapshot), unconfirmed)
        }
    };
    let phase = if unconfirmed {
        *progress = INIT_NEEDS_CLOSE;
        PluginPhase::CleanupFailed
    } else if failure.is_none() {
        *progress = INIT_COMPLETED;
        PluginPhase::Initialized
    } else if attempt.entered {
        PluginPhase::NeedsClose
    } else {
        PluginPhase::Rejected
    };
    let old = {
        let mut state = exports.state.lock();
        let Some(record) = state
            .current
            .as_mut()
            .filter(|round| same_id(&round.id, id))
        else {
            return limited(ErrorKind::Corrupted);
        };
        record.state = attempt.state.take();
        let detached = if matches!(phase, PluginPhase::Rejected) {
            record.source.take()
        } else {
            None
        };
        if matches!(record.phase, PluginPhase::CleanupFailed) {
            *progress = INIT_NEEDS_CLOSE;
        } else {
            if unconfirmed {
                record.close_failure = failure.clone();
            }
            record.phase = phase;
        }
        (
            std::mem::replace(&mut record.init_failure, failure.clone()),
            detached,
        )
    };
    attempt.finished = true;
    let outcome = publish_result(source.as_deref(), failure.as_deref());
    drop(old);
    outcome
}

enum CloseAdmission<S> {
    Start {
        state: Option<S>,
        source: Option<Arc<Registry>>,
    },
    Replay {
        failure: Option<Arc<ManagementFailure>>,
        source: Option<Arc<Registry>>,
    },
}

fn admit_close<S: Send + 'static>(
    exports: &Exports<S>,
    id: &InitIdV1,
    fallback: &Arc<ManagementFailure>,
) -> std::result::Result<CloseAdmission<S>, ErrorKind> {
    let mut state = exports.state.lock();
    if state.closed.iter().any(|closed| same_id(closed, id)) {
        return Ok(CloseAdmission::Replay {
            failure: None,
            source: None,
        });
    }
    let current = state
        .current
        .as_mut()
        .filter(|round| same_id(&round.id, id))
        .ok_or(ErrorKind::InvalidState)?;
    match current.phase {
        PluginPhase::Initializing | PluginPhase::Closing => Err(ErrorKind::Busy),
        PluginPhase::Rejected | PluginPhase::Closed | PluginPhase::CleanupFailed => {
            Ok(CloseAdmission::Replay {
                failure: current.close_failure.clone(),
                source: current.source.clone(),
            })
        }
        PluginPhase::Initialized | PluginPhase::NeedsClose => {
            if current.close_failure.is_some() {
                return Err(ErrorKind::Corrupted);
            }
            current.close_failure = Some(Arc::clone(fallback));
            current.phase = PluginPhase::Closing;
            Ok(CloseAdmission::Start {
                state: current.state.take(),
                source: current.source.clone(),
            })
        }
    }
}

fn close_export<S: Send + 'static>(
    exports: &Exports<S>,
    id: &InitIdV1,
    close: impl FnOnce(&mut Option<S>) -> Result<()>,
) -> OutcomeV1 {
    if id.module_epoch == 0 || id.round == 0 {
        return limited(ErrorKind::InvalidInput);
    }
    let fallback = Arc::new(ManagementFailure::capture(&plugin_error(
        ErrorKind::Internal,
        "plugin close interrupted",
    )));
    let (state, source) = match admit_close(exports, id, &fallback) {
        Ok(CloseAdmission::Start { state, source }) => (state, source),
        Ok(CloseAdmission::Replay { failure, source }) => {
            return publish_result(source.as_deref(), failure.as_deref());
        }
        Err(kind) => return limited(kind),
    };
    let mut attempt = ExportAttempt {
        exports,
        id: copy_id(id),
        state,
        entered: true,
        finished: false,
    };
    let closed = boundary::catch(call_context("run plugin close", Some(id)), || {
        close(&mut attempt.state)
    });
    let (phase, failure) = match closed {
        Ok(()) => match boundary::catch(
            call_context("dispose closed plugin state", Some(id)),
            || {
                drop(attempt.state.take());
                Ok(())
            },
        ) {
            Ok(()) => (PluginPhase::Closed, None),
            Err(error) => (
                PluginPhase::CleanupFailed,
                Some(capture_close_report(error, id).0),
            ),
        },
        Err(error) => {
            let pending = *error.current_context() == ErrorKind::Busy;
            let (failure, unconfirmed) = capture_close_report(error, id);
            let phase = if pending && !unconfirmed {
                PluginPhase::NeedsClose
            } else {
                PluginPhase::CleanupFailed
            };
            (phase, Some(failure))
        }
    };

    let retained_failure = if matches!(phase, PluginPhase::CleanupFailed) {
        failure.clone()
    } else {
        None
    };
    let old = {
        let mut state = exports.state.lock();
        let Some(record) = state
            .current
            .as_mut()
            .filter(|round| same_id(&round.id, id))
        else {
            return limited(ErrorKind::Corrupted);
        };
        record.state = attempt.state.take();
        let detached = if matches!(phase, PluginPhase::Closed) {
            record.source.take()
        } else {
            None
        };
        record.phase = phase;
        (
            std::mem::replace(&mut record.close_failure, retained_failure),
            detached,
        )
    };
    attempt.finished = true;
    let outcome = publish_result(source.as_deref(), failure.as_deref());
    drop(old);
    outcome
}


#[cfg(feature = "init_v1")]
pub(super) unsafe fn export_initialize_v1<S, T, R>(
    exports: &Exports<S>,
    runtime: *const ObjectRefV1,
    terminal: *const ObjectRefV1,
    config: *const ObjectRefV1,
    initialize: impl FnOnce(InputsV1<T, R>, &mut Option<S>) -> Result<()>,
) -> InitReplyV1
where
    S: Send + 'static,
    T: Send + Sync + 'static,
    R: IdleDispatch + RuntimeWait + RuntimeQuery + Send + Sync + 'static,
{
    initialize_export_v1(exports, runtime, terminal, config, initialize, |connection, runtime| {
        // SAFETY: 原输入由同一同步初始化（initialization）调用保护，连接已核同轮。
        unsafe { Runtime::<R>::from_optional_input(connection, runtime) }
    })
}

#[cfg(feature = "init_v1")]
pub(super) unsafe fn export_initialize_v1_without_runtime<
    S: Send + 'static,
    T: Send + Sync + 'static,
>(
    exports: &Exports<S>,
    runtime: *const ObjectRefV1,
    terminal: *const ObjectRefV1,
    config: *const ObjectRefV1,
    initialize: impl FnOnce(InputsV1<T>, &mut Option<S>) -> Result<()>,
) -> InitReplyV1 {
    if !runtime.is_null() {
        let outcome = diagnostic::protect_outcome(call_context("reject runtime for plugin export", None), move || {
            drop(initialize);
            limited(ErrorKind::InvalidInput)
        });
        return InitReplyV1 { progress: INIT_NOT_STARTED, outcome };
    }
    initialize_export_v1(exports, runtime, terminal, config, initialize, |_, _| Ok(None))
}

#[cfg(feature = "init_v1")]
fn initialize_export_v1<S, T, R>(
    exports: &Exports<S>,
    runtime: *const ObjectRefV1,
    terminal: *const ObjectRefV1,
    config: *const ObjectRefV1,
    initialize: impl FnOnce(InputsV1<T, R>, &mut Option<S>) -> Result<()>,
    take_runtime: impl FnOnce(&Connection, Option<&ObjectRefV1>) -> Result<Option<Runtime<R>>>,
) -> InitReplyV1
where
    S: Send + 'static,
    T: Send + Sync + 'static,
{
    let mut progress = INIT_NOT_STARTED;
    let outcome =
        diagnostic::protect_outcome(call_context("initialize plugin export", None), || {
            let inputs = (|| {
                // 安全前提（safety）。这里只借用由遵约宿主明确保活的原始同步参数。
                let config = unsafe { protected_input(config) }?;
                let terminal = unsafe { protected_input(terminal) }?;
                let runtime = if runtime.is_null() {
                    None
                } else {
                    Some(unsafe { protected_input(runtime) }?)
                };
                let id = &config.lease.initialization;
                if !same_id(id, &terminal.lease.initialization)
                    || runtime.is_some_and(|runtime| !same_id(id, &runtime.lease.initialization))
                {
                    return Err(plugin_error(
                        ErrorKind::Conflict,
                        "match plugin input round",
                    ));
                }
                Ok((config, terminal, runtime))
            })();
            let (config, terminal, runtime) = match inputs {
                Ok(inputs) => inputs,
                Err(error) => {
                    return diagnostic::limited_outcome(
                        *error.current_context(),
                        ErrorKind::InvalidState,
                    )
                }
            };
            run_initialization_v1(
                exports,
                config,
                terminal,
                runtime,
                initialize,
                take_runtime,
                &mut progress,
            )
        });
    InitReplyV1 { progress, outcome }
}

#[cfg(feature = "init_v1")]
fn run_initialization_v1<S, T, R>(
    exports: &Exports<S>,
    config: &ObjectRefV1,
    terminal: &ObjectRefV1,
    runtime: Option<&ObjectRefV1>,
    initialize: impl FnOnce(InputsV1<T, R>, &mut Option<S>) -> Result<()>,
    take_runtime: impl FnOnce(&Connection, Option<&ObjectRefV1>) -> Result<Option<Runtime<R>>>,
    progress: &mut u32,
) -> OutcomeV1
where
    S: Send + 'static,
    T: Send + Sync + 'static,
{
    let id = &config.lease.initialization;
    let fallback = Arc::new(ManagementFailure::capture(&plugin_error(
        ErrorKind::Internal,
        "plugin initialization interrupted",
    )));
    let bound = BoundInputs::V1 {
        config: copy_lease(&config.lease),
        terminal: copy_lease(&terminal.lease),
        runtime: runtime.map(|runtime| copy_lease(&runtime.lease)),
    };
    match admit_initialization(exports, id, bound, &fallback) {
        Ok(InitAdmission::Start(previous)) => drop(previous),
        Ok(InitAdmission::Replay {
            progress: known,
            failure,
            source,
        }) => {
            *progress = known;
            return publish_result(source.as_deref(), failure.as_deref());
        }
        Err(kind) => return limited(kind),
    }
    let mut attempt = ExportAttempt {
        exports,
        id: copy_id(id),
        state: None,
        entered: false,
        finished: false,
    };
    let mut source = None;
    let initialized = boundary::catch(call_context("run plugin initialization", Some(id)), || {
        // 安全前提（safety）。config 是宿主仍保活的原始同轮同步输入。
        let config = unsafe { ConfigInput::from_input(config) }?;
        let registry = Arc::new(Registry::attach(config.connection().clone())?);
        {
            let mut state = exports.state.lock();
            let round = state
                .current
                .as_mut()
                .filter(|round| same_id(&round.id, id))
                .ok_or_else(|| {
                    plugin_error(ErrorKind::Corrupted, "save plugin diagnostic source")
                })?;
            round.source = Some(Arc::clone(&registry));
        }
        source = Some(registry);
        // 安全前提（safety）。已由配置输入构造同轮连接，原终端仍由宿主同步保活。
        let terminal = unsafe { Terminal::from_input(config.connection(), terminal) }?;
        let runtime = take_runtime(config.connection(), runtime)?;
        attempt.entered = true;
        *progress = INIT_NEEDS_CLOSE;
        let config = config.take()?;
        initialize(
            InputsV1 {
                terminal,
                config,
                runtime,
            },
            &mut attempt.state,
        )
    });

    let (failure, unconfirmed) = match initialized {
        Ok(()) => (None, false),
        Err(error) => {
            let (snapshot, unconfirmed) = capture_report(error, id);
            (Some(snapshot), unconfirmed)
        }
    };
    let phase = if unconfirmed {
        *progress = INIT_NEEDS_CLOSE;
        PluginPhase::CleanupFailed
    } else if failure.is_none() {
        *progress = INIT_COMPLETED;
        PluginPhase::Initialized
    } else if attempt.entered {
        PluginPhase::NeedsClose
    } else {
        PluginPhase::Rejected
    };
    let old = {
        let mut state = exports.state.lock();
        let Some(record) = state
            .current
            .as_mut()
            .filter(|round| same_id(&round.id, id))
        else {
            return limited(ErrorKind::Corrupted);
        };
        record.state = attempt.state.take();
        let detached = if matches!(phase, PluginPhase::Rejected) {
            record.source.take()
        } else {
            None
        };
        if matches!(record.phase, PluginPhase::CleanupFailed) {
            *progress = INIT_NEEDS_CLOSE;
        } else {
            if unconfirmed {
                record.close_failure = failure.clone();
            }
            record.phase = phase;
        }
        (
            std::mem::replace(&mut record.init_failure, failure.clone()),
            detached,
        )
    };
    attempt.finished = true;
    let outcome = publish_result(source.as_deref(), failure.as_deref());
    drop(old);
    outcome
}

#[cfg(all(test, feature = "init_v1", feature = "init_v2"))]
mod compatibility_tests {
    use super::*;
    use std::cell::Cell;

    // 仅向真实准入函数提供已解析绑定（binding），不构造或调用虚假的终端输入。
    fn lease(slot: u64, round: u64) -> LeaseV1 {
        LeaseV1 {
            object: pi_ffi_abi::ObjectKeyV1 { source: 1, slot, generation: 1 },
            initialization: InitIdV1 { module_epoch: 7, round },
            holding: slot,
        }
    }

    fn bound(round: u64, v2: bool) -> BoundInputs {
        if v2 {
            BoundInputs::V2 { config: lease(1, round), terminal: lease(2, round), runtime: None, data: lease(3, round) }
        } else {
            BoundInputs::V1 { config: lease(1, round), terminal: lease(2, round), runtime: None }
        }
    }

    /// 同一轮次（round）的准确版本及完整租约只接纳一次，关闭结果稳定重放。
    #[test]
    fn compatibility_shares_admission_replay_busy_conflict_and_close() {
        let exports = Exports::<()>::new(Declaration { uuid: [1; 16], version: "1.0.0" });
        let id = InitIdV1 { module_epoch: 7, round: 1 };
        let fallback = Arc::new(ManagementFailure::capture(&plugin_error(ErrorKind::Internal, "test interrupted initialization")));
        assert!(matches!(admit_initialization(&exports, &id, bound(1, false), &fallback), Ok(InitAdmission::Start(None))));
        assert_eq!(admit_initialization(&exports, &id, bound(1, false), &fallback).err(), Some(ErrorKind::Busy));
        assert_eq!(admit_initialization(&exports, &id, bound(1, true), &fallback).err(), Some(ErrorKind::Conflict));
        let mut changed = bound(1, false);
        if let BoundInputs::V1 { terminal, .. } = &mut changed { terminal.holding += 1; }
        assert_eq!(admit_initialization(&exports, &id, changed, &fallback).err(), Some(ErrorKind::Conflict));
        {
            let mut state = exports.state.lock();
            let current = state.current.as_mut().unwrap();
            current.phase = PluginPhase::Initialized;
            current.init_failure = None;
        }
        assert!(matches!(admit_initialization(&exports, &id, bound(1, false), &fallback), Ok(InitAdmission::Replay { progress: INIT_COMPLETED, failure: None, .. })));
        let calls = Cell::new(0);
        assert_eq!(close_export(&exports, &id, |_| { calls.set(calls.get() + 1); Ok(()) }).kind, 0);
        assert_eq!(close_export(&exports, &id, |_| { calls.set(calls.get() + 1); Ok(()) }).kind, 0);
        assert_eq!(calls.get(), 1);
        assert_eq!(admit_initialization(&exports, &id, bound(1, false), &fallback).err(), Some(ErrorKind::InvalidState));
        assert_eq!(admit_initialization(&exports, &id, bound(1, true), &fallback).err(), Some(ErrorKind::Conflict));
        let next = InitIdV1 { module_epoch: 7, round: 2 };
        assert!(matches!(admit_initialization(&exports, &next, bound(2, true), &fallback), Ok(InitAdmission::Start(Some(_)))));
        assert_eq!(admit_initialization(&exports, &id, bound(1, false), &fallback).err(), Some(ErrorKind::InvalidState));
    }

    /// 首次可能消费后仍处于初始化（initialization）阶段；中断沿原清理责任收口。
    #[test]
    fn compatibility_entered_attempt_keeps_initializing_and_cleanup_failure() {
        let exports = Exports::<()>::new(Declaration { uuid: [1; 16], version: "1.0.0" });
        let id = InitIdV1 { module_epoch: 7, round: 20 };
        let fallback = Arc::new(ManagementFailure::capture(&plugin_error(ErrorKind::Internal, "test entered attempt")));
        assert!(matches!(admit_initialization(&exports, &id, bound(20, false), &fallback), Ok(InitAdmission::Start(_))));
        let attempt = ExportAttempt { exports: &exports, id: copy_id(&id), state: None, entered: true, finished: false };
        let called = Cell::new(false);
        let reply = close_export(&exports, &id, |_| { called.set(true); Ok(()) });
        assert_eq!(reply.kind, diagnostic::status_code(ErrorKind::Busy));
        assert!(!called.get());
        assert!(matches!(exports.state.lock().current.as_ref().unwrap().phase, PluginPhase::Initializing));
        drop(attempt);
        assert!(matches!(exports.state.lock().current.as_ref().unwrap().phase, PluginPhase::NeedsClose));
        {
            let mut state = exports.state.lock();
            let current = state.current.as_mut().unwrap();
            current.phase = PluginPhase::CleanupFailed;
            current.close_failure = Some(fallback.clone());
        }
        drop(ExportAttempt { exports: &exports, id: copy_id(&id), state: None, entered: true, finished: false });
        assert!(matches!(exports.state.lock().current.as_ref().unwrap().phase, PluginPhase::CleanupFailed));
        let reply = close_export(&exports, &id, |_| { called.set(true); Ok(()) });
        assert_ne!(reply.kind, 0);
        assert!(!called.get());
    }

    /// 失败清理（cleanup）或在途关闭不能靠新版本、新标识绕过。
    #[test]
    fn compatibility_new_round_obeys_original_cleanup_gates() {
        for phase in [PluginPhase::NeedsClose, PluginPhase::Closing, PluginPhase::CleanupFailed] {
            let exports = Exports::<()>::new(Declaration { uuid: [1; 16], version: "1.0.0" });
            let id = InitIdV1 { module_epoch: 7, round: 10 };
            let fallback = Arc::new(ManagementFailure::capture(&plugin_error(ErrorKind::Internal, "test cleanup failure")));
            assert!(matches!(admit_initialization(&exports, &id, bound(10, true), &fallback), Ok(InitAdmission::Start(_))));
            {
                let mut state = exports.state.lock();
                let current = state.current.as_mut().unwrap();
                current.phase = phase;
                current.close_failure = Some(fallback.clone());
            }
            let next = InitIdV1 { module_epoch: 7, round: 11 };
            assert_eq!(admit_initialization(&exports, &next, bound(11, false), &fallback).err(), Some(ErrorKind::Busy));
            assert_eq!(admit_initialization(&exports, &id, bound(10, false), &fallback).err(), Some(ErrorKind::Conflict));
        }
    }
}
