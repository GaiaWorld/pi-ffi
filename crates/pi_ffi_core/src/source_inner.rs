// 本地实现入口（local implementation entry）；关键类型由父模块声明。
use super::*;
use crate::{boundary, diagnostic};
use pi_ffi_abi::{
    CallTargetV1, SourceDescriptorV1, HOST_CONTROL_ID, HOST_CONTROL_MAJOR, TARGET_DIAGNOSTIC,
    TARGET_OBJECT,
};
use pi_result::ErrorKind;
use std::ffi::c_void;

const READ: u64 = 1;
const WORK: u64 = 2;
const RIGHTS: u64 = 7;

pub(super) fn registry_attach(connection: Connection) -> Result<Registry> {
    if !cfg!(panic = "unwind") {
        return Err(error(
            ErrorKind::Unsupported,
            "register source with panic=abort",
        ));
    }
    let operation = connection.next_operation()?;
    let shared = Arc::new(Shared {
        connection,
        context: std::sync::OnceLock::new(),
        state: Mutex::new(State {
            source: 0,
            phase: SourcePhase::Registering,
            retirement_request: RetirementRequest::Unassigned,
            next_slot: 1,
            next_holding: 1,
            entries: Vec::new(),
            operations: Vec::new(),
            failure: None,
        }),
    });
    let pointer = Box::into_raw(Box::new(SourceContext {
        shared: Arc::clone(&shared),
    }));
    assert!(shared.context.set(SourceAddress { pointer }).is_ok());

    let descriptor = SourceDescriptorV1 {
        context: pointer.cast(),
        objects: &SOURCE_API,
        diagnostics: &DIAGNOSTIC_API,
        control: &SOURCE_CONTROL_API,
    };
    let mut source = 0;
    let outcome = shared
        .connection
        .register_raw(operation, descriptor, &mut source);
    let refused = outcome.kind != 0;
    if let Err(failure) = diagnostic::decode(&shared.connection, outcome) {
        if refused && *failure.current_context() != ErrorKind::Corrupted {
            // SAFETY: 宿主已明确拒绝登记，来源上下文（source context）仍由本次准备拥有。
            drop(unsafe { Box::from_raw(pointer) });
        }
        // 矛盾回复（inconsistent reply）可能已经发布该地址；在确认前保留。
        return Err(failure);
    }
    if source == 0 {
        return Err(error(ErrorKind::Corrupted, "registered source identifier"));
    }
    {
        let mut state = shared.state.lock();
        state.source = source;
        state.phase = SourcePhase::Open;
    }
    Ok(Registry { shared })
}

pub(super) fn registry_export<T: Send + Sync + 'static>(
    receiver: &Registry,
    value: T,
    interfaces: Vec<ExportedInterface<T>>,
    close: impl Fn(&T) -> Result<()> + Send + Sync + 'static,
) -> Result<Object> {
    let mut declarations = Vec::new();
    declarations
        .try_reserve_exact(interfaces.len())
        .map_err(|_| {
            error(
                ErrorKind::ResourceExhausted,
                "export interface declarations",
            )
        })?;
    for interface in interfaces {
        if declarations.iter().any(
            |(request, _, _): &(InterfaceRequestV1, ExportAddress, u64)| {
                request.interface_id.bytes == interface.request.interface_id.bytes
                    && request.major == interface.request.major
            },
        ) {
            return Err(error(ErrorKind::Conflict, "duplicate exported interface"));
        }
        declarations.push((
            interface.request,
            interface.table,
            interface.supported_rights,
        ));
    }
    let rights = declarations
        .iter()
        .fold(0, |all, (_, _, rights)| all | rights);
    let value = Value::Object {
        type_id: TypeId::of::<T>(),
        value: Arc::new(value),
        interfaces: declarations.into_boxed_slice(),
    };
    let cleanup = move |value: &Value| match value {
        Value::Object { value, .. } => match value.downcast_ref::<T>() {
            Some(value) => close(value),
            None => Err(error(ErrorKind::Corrupted, "source cleanup type")),
        },
        Value::Diagnostic(_) => Err(error(ErrorKind::Corrupted, "source cleanup value")),
    };
    let lease = publish_value(&receiver.shared, value, rights, Some(Box::new(cleanup)))?;
    let raw = raw_object(&receiver.shared, lease);
    // SAFETY: 发布（publication）已经预登记并确认唯一持有。该独立持有只交给接收者一次。
    unsafe { Object::from_transfer(receiver.shared.connection.clone(), raw) }
}

pub(super) fn registry_with_access<T: Send + Sync + 'static, R>(
    receiver: &Registry,
    lease: LeaseV1,
    interface: InterfaceRequestV1,
    rights: u64,
    body: impl FnOnce(&T) -> Result<R>,
) -> Result<R> {
    if rights == 0 || rights & !RIGHTS != 0 {
        return Err(error(ErrorKind::PermissionDenied, "access rights"));
    }
    let access = acquire(&receiver.shared, &lease, rights, |entry| {
        let (_, _, declared_rights) = find_interface(entry, &interface)?;
        if rights & !(declared_rights & entry.rights) != 0 {
            return Err(ErrorKind::PermissionDenied);
        }
        match entry.value.as_deref() {
            Some(Value::Object { type_id, .. }) if *type_id == TypeId::of::<T>() => Ok(()),
            _ => Err(ErrorKind::Conflict),
        }
    })?;
    match access.value.as_deref() {
        Some(Value::Object { value, .. }) => match value.downcast_ref::<T>() {
            Some(value) => body(value),
            None => Err(error(ErrorKind::Corrupted, "source access type")),
        },
        _ => Err(error(ErrorKind::Corrupted, "source access value")),
    }
}

pub(super) unsafe fn registry_dispatch<T: Send + Sync + 'static>(
    context: *mut c_void,
    lease: LeaseV1,
    interface: InterfaceRequestV1,
    rights: u64,
    body: impl FnOnce(&T) -> Result<()>,
) -> OutcomeV1 {
    // SAFETY: 来源调用（source call）的外层代码保活覆盖此处的上下文（context）。
    let registry = match unsafe { registry_from_context(context) } {
        Ok(registry) => registry,
        Err(kind) => return diagnostic::limited_outcome(kind, ErrorKind::InvalidState),
    };
    let context = source_context("source dispatch", &lease);
    let result = boundary::catch(context, || {
        registry.with_access(lease, interface, rights, body)
    });
    source_outcome(&registry.shared, result)
}

pub(super) fn registry_revoke(receiver: &Registry, key: ObjectKeyV1) -> Result<()> {
    let result = {
        let mut state = receiver.shared.state.lock();
        entry_by_key(&mut state, &key).and_then(|entry| match entry.phase {
            EntryPhase::Live => {
                entry.phase = EntryPhase::Revoking;
                Ok(())
            }
            EntryPhase::Preparing => Err(ErrorKind::Busy),
            EntryPhase::Failed => Err(entry
                .failure
                .as_ref()
                .map_or(ErrorKind::Internal, |failure| failure.kind)),
            _ => Ok(()),
        })
    };
    result.map_err(|kind| error(kind, "revoke source object"))
}

