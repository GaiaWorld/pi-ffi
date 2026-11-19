// 本地实现入口（local implementation entry）；关键类型由父模块声明。
use super::*;
use pi_ffi_abi::{ApiHeaderV1, DiagnosticApiV1, DiagnosticInfoV1, InitIdV1, LeaseV1, ObjectKeyV1};
use std::panic::{catch_unwind, AssertUnwindSafe};

pub(super) fn catch<T>(context: Context, body: impl FnOnce() -> Result<T>) -> Result<T> {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(result) => result,
        Err(payload) => {
            let detail = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&'static str>().copied())
                .unwrap_or("未知恐慌负载（panic payload）")
                .to_owned();
            let mut error = Error::new(ErrorKind::Internal)
                .attach(pi_result::attachment::Operation(context.operation))
                .attach(format!(
                    "模块代际（module epoch）{}，初始化轮次（initialization round）{}，来源（source）{}",
                    context.module_epoch, context.round, context.source
                ))
                .attach(detail);
            if !dispose_payload(payload) {
                error = error
                    .attach("恐慌负载（panic payload）的析构（drop）结果不明")
                    .attach_opaque(UnconfirmedPanicCleanup);
            }
            Err(error)
        }
    }
}

pub(super) fn catch_outcome(context: Context, body: impl FnOnce() -> OutcomeV1) -> OutcomeV1 {
    let _ = context;
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(outcome) => outcome,
        Err(payload) => {
            dispose_payload(payload);
            OutcomeV1 {
                kind: encode_kind(ErrorKind::Internal),
                diagnostic_failure_kind: encode_kind(ErrorKind::Internal),
                diagnostic: empty_diagnostic(),
            }
        }
    }
}

pub(super) fn catch_status(context: Context, body: impl FnOnce() -> u32) -> u32 {
    let _ = context;
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(status) => status,
        Err(payload) => {
            dispose_payload(payload);
            encode_kind(ErrorKind::Internal)
        }
    }
}

fn dispose_payload(payload: Box<dyn std::any::Any + Send>) -> bool {
    if let Err(cleanup_payload) = catch_unwind(AssertUnwindSafe(|| drop(payload))) {
        // 二次恐慌（panic）的负载（payload）也可能执行用户析构（drop）。
        // 保留结果不明的责任，避免在退出 C 边界（C boundary）时继续析构它。
        std::mem::forget(cleanup_payload);
        return false;
    }
    true
}

pub(super) fn copied_diagnostic_fmt(
    receiver: &CopiedDiagnostic,
    f: &mut core::fmt::Formatter<'_>,
) -> core::fmt::Result {
    write!(
        f,
        "user: {}\nverbose: {}\ndebug: {}",
        receiver.user, receiver.verbose, receiver.debug
    )
}

pub(super) fn copied_context_fmt(
    receiver: &CopiedContext,
    f: &mut core::fmt::Formatter<'_>,
) -> core::fmt::Result {
    f.write_str(&receiver.diagnostic.user)
}

pub(super) fn copied_diagnostic_into_error(receiver: CopiedDiagnostic) -> Error {
    let kind = receiver.kind;
    let diagnostic = Arc::new(receiver);
    pi_result::ErrorReport::new(CopiedContext {
        diagnostic: Arc::clone(&diagnostic),
    })
    .change_context(kind)
    .attach(diagnostic)
}

pub(super) fn encode_kind(kind: ErrorKind) -> u32 {
    use ErrorKind::*;
    match kind {
        InvalidInput => 1,
        InvalidState => 2,
        NotFound => 3,
        AlreadyExists => 4,
        Conflict => 5,
        PermissionDenied => 6,
        Io => 7,
        Timeout => 8,
        Cancelled => 9,
        Busy => 10,
        Unavailable => 11,
        Unsupported => 12,
        ResourceExhausted => 13,
        Corrupted => 14,
        Dependency => 15,
        Internal => 16,
        _ => 17,
    }
}

pub(super) fn decode_kind(code: u32) -> ErrorKind {
    use ErrorKind::*;
    match code {
        1 => InvalidInput,
        2 => InvalidState,
        3 => NotFound,
        4 => AlreadyExists,
        5 => Conflict,
        6 => PermissionDenied,
        7 => Io,
        8 => Timeout,
        9 => Cancelled,
        10 => Busy,
        11 => Unavailable,
        12 => Unsupported,
        13 => ResourceExhausted,
        14 => Corrupted,
        15 => Dependency,
        16 => Internal,
        _ => Other,
    }
}

