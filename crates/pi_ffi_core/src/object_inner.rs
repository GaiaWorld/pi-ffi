// 本地实现入口（local implementation entry）；关键类型由父模块声明。
use super::*;
use crate::{boundary, diagnostic};
use pi_ffi_abi::{
    ApiHeaderV1, CallTargetV1, InterfaceViewV1, ObjectKeyV1, SourceApiV1, TARGET_OBJECT,
};
use pi_result::{Error, ErrorKind};

pub(super) unsafe fn object_from_transfer(
    connection: Connection,
    raw: ObjectRefV1,
) -> Result<Object> {
    let owner = ObjectOwner {
        connection,
        raw,
        state: Mutex::new(OwnerState::Live),
    };
    // 先由本地拥有者（local owner）承担释放义务（release obligation），任一后续失败都走其析构。
    let call = enter_owner(&owner, 1)?;
    call.finish()?;
    Ok(Object {
        owner: Arc::new(owner),
    })
}

pub(super) fn object_initialization(receiver: &Object) -> InitIdV1 {
    copy_round(&receiver.owner.raw.lease.initialization)
}

pub(super) fn object_with_borrowed<R>(
    receiver: &Object,
    body: impl for<'call> FnOnce(&'call ObjectRefV1) -> Result<R>,
) -> Result<R> {
    let call = enter_owner(&receiver.owner, 1)?;
    let result = body(&receiver.owner.raw);
    finish(result, call)
}

pub(super) unsafe fn object_retain_borrowed(
    connection: &Connection,
    input: &ObjectRefV1,
) -> Result<Object> {
    // SAFETY: 本调用的外层初始化保护（initialization protection）覆盖该输入和来源诊断表。
    unsafe { connection.check_peer(copy_raw(input))? };
    let call = connection.enter(target(input), connection.next_operation()?, 1)?;
    let retained = connection.next_operation().and_then(|operation| {
        // SAFETY: 本次调用已登记，且原调用方继续持有输入。
        let transfer = unsafe { retain(connection, input, operation)? };
        // SAFETY: retain 成功已唯一接管新的、经过核实的独立持有（independent holding）。
        unsafe { Object::from_transfer(connection.clone(), transfer.into_raw()) }
    });
    finish(retained, call)
}

pub(super) fn object_try_retain(receiver: &Object) -> Result<Object> {
    let connection = &receiver.owner.connection;
    let call = enter_owner(&receiver.owner, 1)?;
    let retained = connection.next_operation().and_then(|operation| {
        // SAFETY: 活动来源调用（source call）及原拥有者保护基础操作表。
        let transfer = unsafe { retain(connection, &receiver.owner.raw, operation)? };
        // SAFETY: 移交包装（transfer wrapper）只交出这一份独立持有。
        unsafe { Object::from_transfer(connection.clone(), transfer.into_raw()) }
    });
    finish(retained, call)
}

pub(super) fn object_retain_for_transfer(receiver: &Object) -> Result<TransferObject> {
    let connection = &receiver.owner.connection;
    let call = enter_owner(&receiver.owner, 1)?;
    let retained = connection.next_operation().and_then(|operation| {
        // SAFETY: 来源操作表由本次调用保活（call protection）及原拥有者共同保护。
        unsafe { retain(connection, &receiver.owner.raw, operation) }
    });
    finish(retained, call)
}

pub(super) fn object_release(receiver: Object) -> Result<()> {
    let Ok(owner) = Arc::try_unwrap(receiver.owner) else {
        return Ok(());
    };
    let status = owner.submit_release();
    if status == 0 {
        Ok(())
    } else {
        Err(operation_error(boundary::decode_kind(status), "object release").attach(status))
    }
}