pub(super) fn registry_maintain(receiver: &Registry) -> Result<u64> {
    let mut attempt = {
        let mut state = receiver.shared.state.lock();
        let taking = state.entries.iter_mut().find(|entry| {
            matches!(entry.phase, EntryPhase::Revoking)
                && entry.active == 0
                && entry
                    .holdings
                    .iter()
                    .all(|(_, holding)| matches!(holding.phase, HoldingPhase::Returned))
        });
        taking.map(|entry| {
            let value = entry
                .value
                .take()
                .expect("revoking entry must own its value");
            entry.phase = EntryPhase::Closing;
            CleanupAttempt {
                shared: Arc::clone(&receiver.shared),
                key: copy_key(&entry.key),
                value,
                close: entry.close.take(),
                resolved: false,
            }
        })
    };
    let Some(mut attempt) = attempt.take() else {
        return remaining_or_failure(&receiver.shared);
    };

    let close_result =
        boundary::catch(
            cleanup_context(&attempt, "close source object"),
            || match &attempt.close {
                Some(close) => close(&attempt.value),
                None => Ok(()),
            },
        );
    if let Err(failure) = close_result {
        let kind = *failure.current_context();
        if kind == ErrorKind::Busy {
            match discard_busy_report(&receiver.shared, failure) {
                Some(cleanup) => restore_cleanup(&mut attempt, EntryPhase::Failed, Some(cleanup)),
                None => restore_cleanup(&mut attempt, EntryPhase::Revoking, None),
            }
            return remaining_or_failure(&receiver.shared);
        }
        let (cached, unconfirmed) = finish_source_failure(&receiver.shared, failure);
        if let Some(cleanup) = unconfirmed {
            restore_cleanup(&mut attempt, EntryPhase::Failed, Some(cleanup));
            return remaining_or_failure(&receiver.shared);
        }
        let failure = Arc::new(source_error_from_snapshot(&cached));
        restore_cleanup(
            &mut attempt,
            EntryPhase::Failed,
            Some(Failure {
                kind,
                secondary: None,
                report: Some(Arc::clone(&failure)),
            }),
        );
        return Err(error(kind, "close source object").attach(failure));
    }

    {
        let mut state = receiver.shared.state.lock();
        let entry =
            entry_by_key(&mut state, &attempt.key).expect("cleanup entry must remain recorded");
        entry.phase = EntryPhase::ReadyToDrop;
    }
    let value = Arc::clone(&attempt.value);
    let close = attempt.close.take();
    let key = copy_key(&attempt.key);
    let value_context = cleanup_context(&attempt, "drop source object");
    let close_context = cleanup_context(&attempt, "drop source close callback");
    {
        let mut state = receiver.shared.state.lock();
        let entry = entry_by_key(&mut state, &key).expect("cleanup entry must remain recorded");
        entry.phase = EntryPhase::Dropping;
    }
    attempt.resolved = true;
    drop(attempt);

    // 每一项析构（drop）独立受保护。即使其中一项失败也只尝试一次。
    let value_result = boundary::catch(value_context, || {
        drop(value);
        Ok(())
    });
    let close_result = boundary::catch(close_context, || {
        drop(close);
        Ok(())
    });
    let failure = match (value_result, close_result) {
        (Ok(()), Ok(())) => None,
        (Err(primary), Err(secondary)) => {
            let kind = *secondary.current_context();
            Some((primary.attach(secondary), Some(kind)))
        }
        (Err(failure), _) | (_, Err(failure)) => Some((failure, None)),
    };
    if let Some((report, secondary)) = failure {
        let kind = *report.current_context();
        let report = Arc::new(report);
        let retrievable = Arc::clone(&report);
        {
            let mut state = receiver.shared.state.lock();
            let entry = entry_by_key(&mut state, &key).expect("cleanup entry must remain recorded");
            entry.phase = EntryPhase::Failed;
            entry.failure = Some(Failure {
                kind,
                secondary,
                report: Some(report),
            });
        }
        return Err(error(kind, "drop source object").attach(retrievable));
    }
    {
        let mut state = receiver.shared.state.lock();
        let entry = entry_by_key(&mut state, &key).expect("cleanup entry must remain recorded");
        entry.phase = EntryPhase::Retired;
    }
    remaining_or_failure(&receiver.shared)
}

pub(super) fn registry_try_retire(receiver: &Registry) -> Result<()> {
    let request = {
        let mut state = receiver.shared.state.lock();
        if matches!(state.phase, SourcePhase::Retired) {
            return Ok(());
        }
        match &state.retirement_request {
            RetirementRequest::Assigned(operation) => {
                Ok((state.source, Some(copy_operation(operation))))
            }
            RetirementRequest::Assigning => Err(ErrorKind::Busy),
            RetirementRequest::Unassigned => {
                state.retirement_request = RetirementRequest::Assigning;
                Ok((state.source, None))
            }
        }
    };
    let (source, pending) = request.map_err(|kind| error(kind, "assign source retirement"))?;
    let connection = &receiver.shared.connection;
    let operation = match pending {
        Some(operation) => operation,
        None => {
            let next = connection.next_operation();
            let mut state = receiver.shared.state.lock();
            match next {
                Ok(operation) => {
                    if matches!(state.phase, SourcePhase::Retired) {
                        return Ok(());
                    }
                    state.retirement_request =
                        RetirementRequest::Assigned(copy_operation(&operation));
                    if matches!(state.phase, SourcePhase::Open) {
                        state.phase = SourcePhase::Closing;
                    }
                    operation
                }
                Err(failure) => {
                    state.retirement_request = RetirementRequest::Unassigned;
                    if matches!(state.phase, SourcePhase::Retired) {
                        return Ok(());
                    }
                    return Err(failure);
                }
            }
        }
    };
    diagnostic::decode(connection, connection.retire_raw(source, operation))
}

pub(super) fn access_drop(receiver: &mut Access) {
    if receiver.finished {
        return;
    }
    drop(receiver.value.take());
    let mut state = receiver.shared.state.lock();
    let entry =
        entry_by_key(&mut state, &receiver.key).expect("active access must remain recorded");
    entry.active -= 1;
    receiver.finished = true;
}

pub(super) fn cleanup_attempt_drop(receiver: &mut CleanupAttempt) {
    if !receiver.resolved {
        restore_cleanup(receiver, EntryPhase::Failed, Some(unknown_failure()));
    }
}

pub(super) fn mutation_drop(receiver: &mut Mutation) {
    if receiver.resolved {
        return;
    }
    let mut state = receiver.shared.state.lock();
    if let Some(key) = &receiver.object {
        if let Ok(entry) = entry_by_key(&mut state, key) {
            mark_entry_failed(entry);
        }
    } else {
        state.phase = SourcePhase::Failed;
        state.failure.get_or_insert_with(unknown_failure);
    }
    if let Some(operation) = &receiver.operation {
        if let Ok(index) = operation_index(&state, operation) {
            let op = &mut state.operations[index].1;
            if matches!(op.phase, OperationPhase::Running | OperationPhase::Pending) {
                op.phase = OperationPhase::Failed(CachedFailure::Unavailable {
                    operation: ErrorKind::Internal,
                    diagnostic: ErrorKind::InvalidState,
                });
            }
        }
    }
}

