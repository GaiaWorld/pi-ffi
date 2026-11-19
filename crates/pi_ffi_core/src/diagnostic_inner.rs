// 本地实现入口（local implementation entry）；关键类型由父模块声明。
use super::*;
use crate::boundary::{self, Context, DiagnosticKindMismatch};
use pi_ffi_abi::{CallTargetV1, InitIdV1, LeaseV1, ObjectKeyV1, TARGET_DIAGNOSTIC};

pub(super) fn copied_diagnostic(error: &Error) -> Option<&CopiedDiagnostic> {
    error
        .downcast_ref::<Arc<CopiedDiagnostic>>()
        .map(Arc::as_ref)
}

pub(super) fn snapshot(error: &Error) -> core::result::Result<Snapshot, SnapshotFailure> {
    let kind = *error.current_context();
    boundary::catch(
        Context {
            operation: "diagnostic snapshot",
            module_epoch: 0,
            round: 0,
            source: 0,
        },
        || {
            Ok(Snapshot {
                kind,
                user: pi_result::user_message(error),
                verbose: pi_result::verbose_message(error),
                debug: pi_result::debug_message(error),
            })
        },
    )
    .map_err(|diagnostic| SnapshotFailure {
        operation: kind,
        diagnostic: *diagnostic.current_context(),
    })
}

pub(super) fn copy_and_release(
    connection: &Connection,
    diagnostic: DiagnosticRefV1,
    expected_kind: u32,
) -> Result<CopiedDiagnostic> {
    let entered = connection
        .next_operation()
        .and_then(|operation| connection.enter(target(&diagnostic), operation, 1));
    let (copied, call) = match entered {
        Ok(call) => {
            let context = Context {
                operation: "diagnostic copy",
                module_epoch: diagnostic.lease.initialization.module_epoch,
                round: diagnostic.lease.initialization.round,
                source: diagnostic.lease.object.source,
            };
            // SAFETY: 本次来源调用（source call）已实际获准，调用保活尚未归还。
            let copied = boundary::catch(context, || unsafe {
                boundary::read_diagnostic(&diagnostic, expected_kind)
            });
            (copied, Some(call))
        }
        Err(error) => (Err(error), None),
    };

    // 接管的既有释放义务（release obligation）先交宿主，再归还本次调用保活。
    let released = connection.submit_release(target(&diagnostic));
    let finished = call.map_or(Ok(()), |call| call.finish());
    if released == 0 && finished.is_ok() {
        return copied;
    }

    let mut error = match copied {
        Ok(copied) => copied.into_error(),
        Err(error) => error,
    };
    if released != 0 {
        error = error.attach(
            Error::new(boundary::decode_kind(released))
                .attach(pi_result::attachment::Operation(
                    "diagnostic release submission",
                ))
                .attach(released),
        );
    }
    if let Err(failure) = finished {
        error = error.attach(failure);
    }
    Err(error)
}

pub(super) fn decode(connection: &Connection, outcome: OutcomeV1) -> Result<()> {
    let OutcomeV1 {
        kind,
        diagnostic_failure_kind,
        diagnostic,
    } = outcome;
    let lease = &diagnostic.lease;
    let initialization = connection.initialization();
    if kind != 0
        && diagnostic_failure_kind == 0
        && !diagnostic.context.is_null()
        && !diagnostic.api.is_null()
        && lease.object.source != 0
        && lease.object.slot != 0
        && lease.object.generation != 0
        && lease.holding != 0
        && initialization.module_epoch == lease.initialization.module_epoch
        && initialization.round == lease.initialization.round
    {
        let error = match super::copy_and_release(connection, diagnostic, kind) {
            Ok(value) => value.into_error(),
            Err(failure) if failure.downcast_ref::<DiagnosticKindMismatch>().is_some() => failure,
            Err(failure) => failure
                .change_context(boundary::decode_kind(kind))
                .attach(kind),
        };
        Err(error)
    } else {
        decode_without_source(kind, diagnostic_failure_kind, &diagnostic)
    }
}

pub(super) fn capture_management_failure(error: &Error) -> ManagementFailure {
    let cached = match super::snapshot(error) {
        Ok(snapshot) => CachedFailure::Complete(Arc::new(snapshot)),
        Err(failure) => CachedFailure::Unavailable {
            operation: failure.operation,
            diagnostic: failure.diagnostic,
        },
    };
    ManagementFailure { cached }
}