pub(super) fn object_interface<I: AbiInterface>(receiver: &Object) -> Result<Interface<I>> {
    let size = u32::try_from(size_of::<I::Table>())
        .map_err(|_| operation_error(ErrorKind::InvalidInput, "interface size"))?;
    if I::ID.bytes == [0; 16]
        || I::MAJOR == 0
        || (size as usize) < size_of::<ApiHeaderV1>()
        || I::SUPPORTED_RIGHTS & !7 != 0
    {
        return Err(operation_error(
            ErrorKind::InvalidInput,
            "interface declaration",
        ));
    }
    let call = enter_owner(&receiver.owner, 1)?;
    let result = (|| {
        let raw = &receiver.owner.raw;
        // SAFETY: 当前来源表（source table）已由宿主活动调用保护。
        let api = unsafe { source_table(raw)? };
        let query = api
            .query_interface
            .ok_or_else(|| operation_error(ErrorKind::Corrupted, "query interface"))?;
        let request = InterfaceRequestV1 {
            interface_id: I::ID,
            major: I::MAJOR,
            size,
        };
        let mut view = InterfaceViewV1 {
            context: std::ptr::null_mut(),
            table: std::ptr::null(),
        };
        // SAFETY: 持有仍有效。输出槽（output slot）由调用方独占，仅成功时读取。
        let outcome = unsafe { query(raw.context, copy_lease(&raw.lease), request, &mut view) };
        decode_source(&receiver.owner.connection, raw, outcome)?;
        let pointer = view.table.cast::<I::Table>();
        if view.context.is_null() || pointer.is_null() || !pointer.is_aligned() {
            return Err(operation_error(
                ErrorKind::Corrupted,
                "queried interface address",
            ));
        }
        // SAFETY: 来源成功返回固定首部（API header），整个借用在当前保护内。
        let header = unsafe { &*pointer.cast::<ApiHeaderV1>() };
        if header.major != I::MAJOR || header.size != size {
            return Err(operation_error(
                ErrorKind::Corrupted,
                "queried interface header",
            ));
        }

        Ok(Interface {
            object: receiver.clone(),
            address: TableAddress {
                context: view.context,
                pointer,
                marker: PhantomData,
            },
        })
    })();
    finish(result, call)
}

pub(super) fn object_clone(receiver: &Object) -> Object {
    Object {
        owner: Arc::clone(&receiver.owner),
    }
}

pub(super) fn object_owner_submit_release(receiver: &ObjectOwner) -> u32 {
    {
        let mut state = receiver.state.lock();
        match *state {
            OwnerState::ReleaseSubmitted => return 0,
            OwnerState::Quarantined => return boundary::encode_kind(ErrorKind::InvalidState),
            // 领取提交资格后停止该拥有者取得新调用。
            OwnerState::Live => *state = OwnerState::Quarantined,
        }
    }
    let status = receiver.connection.submit_release(target(&receiver.raw));
    if status == 0 {
        *receiver.state.lock() = OwnerState::ReleaseSubmitted;
    }
    status
}

pub(super) fn object_owner_drop(receiver: &mut ObjectOwner) {
    receiver.submit_release();
}

pub(super) fn transfer_object_into_raw(receiver: TransferObject) -> ObjectRefV1 {
    let mut receiver = receiver;
    receiver.transferred = true;
    copy_raw(&receiver.raw)
}

pub(super) fn transfer_object_drop(receiver: &mut TransferObject) {
    if !receiver.transferred {
        receiver.connection.submit_release(target(&receiver.raw));
    }
}

pub(super) fn interface_with_call<I: AbiInterface, R>(
    receiver: &Interface<I>,
    rights: u64,
    body: impl for<'call> FnOnce(InterfaceCall<'call, I>) -> Result<R>,
) -> Result<R> {
    if rights == 0 || rights & !(7 & I::SUPPORTED_RIGHTS) != 0 {
        return Err(operation_error(
            ErrorKind::PermissionDenied,
            "interface call rights",
        ));
    }
    let owner = &receiver.object.owner;
    let call = enter_owner(owner, rights)?;
    let result = body(InterfaceCall {
        // SAFETY: 查询时已核准确类型（type）和表头。本次来源调用保活覆盖借用。
        table: unsafe { &*receiver.address.pointer },
        context: receiver.address.context,
        lease: copy_lease(&owner.raw.lease),
        connection: &owner.connection,
    });
    finish(result, call)
}

pub(super) fn interface_call_lease<'call, I: AbiInterface>(
    receiver: &InterfaceCall<'call, I>,
) -> LeaseV1 {
    copy_lease(&receiver.lease)
}

pub(super) fn interface_call_connection<'call, I: AbiInterface>(
    receiver: &InterfaceCall<'call, I>,
) -> &'call Connection {
    receiver.connection
}

pub(super) unsafe fn interface_call_decode<'call, I: AbiInterface>(
    receiver: &InterfaceCall<'call, I>,
    outcome: OutcomeV1,
) -> Result<()> {
    if outcome.kind != 0
        && outcome.diagnostic_failure_kind == 0
        && !outcome.diagnostic.context.is_null()
        && outcome.diagnostic.lease.object.source != receiver.lease.object.source
    {
        return Err(
            operation_error(ErrorKind::Corrupted, "interface diagnostic source")
                .attach(outcome.kind),
        );
    }
    diagnostic::decode(receiver.connection, outcome)
}

pub(super) fn interface_clone<I: AbiInterface>(receiver: &Interface<I>) -> Interface<I> {
    Interface {
        object: receiver.object.clone(),
        address: TableAddress {
            context: receiver.address.context,
            pointer: receiver.address.pointer,
            marker: PhantomData,
        },
    }
}