pub(super) unsafe fn exported_interface_new<T: Send + Sync + 'static, I: AbiInterface>(
    table: &'static I::Table,
) -> Result<ExportedInterface<T>> {
    let size = u32::try_from(size_of::<I::Table>())
        .map_err(|_| error(ErrorKind::InvalidInput, "export interface size"))?;
    if I::ID.bytes == [0; 16]
        || I::ID.bytes == HOST_CONTROL_ID.bytes
        || I::MAJOR == 0
        || I::SUPPORTED_RIGHTS & !RIGHTS != 0
        || size_of::<I::Table>() < size_of::<ApiHeaderV1>()
        || align_of::<I::Table>() < align_of::<ApiHeaderV1>()
    {
        return Err(error(
            ErrorKind::InvalidInput,
            "export interface declaration",
        ));
    }
    let pointer = std::ptr::from_ref(table);
    // SAFETY: AbiInterface 要求表的首字段（first field）为固定 C 表头（API header）。
    let header = unsafe { &*pointer.cast::<ApiHeaderV1>() };
    if header.major != I::MAJOR || header.size != size {
        return Err(error(ErrorKind::Corrupted, "export interface header"));
    }
    Ok(ExportedInterface {
        request: InterfaceRequestV1 {
            interface_id: I::ID,
            major: I::MAJOR,
            size,
        },
        table: ExportAddress {
            pointer: pointer.cast(),
        },
        supported_rights: I::SUPPORTED_RIGHTS,
        marker: PhantomData,
    })
}

pub(super) unsafe fn validate(
    context: *mut c_void,
    lease: LeaseV1,
    request: InterfaceRequestV1,
    rights: u64,
) -> OutcomeV1 {
    // SAFETY: 调用方先为来源和完整 C 表取得活动调用保护。
    let registry = match unsafe { registry_from_context(context) } {
        Ok(registry) => registry,
        Err(kind) => return diagnostic::limited_outcome(kind, ErrorKind::InvalidState),
    };
    let result = {
        let mut state = registry.shared.state.lock();
        let open = matches!(state.phase, SourcePhase::Open);
        if rights & !RIGHTS != 0 {
            Err(ErrorKind::PermissionDenied)
        } else {
            active_entry(&mut state, &lease, rights, open).and_then(|entry| {
                let supported = if get_management_request(&request) {
                    validate_kind(entry, false)?;
                    READ
                } else {
                    let (_, _, rights) = find_interface(entry, &request)?;
                    rights & entry.rights
                };
                if rights & !supported == 0 {
                    Ok(())
                } else {
                    Err(ErrorKind::PermissionDenied)
                }
            })
        }
    };
    source_outcome(
        &registry.shared,
        result.map_err(|kind| error(kind, "validate source object")),
    )
}

pub(super) unsafe fn retain(
    context: *mut c_void,
    lease: LeaseV1,
    operation: OperationIdV1,
    out: *mut ObjectRefV1,
) -> OutcomeV1 {
    // SAFETY: 调用方的代码保活保护真实来源上下文（source context）。
    let registry = match unsafe { registry_from_context(context) } {
        Ok(registry) => registry,
        Err(kind) => return diagnostic::limited_outcome(kind, ErrorKind::InvalidState),
    };
    if !writable(out) {
        return source_outcome(
            &registry.shared,
            Err(error(ErrorKind::InvalidInput, "retain output")),
        );
    }
    let shared = &registry.shared;
    let request = OperationInput::Retain(copy_lease(&lease));
    let result = match start_operation(shared, &operation, request, Some(copy_key(&lease.object))) {
        Ok(Start::New(mutation)) => {
            let result = retain_value(shared, &lease);
            match result {
                Ok(held) => {
                    let raw = raw_object(shared, held);
                    complete_operation(mutation, OperationPhase::Retained(copy_lease(&raw.lease)));
                    Ok(raw)
                }
                Err(failure) => return reject_operation(mutation, failure, false),
            }
        }
        Ok(Start::Existing(OperationPhase::Retained(retained))) => {
            let result = {
                let mut state = shared.state.lock();
                let open = matches!(state.phase, SourcePhase::Open);
                active_entry(&mut state, &retained, READ, open)
                    .map(|_| raw_object(shared, retained))
            };
            result.map_err(|kind| error(kind, "retained holding replay"))
        }
        Ok(Start::Existing(OperationPhase::Failed(failure))) => {
            return publish_failure(shared, &failure)
        }
        Ok(Start::Existing(_)) => Err(error(ErrorKind::Corrupted, "retain replay state")),
        Err(kind) => Err(error(kind, "retain operation")),
    };
    match result {
        Ok(raw) => {
            // SAFETY: 输出槽（output slot）由调用方独占并已核对。失败路径不写入。
            unsafe { out.write(raw) };
            diagnostic::success_outcome()
        }
        Err(failure) => source_outcome(shared, Err(failure)),
    }
}

pub(super) unsafe fn release(
    context: *mut c_void,
    lease: LeaseV1,
    operation: OperationIdV1,
) -> OutcomeV1 {
    // SAFETY: 宿主在实际释放回调（release callback）期间保护本来源。
    let registry = match unsafe { registry_from_context(context) } {
        Ok(registry) => registry,
        Err(kind) => return diagnostic::limited_outcome(kind, ErrorKind::InvalidState),
    };
    let shared = &registry.shared;
    let request = OperationInput::Release(copy_lease(&lease));
    match start_operation(shared, &operation, request, Some(copy_key(&lease.object))) {
        Ok(Start::New(mutation)) => {
            let result = {
                let mut state = shared.state.lock();
                returned_holding(&mut state, &lease, false)
            };
            match result {
                Ok(()) => {
                    complete_operation(mutation, OperationPhase::Released);
                    diagnostic::success_outcome()
                }
                Err(kind) => reject_operation(mutation, error(kind, "release holding"), false),
            }
        }
        Ok(Start::Existing(OperationPhase::Released)) => diagnostic::success_outcome(),
        Ok(Start::Existing(OperationPhase::Failed(failure))) => publish_failure(shared, &failure),
        Ok(Start::Existing(_)) => source_outcome(
            shared,
            Err(error(ErrorKind::Corrupted, "release replay state")),
        ),
        Err(kind) => source_outcome(shared, Err(error(kind, "release operation"))),
    }
}

pub(super) unsafe fn query_interface(
    context: *mut c_void,
    lease: LeaseV1,
    request: InterfaceRequestV1,
    out: *mut InterfaceViewV1,
    forwarded_view: fn(&Connection) -> Result<InterfaceViewV1>,
) -> OutcomeV1 {
    // SAFETY: 调用方的外层保护覆盖真实来源上下文（source context）和基础表。
    let registry = match unsafe { registry_from_context(context) } {
        Ok(registry) => registry,
        Err(kind) => return diagnostic::limited_outcome(kind, ErrorKind::InvalidState),
    };
    let shared = &registry.shared;
    if !writable(out) {
        return source_outcome(
            shared,
            Err(error(ErrorKind::InvalidInput, "query interface output")),
        );
    }
    let table = {
        let mut state = shared.state.lock();
        let open = matches!(state.phase, SourcePhase::Open);
        active_entry(&mut state, &lease, READ, open).and_then(|entry| {
            validate_kind(entry, false)?;
            if request.interface_id.bytes == HOST_CONTROL_ID.bytes {
                if get_management_request(&request) {
                    Ok(None)
                } else {
                    Err(ErrorKind::Unsupported)
                }
            } else {
                find_interface(entry, &request).map(|(_, view, _)| Some(view.pointer))
            }
        })
    };
    let view = match table {
        Ok(None) => forwarded_view(&shared.connection),
        Ok(Some(table)) => Ok(InterfaceViewV1 { context, table }),
        Err(kind) => Err(error(kind, "query source interface")),
    };
    match view {
        Ok(view) => {
            // SAFETY: 已核调用方独占的输出槽（output slot）。借用仍由原持有保护。
            unsafe { out.write(view) };
            diagnostic::success_outcome()
        }
        Err(failure) => source_outcome(shared, Err(failure)),
    }
}