pub(super) fn diagnostic_kind_mismatch_fmt(
    receiver: &DiagnosticKindMismatch,
    f: &mut core::fmt::Formatter<'_>,
) -> core::fmt::Result {
    write!(
        f,
        "外层操作分类（operation kind）{}与来源诊断分类（diagnostic kind）{}不符",
        receiver.expected, receiver.reported
    )
}

pub(super) unsafe fn read_diagnostic(
    diagnostic: &DiagnosticRefV1,
    expected_kind: u32,
) -> Result<CopiedDiagnostic> {
    // SAFETY: 调用方持有来源的代码及上下文（context），只读表（read-only table）全程有效。
    let (info, copy, _) = unsafe { diagnostic_entries(diagnostic.api)? };
    let mut lengths = DiagnosticInfoV1 {
        kind: 0,
        user_len: 0,
        verbose_len: 0,
        debug_len: 0,
    };
    // SAFETY: 输出槽（output slot）是本地独占可写内存，诊断持有（diagnostic holding）由调用方保护。
    let status = unsafe {
        info(
            diagnostic.context,
            copy_lease(&diagnostic.lease),
            &mut lengths,
        )
    };
    check_status(status, "diagnostic.info")?;
    if lengths.kind != expected_kind {
        let mismatch = DiagnosticKindMismatch {
            expected: expected_kind,
            reported: lengths.kind,
        };
        return Err(Error::new(ErrorKind::Corrupted).attach(mismatch));
    }

    let total = lengths
        .user_len
        .checked_add(lengths.verbose_len)
        .and_then(|length| length.checked_add(lengths.debug_len))
        .and_then(|length| usize::try_from(length).ok())
        .filter(|&length| length <= isize::MAX as usize)
        .ok_or_else(|| {
            Error::new(ErrorKind::Corrupted).attach("诊断长度（diagnostic length）越界")
        })?;
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(total)
        .map_err(|error| Error::new(ErrorKind::ResourceExhausted).attach(error.to_string()))?;
    buffer.resize(total, 0);
    let mut written = 0;
    // SAFETY: 缓冲区（buffer）至少包含 total 个独占可写字节。长度为零时协议允许空地址。
    let status = unsafe {
        copy(
            diagnostic.context,
            copy_lease(&diagnostic.lease),
            if total == 0 {
                std::ptr::null_mut()
            } else {
                buffer.as_mut_ptr()
            },
            total as u64,
            &mut written,
        )
    };
    check_status(status, "diagnostic.copy")?;
    if written != total as u64 {
        return Err(Error::new(ErrorKind::Corrupted).attach("诊断复制（diagnostic copy）长度不符"));
    }

    let (user, rest) = buffer.split_at(lengths.user_len as usize);
    let (verbose, debug) = rest.split_at(lengths.verbose_len as usize);
    Ok(CopiedDiagnostic {
        kind: decode_kind(lengths.kind),
        raw_kind: lengths.kind,
        user: copy_text(user)?,
        verbose: copy_text(verbose)?,
        debug: copy_text(debug)?,
    })
}

fn copy_text(bytes: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| Error::new(ErrorKind::Corrupted).attach(error.to_string()))?;
    let mut owned = String::new();
    owned
        .try_reserve_exact(text.len())
        .map_err(|error| Error::new(ErrorKind::ResourceExhausted).attach(error.to_string()))?;
    owned.push_str(text);
    Ok(owned)
}

type DiagnosticInfo =
    unsafe extern "C" fn(*mut core::ffi::c_void, LeaseV1, *mut DiagnosticInfoV1) -> u32;
type DiagnosticCopy =
    unsafe extern "C" fn(*mut core::ffi::c_void, LeaseV1, *mut u8, u64, *mut u64) -> u32;
type DiagnosticRelease = unsafe extern "C" fn(*mut core::ffi::c_void, LeaseV1) -> u32;