fn enter_owner(owner: &ObjectOwner, rights: u64) -> Result<crate::control::Call> {
    if !matches!(*owner.state.lock(), OwnerState::Live) {
        return Err(operation_error(ErrorKind::InvalidState, "object call"));
    }
    let operation = owner.connection.next_operation()?;
    owner
        .connection
        .enter(target(&owner.raw), operation, rights)
}

fn finish<R>(operation: Result<R>, call: crate::control::Call) -> Result<R> {
    match (operation, call.finish()) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(primary), Err(secondary)) => Err(primary.attach(secondary)),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

unsafe fn source_table(raw: &ObjectRefV1) -> Result<&SourceApiV1> {
    let invalid = || operation_error(ErrorKind::Corrupted, "object source table");
    if raw.api.is_null() || !raw.api.is_aligned() {
        return Err(invalid());
    }
    // SAFETY: 调用者拥有实际来源代码保活，先读取固定表头（API header）。
    let header = unsafe { &*raw.api.cast::<ApiHeaderV1>() };
    if header.major != 1 || header.size as usize != size_of::<SourceApiV1>() {
        return Err(invalid());
    }
    // SAFETY: 已核版本、大小及地址，来源保证整个只读表有效。
    let api = unsafe { &*raw.api };
    if api.validate.is_none()
        || api.retain.is_none()
        || api.release.is_none()
        || api.query_interface.is_none()
    {
        return Err(invalid());
    }
    Ok(api)
}

unsafe fn retain(
    connection: &Connection,
    input: &ObjectRefV1,
    operation: pi_ffi_abi::OperationIdV1,
) -> Result<TransferObject> {
    // SAFETY: 调用者先取得原持有（original holding）的实际代码保活。
    let api = unsafe { source_table(input)? };
    let retain = api
        .retain
        .ok_or_else(|| operation_error(ErrorKind::Corrupted, "retain"))?;
    let mut raw = ObjectRefV1 {
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
    };
    // SAFETY: 已持有同一来源表及原持有。输出槽（output slot）是本地独占可写内存。
    let outcome = unsafe { retain(input.context, copy_lease(&input.lease), operation, &mut raw) };
    decode_source(connection, input, outcome)?;
    if raw.context != input.context
        || raw.api != input.api
        || raw.lease.object.source != input.lease.object.source
        || raw.lease.object.slot != input.lease.object.slot
        || raw.lease.object.generation != input.lease.object.generation
        || raw.lease.initialization.module_epoch != input.lease.initialization.module_epoch
        || raw.lease.initialization.round != input.lease.initialization.round
        || raw.lease.holding == 0
        || raw.lease.holding == input.lease.holding
    {
        return Err(operation_error(
            ErrorKind::Corrupted,
            "retained holding identity",
        ));
    }
    Ok(TransferObject {
        connection: connection.clone(),
        raw,
        transferred: false,
    })
}

fn decode_source(connection: &Connection, raw: &ObjectRefV1, outcome: OutcomeV1) -> Result<()> {
    let diagnostic = &outcome.diagnostic;
    if outcome.kind != 0
        && outcome.diagnostic_failure_kind == 0
        && !diagnostic.context.is_null()
        && (diagnostic.context != raw.context
            || diagnostic.lease.object.source != raw.lease.object.source
            || diagnostic.lease.initialization.module_epoch
                != raw.lease.initialization.module_epoch
            || diagnostic.lease.initialization.round != raw.lease.initialization.round)
    {
        return Err(
            operation_error(ErrorKind::Corrupted, "object diagnostic source").attach(outcome.kind),
        );
    }
    diagnostic::decode(connection, outcome)
}

fn operation_error(kind: ErrorKind, operation: &'static str) -> Error {
    Error::new(kind).attach(pi_result::attachment::Operation(operation))
}

fn target(raw: &ObjectRefV1) -> CallTargetV1 {
    CallTargetV1 {
        context: raw.context,
        api: raw.api.cast(),
        lease: copy_lease(&raw.lease),
        kind: TARGET_OBJECT,
    }
}

fn copy_raw(raw: &ObjectRefV1) -> ObjectRefV1 {
    ObjectRefV1 {
        context: raw.context,
        api: raw.api,
        lease: copy_lease(&raw.lease),
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
        object: ObjectKeyV1 {
            source: lease.object.source,
            slot: lease.object.slot,
            generation: lease.object.generation,
        },
        initialization: copy_round(&lease.initialization),
        holding: lease.holding,
    }
}