fn get_management_request(request: &InterfaceRequestV1) -> bool {
    request.interface_id.bytes == HOST_CONTROL_ID.bytes
        && request.major == HOST_CONTROL_MAJOR
        && request.size as usize == size_of::<pi_ffi_abi::HostControlApiV1>()
}

pub(super) unsafe fn diagnostic_info(
    context: *mut c_void,
    lease: LeaseV1,
    out: *mut DiagnosticInfoV1,
) -> u32 {
    if !writable(out) {
        return boundary::encode_kind(ErrorKind::InvalidInput);
    }
    // SAFETY: 本次诊断调用（diagnostic call）的代码保活覆盖来源上下文。
    let registry = match unsafe { registry_from_context(context) } {
        Ok(registry) => registry,
        Err(kind) => return boundary::encode_kind(kind),
    };
    let access = match acquire(&registry.shared, &lease, READ, validate_diagnostic) {
        Ok(access) => access,
        Err(error) => return boundary::encode_kind(*error.current_context()),
    };
    let Some(Value::Diagnostic(snapshot)) = access.value.as_deref() else {
        return boundary::encode_kind(ErrorKind::Corrupted);
    };
    let data = DiagnosticInfoV1 {
        kind: boundary::encode_kind(snapshot.kind),
        user_len: snapshot.user.len() as u64,
        verbose_len: snapshot.verbose.len() as u64,
        debug_len: snapshot.debug.len() as u64,
    };
    // SAFETY: out 是调用方提供的独占可写输出槽（output slot）。
    unsafe { out.write(data) };
    0
}

pub(super) unsafe fn diagnostic_copy(
    context: *mut c_void,
    lease: LeaseV1,
    buffer: *mut u8,
    capacity: u64,
    written: *mut u64,
) -> u32 {
    if !writable(written) {
        return boundary::encode_kind(ErrorKind::InvalidInput);
    }
    // SAFETY: 调用方持有诊断来源（diagnostic source）的活动代码保活。
    let registry = match unsafe { registry_from_context(context) } {
        Ok(registry) => registry,
        Err(kind) => return boundary::encode_kind(kind),
    };
    let access = match acquire(&registry.shared, &lease, READ, validate_diagnostic) {
        Ok(access) => access,
        Err(error) => return boundary::encode_kind(*error.current_context()),
    };
    let Some(Value::Diagnostic(snapshot)) = access.value.as_deref() else {
        return boundary::encode_kind(ErrorKind::Corrupted);
    };
    let parts = [
        snapshot.user.as_bytes(),
        snapshot.verbose.as_bytes(),
        snapshot.debug.as_bytes(),
    ];
    let total = parts
        .iter()
        .try_fold(0usize, |count, part| count.checked_add(part.len()));
    let Some(total) = total.filter(|total| *total <= isize::MAX as usize) else {
        return boundary::encode_kind(ErrorKind::Corrupted);
    };
    if capacity < total as u64 || (total != 0 && buffer.is_null()) {
        return boundary::encode_kind(ErrorKind::InvalidInput);
    }
    let mut offset = 0;
    for part in parts {
        if !part.is_empty() {
            // SAFETY: 调用方提供非重叠缓冲区（non-overlapping buffer），每段都在已核容量内。
            unsafe { std::ptr::copy_nonoverlapping(part.as_ptr(), buffer.add(offset), part.len()) };
            offset += part.len();
        }
    }
    // SAFETY: written 是独占可写输出槽（output slot），只在完整复制后报告。
    unsafe { written.write(total as u64) };
    0
}

pub(super) unsafe fn diagnostic_release(context: *mut c_void, lease: LeaseV1) -> u32 {
    // SAFETY: 引导（bootstrap）外层保护或宿主释放调用持续保护同一来源。
    let registry = match unsafe { registry_from_context(context) } {
        Ok(registry) => registry,
        Err(kind) => return boundary::encode_kind(kind),
    };
    let shared = &registry.shared;
    let valid = {
        let mut state = shared.state.lock();
        entry_by_lease(&mut state, &lease).and_then(|entry| {
            validate_kind(entry, true)?;
            let holding = find_holding(entry, lease.holding)?;
            if holding.tracked && !matches!(holding.phase, HoldingPhase::Preparing) {
                Ok(())
            } else {
                Err(ErrorKind::InvalidState)
            }
        })
    };
    if let Err(kind) = valid {
        return boundary::encode_kind(kind);
    }

    // 先完成已登记责任的提交。此调用幂等（idempotent），不会重入来源回调。
    let target = target(shared, copy_lease(&lease), TARGET_DIAGNOSTIC);
    let submitted = shared.connection.submit_release(target);
    if submitted != 0 {
        return submitted;
    }
    let mut state = shared.state.lock();
    match returned_holding(&mut state, &lease, true) {
        Ok(()) => 0,
        Err(kind) => boundary::encode_kind(kind),
    }
}

pub(super) unsafe fn try_retire(
    context: *mut c_void,
    source: u64,
    operation: OperationIdV1,
) -> OutcomeV1 {
    // SAFETY: 宿主独占的来源控制调用（source control call）保护上下文。
    let registry = match unsafe { registry_from_context(context) } {
        Ok(registry) => registry,
        Err(kind) => return diagnostic::limited_outcome(kind, ErrorKind::InvalidState),
    };
    let shared = &registry.shared;
    if let Err(failure) = check_source(shared, source) {
        return source_outcome(shared, Err(failure));
    }
    match start_operation(shared, &operation, OperationInput::Retire, None) {
        Ok(Start::New(mutation)) => {
            let result = retirement_check(shared);
            match result {
                Ok(()) => {
                    complete_operation(mutation, OperationPhase::Retired);
                    diagnostic::success_outcome()
                }
                Err(failure) => reject_operation(mutation, failure, true),
            }
        }
        Ok(Start::Existing(OperationPhase::Retired)) => diagnostic::success_outcome(),
        Ok(Start::Existing(OperationPhase::Failed(failure))) => publish_failure(shared, &failure),
        Ok(Start::Existing(_)) => source_outcome(
            shared,
            Err(error(ErrorKind::Corrupted, "retirement replay")),
        ),
        Err(ErrorKind::Busy) => {
            diagnostic::limited_outcome(ErrorKind::Busy, ErrorKind::InvalidState)
        }
        Err(kind) => source_outcome(shared, Err(error(kind, "retire source"))),
    }
}