unsafe fn diagnostic_entries(
    api: *const DiagnosticApiV1,
) -> Result<(DiagnosticInfo, DiagnosticCopy, DiagnosticRelease)> {
    let invalid =
        || Error::new(ErrorKind::Corrupted).attach("诊断操作表（diagnostic operation table）无效");
    if api.is_null() || !api.is_aligned() {
        return Err(invalid());
    }

    // SAFETY: 已取得诊断来源保护。先读固定表头（API header），尺寸匹配后才读其余字段。
    let header = unsafe { &*api.cast::<ApiHeaderV1>() };
    if header.major != 1 || header.size as usize != size_of::<DiagnosticApiV1>() {
        return Err(invalid());
    }
    // SAFETY: 合法来源保证所声明的只读表（read-only table）在现有保护内有效。
    let api = unsafe { &*api };
    match (api.info, api.copy, api.release) {
        (Some(info), Some(copy), Some(release)) => Ok((info, copy, release)),
        _ => Err(invalid()),
    }
}

pub(super) unsafe fn decode_bootstrap(
    seed: &ObjectRefV1,
    outcome: OutcomeV1,
    read: unsafe fn(&DiagnosticRefV1, u32) -> Result<CopiedDiagnostic>,
) -> Result<()> {
    let OutcomeV1 {
        kind,
        diagnostic_failure_kind,
        diagnostic,
    } = outcome;
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
        (0, 0, true) => return Ok(()),
        (kind, secondary, true) if kind != 0 && secondary != 0 => {
            return Err(Error::new(decode_kind(kind)).attach(format!(
                "引导（bootstrap）操作分类（operation kind）{kind}，诊断建立分类（diagnostic failure kind）{secondary}"
            )));
        }
        (kind, 0, false)
            if kind != 0
                && diagnostic.context == seed.context
                && !diagnostic.context.is_null()
                && !diagnostic.api.is_null()
                && lease.object.source != 0
                && lease.object.source == seed.lease.object.source
                && lease.object.slot != 0
                && lease.object.generation != 0
                && lease.initialization.module_epoch == seed.lease.initialization.module_epoch
                && lease.initialization.round == seed.lease.initialization.round
                && lease.holding != 0 => {}
        _ => return Err(corrupted_outcome(kind, diagnostic_failure_kind)),
    }

    // SAFETY: 诊断与引导输入同来源、上下文和轮次（round），调用方的外层保护覆盖其诊断表。
    let (_, _, release) = unsafe { diagnostic_entries(diagnostic.api) }.map_err(|error| {
        error.attach(format!("引导（bootstrap）操作分类（operation kind）{kind}"))
    })?;
    let context = Context {
        operation: "bootstrap diagnostic",
        module_epoch: lease.initialization.module_epoch,
        round: lease.initialization.round,
        source: lease.object.source,
    };
    // SAFETY: 该保护持续到显式归还（release）之后。
    let copied = catch(context, || unsafe { read(&diagnostic, kind) });
    let released = check_status(
        // SAFETY: 此处唯一接管本次独立诊断持有（diagnostic holding），来源仍在外层保护内。
        unsafe { release(diagnostic.context, copy_lease(lease)) },
        "bootstrap diagnostic.release",
    );

    let error = match copied {
        Ok(copied) => copied.into_error(),
        Err(error) if error.downcast_ref::<DiagnosticKindMismatch>().is_some() => error,
        Err(error) => error
            .change_context(decode_kind(kind))
            .attach(format!("引导（bootstrap）操作分类（operation kind）{kind}")),
    };
    match released {
        Ok(()) => Err(error),
        Err(release_error) => Err(error.attach(format!(
            "诊断归还（diagnostic release）失败 {release_error:?}"
        ))),
    }
}

fn check_status(code: u32, operation: &'static str) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(Error::new(decode_kind(code))
            .attach(pi_result::attachment::Operation(operation))
            .attach(code))
    }
}

fn corrupted_outcome(kind: u32, diagnostic_failure_kind: u32) -> Error {
    Error::new(ErrorKind::Corrupted).attach(format!(
        "结果（outcome）字段冲突，操作分类（operation kind）{kind}，诊断建立分类（diagnostic failure kind）{diagnostic_failure_kind}"
    ))
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

fn copy_lease(lease: &LeaseV1) -> LeaseV1 {
    LeaseV1 {
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
    }
}