pub(super) fn management_failure_without_source(failure: &ManagementFailure) -> OutcomeV1 {
    match &failure.cached {
        CachedFailure::Complete(snapshot) => {
            super::limited_outcome(snapshot.kind, ErrorKind::InvalidState)
        }
        CachedFailure::Unavailable {
            operation,
            diagnostic,
        } => super::limited_outcome(*operation, *diagnostic),
    }
}

pub(super) fn success_outcome() -> OutcomeV1 {
    OutcomeV1 {
        kind: 0,
        diagnostic_failure_kind: 0,
        diagnostic: empty_diagnostic(),
    }
}

pub(super) fn limited_outcome(operation: ErrorKind, diagnostic: ErrorKind) -> OutcomeV1 {
    OutcomeV1 {
        kind: boundary::encode_kind(operation),
        diagnostic_failure_kind: boundary::encode_kind(diagnostic),
        diagnostic: empty_diagnostic(),
    }
}

pub(super) fn status_code(kind: ErrorKind) -> u32 {
    boundary::encode_kind(kind)
}

pub(super) fn protect_outcome(
    context: crate::boundary::Context,
    body: impl FnOnce() -> OutcomeV1,
) -> OutcomeV1 {
    boundary::catch_outcome(context, body)
}

pub(super) fn protect_status(context: crate::boundary::Context, body: impl FnOnce() -> u32) -> u32 {
    boundary::catch_status(context, body)
}

pub(super) unsafe fn decode_management(
    connection: Option<&Connection>,
    outcome: OutcomeV1,
) -> Result<()> {
    match connection {
        Some(connection) => super::decode(connection, outcome),
        None => decode_without_source(
            outcome.kind,
            outcome.diagnostic_failure_kind,
            &outcome.diagnostic,
        ),
    }
}

fn decode_without_source(
    kind: u32,
    diagnostic_failure_kind: u32,
    diagnostic: &DiagnosticRefV1,
) -> Result<()> {
    let lease = &diagnostic.lease;
    let empty = diagnostic.context.is_null()
        && diagnostic.api.is_null()
        && lease.object.source == 0
        && lease.object.slot == 0
        && lease.object.generation == 0
        && lease.initialization.module_epoch == 0
        && lease.initialization.round == 0
        && lease.holding == 0;
    match (kind, diagnostic_failure_kind, empty) {
        (0, 0, true) => Ok(()),
        (primary, secondary, true) if primary != 0 && secondary != 0 => {
            let error = Error::new(boundary::decode_kind(primary)).attach(primary).attach(
                Error::new(boundary::decode_kind(secondary))
                    .attach(pi_result::attachment::Operation("diagnostic creation"))
                    .attach(secondary),
            );
            Err(error)
        }
        _ => Err(Error::new(ErrorKind::Corrupted).attach(format!(
            "结果（outcome）字段不匹配，操作分类（operation kind）{kind}，诊断建立分类（diagnostic failure kind）{diagnostic_failure_kind}"
        ))),
    }
}

fn empty_diagnostic() -> DiagnosticRefV1 {
    DiagnosticRefV1 {
        context: std::ptr::null_mut(),
        api: std::ptr::null(),
        lease: LeaseV1 {
            object: ObjectKeyV1 {
                source: 0,
                slot: 0,
                generation: 0,
            },
            initialization: InitIdV1 {
                module_epoch: 0,
                round: 0,
            },
            holding: 0,
        },
    }
}

fn target(diagnostic: &DiagnosticRefV1) -> CallTargetV1 {
    let lease = &diagnostic.lease;
    CallTargetV1 {
        context: diagnostic.context,
        api: diagnostic.api.cast(),
        kind: TARGET_DIAGNOSTIC,
        lease: LeaseV1 {
            object: ObjectKeyV1 {
                source: lease.object.source,
                slot: lease.object.slot,
                generation: lease.object.generation,
            },
            initialization: InitIdV1 {
                module_epoch: lease.initialization.module_epoch,
                round: lease.initialization.round,
            },
            holding: lease.holding,
        },
    }
}