pub(super) unsafe fn maintain(
    context: *mut c_void,
    source: u64,
    operation: OperationIdV1,
    out: *mut MaintenanceV1,
) -> OutcomeV1 {
    // SAFETY: 宿主在清理推进（maintenance）期间保有该登记来源。
    let registry = match unsafe { registry_from_context(context) } {
        Ok(registry) => registry,
        Err(kind) => return diagnostic::limited_outcome(kind, ErrorKind::InvalidState),
    };
    let shared = &registry.shared;
    if !writable(out) {
        return source_outcome(
            shared,
            Err(error(ErrorKind::InvalidInput, "maintenance output")),
        );
    }
    if let Err(failure) = check_source(shared, source) {
        return source_outcome(shared, Err(failure));
    }
    let snapshot = match start_operation(shared, &operation, OperationInput::Maintain, None) {
        Ok(Start::New(mutation)) => match registry.maintain() {
            Ok(remaining) => {
                let snapshot = MaintenanceV1 { remaining };
                complete_operation(mutation, OperationPhase::Maintained(snapshot));
                Ok(snapshot)
            }
            Err(failure) => return reject_operation(mutation, failure, false),
        },
        Ok(Start::Existing(OperationPhase::Maintained(snapshot))) => Ok(snapshot),
        Ok(Start::Existing(OperationPhase::Failed(failure))) => {
            return publish_failure(shared, &failure)
        }
        Ok(Start::Existing(_)) => Err(error(ErrorKind::Corrupted, "maintenance replay")),
        Err(kind) => Err(error(kind, "maintain source")),
    };
    match snapshot {
        Ok(snapshot) => {
            // SAFETY: 调用方保证输出槽（output slot）可写且独占，只有成功才写回。
            unsafe { out.write(snapshot) };
            diagnostic::success_outcome()
        }
        Err(failure) => source_outcome(shared, Err(failure)),
    }
}

pub(super) unsafe fn release_context(context: *mut c_void, source: u64) -> u32 {
    let pointer = context.cast::<SourceContext>();
    if !writable(pointer) {
        return boundary::encode_kind(ErrorKind::InvalidInput);
    }
    // SAFETY: 宿主拥有唯一上下文释放资格（context release authority），该指针仍然存活。
    let shared = &unsafe { &*pointer }.shared;
    let detached = {
        let mut state = shared.state.lock();
        if state.source != source || source == 0 {
            return boundary::encode_kind(ErrorKind::Conflict);
        }
        if !matches!(state.phase, SourcePhase::Retiring)
            || state.failure.is_some()
            || state
                .entries
                .iter()
                .any(|entry| !matches!(entry.phase, EntryPhase::Retired))
        {
            return boundary::encode_kind(ErrorKind::InvalidState);
        }
        state.phase = SourcePhase::Retired;
        (
            std::mem::take(&mut state.entries),
            std::mem::take(&mut state.operations),
        )
    };
    drop(detached);
    // SAFETY: 取得唯一资格后恰好消费登记时分配的 Box（owned allocation）。
    drop(unsafe { Box::from_raw(pointer) });
    0
}

pub(super) fn publish_management_failure(
    registry: &Registry,
    failure: &crate::diagnostic::ManagementFailure,
) -> OutcomeV1 {
    let shared = &registry.shared;
    if failure.cleanup_unconfirmed {
        let (original_kind, cleanup_kind) = failure.report_cleanup.as_ref().map_or_else(
            || (snapshot_kind(&failure.cached), ErrorKind::Internal),
            |cleanup| (cleanup.original_kind, cleanup.cleanup_kind),
        );
        let recorded = Failure {
            kind: cleanup_kind,
            secondary: Some(original_kind),
            report: Some(Arc::new(source_error_from_snapshot(&failure.cached))),
        };
        retain_cleanup_failure(shared, &recorded);
    }
    publish_failure(shared, &failure.cached)
}

fn source_outcome(shared: &Shared, result: Result<()>) -> OutcomeV1 {
    match result {
        Ok(()) => diagnostic::success_outcome(),
        Err(failure) => {
            let (cached, _) = finish_source_failure(shared, failure);
            publish_failure(shared, &cached)
        }
    }
}

fn finish_source_failure(shared: &Shared, original: Error) -> (CachedFailure, Option<Failure>) {
    let kind = *original.current_context();
    let failure = diagnostic::ManagementFailure::capture(&original);
    if let Err(report) = drop_source_report(shared, original) {
        let cleanup = retain_unconfirmed_cleanup(shared, kind, Some(report));
        return (
            CachedFailure::Unavailable {
                operation: kind,
                diagnostic: cleanup.kind,
            },
            Some(cleanup),
        );
    }
    let cleanup = failure
        .cleanup_unconfirmed
        .then(|| retain_unconfirmed_cleanup(shared, kind, None));
    (failure.cached, cleanup)
}

fn discard_busy_report(shared: &Shared, original: Error) -> Option<Failure> {
    let marked = original
        .downcast_ref::<boundary::UnconfirmedPanicCleanup>()
        .is_some();
    let report = drop_source_report(shared, original).err();
    (marked || report.is_some())
        .then(|| retain_unconfirmed_cleanup(shared, ErrorKind::Busy, report))
}

fn drop_source_report(shared: &Shared, original: Error) -> Result<()> {
    let initialization = shared.connection.initialization();
    let source = shared.state.lock().source;
    boundary::catch(
        boundary::Context {
            operation: "drop source report",
            module_epoch: initialization.module_epoch,
            round: initialization.round,
            source,
        },
        || {
            drop(original);
            Ok(())
        },
    )
}

fn retain_unconfirmed_cleanup(
    shared: &Shared,
    original: ErrorKind,
    report: Option<Error>,
) -> Failure {
    let kind = report
        .as_ref()
        .map_or(ErrorKind::Internal, |report| *report.current_context());
    let failure = Failure {
        kind,
        secondary: Some(original),
        report: report.map(Arc::new),
    };
    retain_cleanup_failure(shared, &failure);
    failure
}

fn retain_cleanup_failure(shared: &Shared, failure: &Failure) {
    let mut state = shared.state.lock();
    if !matches!(state.phase, SourcePhase::Retired) {
        state.phase = SourcePhase::Failed;
        if state.failure.is_none() {
            state.failure = Some(copy_recorded_failure(failure));
        }
    }
}

fn snapshot_kind(failure: &CachedFailure) -> ErrorKind {
    match failure {
        CachedFailure::Complete(snapshot) => snapshot.kind,
        CachedFailure::Unavailable { operation, .. } => *operation,
    }
}

fn source_error_from_snapshot(failure: &CachedFailure) -> Error {
    match failure {
        CachedFailure::Complete(snapshot) => diagnostic::CopiedDiagnostic {
            kind: snapshot.kind,
            user: snapshot.user.clone(),
            verbose: snapshot.verbose.clone(),
            debug: snapshot.debug.clone(),
            raw_kind: boundary::encode_kind(snapshot.kind),
        }
        .into_error(),
        CachedFailure::Unavailable {
            operation,
            diagnostic,
        } => error(*operation, "source close failure")
            .attach(error(*diagnostic, "source close diagnostic snapshot")),
    }
}

fn publish_failure(shared: &Shared, failure: &CachedFailure) -> OutcomeV1 {
    match failure {
        CachedFailure::Complete(snapshot) => {
            let kind = snapshot.kind;
            let value = Value::Diagnostic(Arc::clone(snapshot));
            let lease = match publish_value(shared, value, READ, None) {
                Ok(lease) => lease,
                Err(diagnostic) => {
                    return diagnostic::limited_outcome(kind, *diagnostic.current_context())
                }
            };
            OutcomeV1 {
                kind: boundary::encode_kind(kind),
                diagnostic_failure_kind: 0,
                diagnostic: DiagnosticRefV1 {
                    context: source_address(shared),
                    api: &DIAGNOSTIC_API,
                    lease,
                },
            }
        }
        CachedFailure::Unavailable {
            operation,
            diagnostic,
        } => diagnostic::limited_outcome(*operation, *diagnostic),
    }
}

fn publish_value(
    shared: &Shared,
    value: Value,
    rights: u64,
    close: Option<Box<dyn Fn(&Value) -> Result<()> + Send + Sync>>,
) -> Result<LeaseV1> {
    let is_diagnostic = matches!(value, Value::Diagnostic(_));
    let target_kind = if is_diagnostic {
        TARGET_DIAGNOSTIC
    } else {
        TARGET_OBJECT
    };
    let initialization = shared.connection.initialization();
    let mut holdings = Vec::new();
    holdings
        .try_reserve_exact(1)
        .map_err(|_| error(ErrorKind::ResourceExhausted, "prepare source holding"))?;
    let value = Arc::new(value);
    let published = {
        let mut state = shared.state.lock();
        if !accepts_publication(&state.phase, is_diagnostic) {
            Err(ErrorKind::InvalidState)
        } else {
            let slot = state.next_slot;
            let holding = state.next_holding;
            match (
                slot.checked_add(1),
                holding.checked_add(1),
                state.entries.try_reserve(1),
            ) {
                (Some(next_slot), Some(next_holding), Ok(())) => {
                    let key = ObjectKeyV1 {
                        source: state.source,
                        slot,
                        generation: 1,
                    };
                    holdings.push((
                        holding,
                        Holding {
                            phase: HoldingPhase::Preparing,
                            tracked: false,
                        },
                    ));
                    state.entries.push(Entry {
                        key: copy_key(&key),
                        initialization: copy_round(&initialization),
                        phase: EntryPhase::Preparing,
                        rights,
                        active: 0,
                        holdings,
                        value: Some(value),
                        close,
                        failure: None,
                    });
                    state.next_slot = next_slot;
                    state.next_holding = next_holding;
                    Ok(LeaseV1 {
                        object: key,
                        initialization,
                        holding,
                    })
                }
                _ => Err(ErrorKind::ResourceExhausted),
            }
        }
    };
    let lease = published.map_err(|kind| error(kind, "prepare source object"))?;
    track_publication(shared, &lease, target_kind, true)?;
    Ok(lease)
}

fn retain_value(shared: &Shared, original: &LeaseV1) -> Result<LeaseV1> {
    let prepared = {
        let mut state = shared.state.lock();
        let holding = state.next_holding;
        let open = matches!(state.phase, SourcePhase::Open);
        let candidate = active_entry(&mut state, original, WORK, open).and_then(|entry| {
            validate_kind(entry, false)?;
            // 共同取得（retain）按对象准入判定，无需对象支持工作权限（work right）。
            if !matches!(entry.phase, EntryPhase::Live) || !open {
                return Err(ErrorKind::InvalidState);
            }
            let next = holding.checked_add(1).ok_or(ErrorKind::ResourceExhausted)?;
            entry
                .holdings
                .try_reserve(1)
                .map_err(|_| ErrorKind::ResourceExhausted)?;
            entry.holdings.push((
                holding,
                Holding {
                    phase: HoldingPhase::Preparing,
                    tracked: false,
                },
            ));
            Ok(next)
        });
        candidate.map(|next| {
            state.next_holding = next;
            LeaseV1 {
                object: copy_key(&original.object),
                initialization: copy_round(&original.initialization),
                holding,
            }
        })
    };
    let lease = prepared.map_err(|kind| error(kind, "prepare retained holding"))?;
    track_publication(shared, &lease, TARGET_OBJECT, false)?;
    Ok(lease)
}

fn track_publication(
    shared: &Shared,
    lease: &LeaseV1,
    target_kind: u32,
    first: bool,
) -> Result<()> {
    let tracked = shared
        .connection
        .track(target(shared, copy_lease(lease), target_kind));
    let ready = {
        let mut state = shared.state.lock();
        let open = accepts_publication(&state.phase, target_kind == TARGET_DIAGNOSTIC);
        let entry =
            entry_by_lease(&mut state, lease).expect("pending holding must remain recorded");
        let phase = if first {
            matches!(entry.phase, EntryPhase::Preparing)
        } else {
            matches!(entry.phase, EntryPhase::Live)
        };
        let holding =
            find_holding(entry, lease.holding).expect("pending holding must remain recorded");
        if tracked.is_err() {
            holding.phase = HoldingPhase::Returned;
            if (first
                || entry
                    .holdings
                    .iter()
                    .all(|(_, holding)| matches!(holding.phase, HoldingPhase::Returned)))
                && matches!(entry.phase, EntryPhase::Preparing | EntryPhase::Live)
            {
                entry.phase = EntryPhase::Revoking;
            }
            false
        } else {
            holding.tracked = true;
            if open && phase && matches!(holding.phase, HoldingPhase::Preparing) {
                holding.phase = HoldingPhase::Live;
                if first {
                    entry.phase = EntryPhase::Live;
                }
                true
            } else {
                if matches!(holding.phase, HoldingPhase::Preparing) {
                    holding.phase = HoldingPhase::ReleaseRequested;
                }
                if first && matches!(entry.phase, EntryPhase::Preparing) {
                    entry.phase = EntryPhase::Revoking;
                }
                false
            }
        }
    };
    tracked?;
    if ready {
        return Ok(());
    }
    // 宿主已建立记录，来源因并发关闭（concurrent close）未发布，仍须提交这项义务。
    let status = shared
        .connection
        .submit_release(target(shared, copy_lease(lease), target_kind));
    let failure = error(ErrorKind::InvalidState, "publish source holding");
    if status == 0 {
        Err(failure)
    } else {
        Err(failure.attach(
            error(boundary::decode_kind(status), "release unpublished holding").attach(status),
        ))
    }
}

fn accepts_publication(phase: &SourcePhase, diagnostic: bool) -> bool {
    matches!(phase, SourcePhase::Open)
        || (diagnostic && matches!(phase, SourcePhase::Closing | SourcePhase::Failed))
}

fn acquire(
    shared: &Arc<Shared>,
    lease: &LeaseV1,
    rights: u64,
    check: impl FnOnce(&Entry) -> core::result::Result<(), ErrorKind>,
) -> Result<Access> {
    let access = {
        let mut state = shared.state.lock();
        let open = matches!(state.phase, SourcePhase::Open);
        active_entry(&mut state, lease, rights, open).and_then(|entry| {
            check(entry)?;
            let active = entry
                .active
                .checked_add(1)
                .ok_or(ErrorKind::ResourceExhausted)?;
            let value = entry.value.as_ref().ok_or(ErrorKind::Corrupted)?;
            let value = Arc::clone(value);
            entry.active = active;
            Ok(Access {
                shared: Arc::clone(shared),
                key: copy_key(&lease.object),
                value: Some(value),
                finished: false,
            })
        })
    };
    access.map_err(|kind| error(kind, "source access"))
}

fn active_entry<'a>(
    state: &'a mut State,
    lease: &LeaseV1,
    rights: u64,
    open: bool,
) -> core::result::Result<&'a mut Entry, ErrorKind> {
    if !matches!(
        state.phase,
        SourcePhase::Open | SourcePhase::Closing | SourcePhase::Failed
    ) {
        return Err(ErrorKind::InvalidState);
    }
    let entry = entry_by_lease(state, lease)?;
    let holding = find_holding(entry, lease.holding)?;
    if !holding.tracked || !matches!(holding.phase, HoldingPhase::Live) {
        return Err(ErrorKind::InvalidState);
    }
    match (&entry.phase, open, rights & WORK != 0) {
        (EntryPhase::Live, true, _) | (EntryPhase::Live | EntryPhase::Revoking, _, false) => {
            Ok(entry)
        }
        _ => Err(ErrorKind::InvalidState),
    }
}

fn entry_by_key<'a>(
    state: &'a mut State,
    key: &ObjectKeyV1,
) -> core::result::Result<&'a mut Entry, ErrorKind> {
    if key.source != state.source || key.source == 0 || key.generation != 1 {
        return Err(ErrorKind::Conflict);
    }
    let index = key
        .slot
        .checked_sub(1)
        .and_then(|slot| usize::try_from(slot).ok())
        .ok_or(ErrorKind::NotFound)?;
    let entry = state.entries.get_mut(index).ok_or(ErrorKind::NotFound)?;
    if key.slot != entry.key.slot || key.generation != entry.key.generation {
        return Err(ErrorKind::Conflict);
    }
    Ok(entry)
}

fn entry_by_lease<'a>(
    state: &'a mut State,
    lease: &LeaseV1,
) -> core::result::Result<&'a mut Entry, ErrorKind> {
    let entry = entry_by_key(state, &lease.object)?;
    if entry.initialization.module_epoch != lease.initialization.module_epoch
        || entry.initialization.round != lease.initialization.round
    {
        return Err(ErrorKind::Conflict);
    }
    Ok(entry)
}

fn find_holding(entry: &mut Entry, holding: u64) -> core::result::Result<&mut Holding, ErrorKind> {
    let index = entry
        .holdings
        .binary_search_by_key(&holding, |(id, _)| *id)
        .map_err(|_| ErrorKind::NotFound)?;
    Ok(&mut entry.holdings[index].1)
}

fn validate_kind(entry: &Entry, diagnostic: bool) -> core::result::Result<(), ErrorKind> {
    match entry.value.as_deref() {
        Some(Value::Diagnostic(_)) if diagnostic => Ok(()),
        Some(Value::Object { .. }) if !diagnostic => Ok(()),
        // 本回调固定对应种类。值已清除后只允许有效归还的重放。
        None if matches!(
            entry.phase,
            EntryPhase::Closing
                | EntryPhase::ReadyToDrop
                | EntryPhase::Dropping
                | EntryPhase::Retired
                | EntryPhase::Failed
        ) =>
        {
            Ok(())
        }
        _ => Err(ErrorKind::Conflict),
    }
}

fn validate_diagnostic(entry: &Entry) -> core::result::Result<(), ErrorKind> {
    validate_kind(entry, true)
}

fn find_interface<'a>(
    entry: &'a Entry,
    request: &InterfaceRequestV1,
) -> core::result::Result<&'a (InterfaceRequestV1, ExportAddress, u64), ErrorKind> {
    let Some(Value::Object { interfaces, .. }) = entry.value.as_deref() else {
        return Err(ErrorKind::InvalidState);
    };
    if !matches!(entry.phase, EntryPhase::Live | EntryPhase::Revoking) {
        return Err(ErrorKind::InvalidState);
    }
    interfaces
        .iter()
        .find(|(declared, _, _)| {
            declared.interface_id.bytes == request.interface_id.bytes
                && declared.major == request.major
                && declared.size == request.size
        })
        .ok_or(ErrorKind::Unsupported)
}

fn returned_holding(
    state: &mut State,
    lease: &LeaseV1,
    diagnostic: bool,
) -> core::result::Result<(), ErrorKind> {
    let entry = entry_by_lease(state, lease)?;
    validate_kind(entry, diagnostic)?;
    let holding = find_holding(entry, lease.holding)?;
    if !holding.tracked {
        return Err(ErrorKind::InvalidState);
    }
    match holding.phase {
        HoldingPhase::Live | HoldingPhase::ReleaseRequested => {
            holding.phase = HoldingPhase::Returned
        }
        HoldingPhase::Returned => return Ok(()),
        HoldingPhase::Preparing => return Err(ErrorKind::Busy),
    }
    if matches!(entry.phase, EntryPhase::Live)
        && entry
            .holdings
            .iter()
            .all(|(_, holding)| matches!(holding.phase, HoldingPhase::Returned))
    {
        entry.phase = EntryPhase::Revoking;
    }
    Ok(())
}

enum Start {
    New(Mutation),
    Existing(OperationPhase),
}

fn start_operation(
    shared: &Arc<Shared>,
    id: &OperationIdV1,
    input: OperationInput,
    object: Option<ObjectKeyV1>,
) -> core::result::Result<Start, ErrorKind> {
    let initialization = shared.connection.initialization();
    if id.sequence == 0 || (id.actor != 0 && id.actor != initialization.module_epoch) {
        return Err(ErrorKind::InvalidInput);
    }
    let mut state = shared.state.lock();
    match operation_index(&state, id) {
        Ok(index) => {
            let operation = &mut state.operations[index].1;
            if !same_input(&operation.input, &input) {
                return Err(ErrorKind::Conflict);
            }
            match &operation.phase {
                OperationPhase::Running => return Err(ErrorKind::Busy),
                OperationPhase::Pending => operation.phase = OperationPhase::Running,
                phase => return Ok(Start::Existing(copy_phase(phase))),
            }
        }
        Err(index) => {
            if matches!(
                state.phase,
                SourcePhase::Registering | SourcePhase::Retiring | SourcePhase::Retired
            ) {
                return Err(ErrorKind::InvalidState);
            }
            state
                .operations
                .try_reserve(1)
                .map_err(|_| ErrorKind::ResourceExhausted)?;
            state.operations.insert(
                index,
                (
                    copy_operation(id),
                    Operation {
                        input,
                        phase: OperationPhase::Running,
                    },
                ),
            );
        }
    }
    Ok(Start::New(Mutation {
        shared: Arc::clone(shared),
        operation: Some(copy_operation(id)),
        object,
        resolved: false,
    }))
}

fn complete_operation(mut mutation: Mutation, phase: OperationPhase) {
    let id = mutation
        .operation
        .as_ref()
        .expect("recorded operation must have an identity");
    {
        let mut state = mutation.shared.state.lock();
        let index = operation_index(&state, id).expect("current operation must remain recorded");
        state.operations[index].1.phase = phase;
    }
    mutation.resolved = true;
}

fn reject_operation(mutation: Mutation, error: Error, pending_on_busy: bool) -> OutcomeV1 {
    let shared = Arc::clone(&mutation.shared);
    let report = if pending_on_busy && *error.current_context() == ErrorKind::Busy {
        match discard_busy_report(&shared, error) {
            Some(cleanup) => cleanup_error(cleanup),
            None => {
                complete_operation(mutation, OperationPhase::Pending);
                // 保留同次退役请求和下一次检查资格，Busy 无须创建来源诊断持有（diagnostic holding）。
                return diagnostic::limited_outcome(ErrorKind::Busy, ErrorKind::InvalidState);
            }
        }
    } else {
        error
    };
    let (cached, _) = finish_source_failure(&shared, report);
    complete_operation(mutation, OperationPhase::Failed(copy_failure(&cached)));
    publish_failure(&shared, &cached)
}

fn operation_index(state: &State, operation: &OperationIdV1) -> core::result::Result<usize, usize> {
    state
        .operations
        .binary_search_by_key(&(operation.actor, operation.sequence), |(id, _)| {
            (id.actor, id.sequence)
        })
}

fn same_input(left: &OperationInput, right: &OperationInput) -> bool {
    match (left, right) {
        (OperationInput::Maintain, OperationInput::Maintain)
        | (OperationInput::Retire, OperationInput::Retire) => true,
        (OperationInput::Retain(left), OperationInput::Retain(right))
        | (OperationInput::Release(left), OperationInput::Release(right)) => {
            left.object.source == right.object.source
                && left.object.slot == right.object.slot
                && left.object.generation == right.object.generation
                && left.initialization.module_epoch == right.initialization.module_epoch
                && left.initialization.round == right.initialization.round
                && left.holding == right.holding
        }
        _ => false,
    }
}

fn copy_phase(phase: &OperationPhase) -> OperationPhase {
    match phase {
        OperationPhase::Running => OperationPhase::Running,
        OperationPhase::Pending => OperationPhase::Pending,
        OperationPhase::Retained(lease) => OperationPhase::Retained(copy_lease(lease)),
        OperationPhase::Released => OperationPhase::Released,
        OperationPhase::Maintained(snapshot) => OperationPhase::Maintained(*snapshot),
        OperationPhase::Retired => OperationPhase::Retired,
        OperationPhase::Failed(failure) => OperationPhase::Failed(copy_failure(failure)),
    }
}

fn copy_failure(failure: &CachedFailure) -> CachedFailure {
    match failure {
        CachedFailure::Complete(snapshot) => CachedFailure::Complete(Arc::clone(snapshot)),
        CachedFailure::Unavailable {
            operation,
            diagnostic,
        } => CachedFailure::Unavailable {
            operation: *operation,
            diagnostic: *diagnostic,
        },
    }
}

fn retirement_check(shared: &Shared) -> Result<()> {
    let failure = {
        let mut state = shared.state.lock();
        if matches!(state.phase, SourcePhase::Open) {
            state.phase = SourcePhase::Closing;
        }
        if let Some(failure) = recorded_failure(&state) {
            Some(failure)
        } else {
            let ready = matches!(state.phase, SourcePhase::Closing | SourcePhase::Retiring)
                && state
                    .entries
                    .iter()
                    .all(|entry| matches!(entry.phase, EntryPhase::Retired));
            if ready {
                state.phase = SourcePhase::Retiring;
                return Ok(());
            }
            None
        }
    };
    Err(failure.map_or_else(
        || error(ErrorKind::Busy, "source retirement check"),
        cleanup_error,
    ))
}

fn remaining_or_failure(shared: &Shared) -> Result<u64> {
    let result = {
        let state = shared.state.lock();
        match recorded_failure(&state) {
            Some(failure) => Err(failure),
            None => Ok(state
                .entries
                .iter()
                .filter(|entry| !matches!(entry.phase, EntryPhase::Live | EntryPhase::Retired))
                .count() as u64),
        }
    };
    result.map_err(cleanup_error)
}

fn recorded_failure(state: &State) -> Option<Failure> {
    state
        .failure
        .as_ref()
        .or_else(|| {
            state
                .entries
                .iter()
                .find_map(|entry| entry.failure.as_ref())
        })
        .map(copy_recorded_failure)
}

fn copy_recorded_failure(failure: &Failure) -> Failure {
    Failure {
        kind: failure.kind,
        secondary: failure.secondary,
        report: failure.report.as_ref().map(Arc::clone),
    }
}

fn cleanup_error(recorded: Failure) -> Error {
    let mut failure = error(recorded.kind, "source cleanup");
    if let Some(report) = recorded.report {
        if let Some(snapshot) = diagnostic::copied_diagnostic(&report) {
            failure = failure.attach(snapshot.to_string());
        }
        failure = failure.attach(report);
    }
    if let Some(kind) = recorded.secondary {
        // 将原分类作为独立报告附加，避免 pi_result 将同类型的附加数据当作主分类。
        failure = failure.attach(error(kind, "related source failure"));
    }
    failure
}

fn restore_cleanup(attempt: &mut CleanupAttempt, phase: EntryPhase, failure: Option<Failure>) {
    {
        let mut state = attempt.shared.state.lock();
        let entry =
            entry_by_key(&mut state, &attempt.key).expect("cleanup entry must remain recorded");
        entry.value = Some(Arc::clone(&attempt.value));
        entry.close = attempt.close.take();
        entry.phase = phase;
        entry.failure = failure;
    }
    attempt.resolved = true;
}

fn unknown_failure() -> Failure {
    Failure {
        kind: ErrorKind::Internal,
        secondary: Some(ErrorKind::InvalidState),
        report: None,
    }
}

fn mark_entry_failed(entry: &mut Entry) {
    if !matches!(entry.phase, EntryPhase::Retired) {
        entry.phase = EntryPhase::Failed;
        entry.failure.get_or_insert_with(unknown_failure);
    }
}

fn raw_object(shared: &Shared, lease: LeaseV1) -> ObjectRefV1 {
    ObjectRefV1 {
        context: source_address(shared),
        api: &SOURCE_API,
        lease,
    }
}

fn target(shared: &Shared, lease: LeaseV1, kind: u32) -> CallTargetV1 {
    let api = if kind == TARGET_DIAGNOSTIC {
        std::ptr::from_ref(&DIAGNOSTIC_API).cast()
    } else {
        std::ptr::from_ref(&SOURCE_API).cast()
    };
    CallTargetV1 {
        context: source_address(shared),
        api,
        lease,
        kind,
    }
}

fn source_address(shared: &Shared) -> *mut c_void {
    shared
        .context
        .get()
        .expect("registered source must have its stable address")
        .pointer
        .cast()
}

unsafe fn registry_from_context(context: *mut c_void) -> core::result::Result<Registry, ErrorKind> {
    let pointer = context.cast::<SourceContext>();
    if !writable(pointer) {
        return Err(ErrorKind::InvalidInput);
    }
    // SAFETY: 调用前已取得来源的真实代码与上下文保护，地址来自该来源的登记。
    let source = unsafe { &*pointer };
    Ok(Registry {
        shared: Arc::clone(&source.shared),
    })
}

fn check_source(shared: &Shared, source: u64) -> Result<()> {
    if source == 0 || shared.state.lock().source != source {
        Err(error(ErrorKind::Conflict, "source identity"))
    } else {
        Ok(())
    }
}

fn writable<T>(pointer: *mut T) -> bool {
    !pointer.is_null() && pointer.is_aligned()
}

fn cleanup_context(attempt: &CleanupAttempt, operation: &'static str) -> boundary::Context {
    let id = attempt.shared.connection.initialization();
    boundary::Context {
        operation,
        module_epoch: id.module_epoch,
        round: id.round,
        source: attempt.key.source,
    }
}

fn source_context(operation: &'static str, lease: &LeaseV1) -> boundary::Context {
    boundary::Context {
        operation,
        module_epoch: lease.initialization.module_epoch,
        round: lease.initialization.round,
        source: lease.object.source,
    }
}

fn error(kind: ErrorKind, operation: &'static str) -> Error {
    Error::new(kind).attach(pi_result::attachment::Operation(operation))
}

fn copy_key(key: &ObjectKeyV1) -> ObjectKeyV1 {
    ObjectKeyV1 {
        source: key.source,
        slot: key.slot,
        generation: key.generation,
    }
}

fn copy_round(round: &InitIdV1) -> InitIdV1 {
    InitIdV1 {
        module_epoch: round.module_epoch,
        round: round.round,
    }
}

fn copy_lease(lease: &LeaseV1) -> LeaseV1 {
    LeaseV1 {
        object: copy_key(&lease.object),
        initialization: copy_round(&lease.initialization),
        holding: lease.holding,
    }
}

fn copy_operation(id: &OperationIdV1) -> OperationIdV1 {
    OperationIdV1 {
        actor: id.actor,
        sequence: id.sequence,
    }
}
