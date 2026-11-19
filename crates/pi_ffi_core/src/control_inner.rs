// 本地实现入口（local implementation entry）；关键类型由父模块声明。
use super::*;
use crate::boundary;
use pi_ffi_abi::{
    ApiHeaderV1, DiagnosticRefV1, InterfaceRequestV1, InterfaceViewV1, LeaseV1, ObjectKeyV1,
    SourceApiV1, HOST_CONTROL_ID, HOST_CONTROL_MAJOR, TARGET_DIAGNOSTIC, TARGET_OBJECT,
};
use pi_result::{Error, ErrorKind};
use std::mem::ManuallyDrop;

pub(super) unsafe fn connection_from_native(
    management: InterfaceViewV1,
    forwarded: InterfaceViewV1,
    initialization: InitIdV1,
    lifetime: Arc<dyn Send + Sync>,
) -> Result<Connection> {
    if initialization.module_epoch == 0 || initialization.round == 0 {
        return Err(Error::new(ErrorKind::InvalidInput)
            .attach("初始化标识（initialization identifier）无效"));
    }
    // SAFETY: lifetime 保持两项实际管理视图（management view）的代码和上下文（context）。
    let management = unsafe { management_address(management)? };
    // SAFETY: lifetime 同时覆盖面向插件的受限视图（restricted view）。
    let forwarded_management = unsafe { management_address(forwarded)? };
    if management.context == forwarded_management.context
        && management.table == forwarded_management.table
    {
        return Err(Error::new(ErrorKind::Conflict)
            .attach("宿主与插件的管理能力（management capability）相同"));
    }
    Ok(Connection {
        binding: Arc::new(Binding {
            management,
            forwarded_management,
            initialization,
            protection: Protection::Native(lifetime),
            state: Mutex::new(BindingState::Live),
        }),
    })
}

pub(super) unsafe fn connection_from_borrowed(seed: ObjectRefV1) -> Result<Connection> {
    validate_seed(&seed)?;
    // SAFETY: 引导（bootstrap）调用方的外层保护覆盖原输入及其诊断表（diagnostic table）。
    let (management, api) = unsafe { query_management(&seed)? };
    let operation = next_operation(&management, seed.lease.initialization.module_epoch)?;
    let mut retained = ObjectRefV1 {
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
    let retain = api
        .retain
        .ok_or_else(|| operation_error(ErrorKind::Corrupted, "bootstrap retain"))?;
    // SAFETY: 来源存在于同一外层调用保护（call protection），输出槽（output slot）由本地独占。
    let outcome = unsafe {
        retain(
            seed.context,
            copy_lease(&seed.lease),
            operation,
            &mut retained,
        )
    };
    // SAFETY: 原来源和实际诊断表在该外层保护内，返回结果只能接管一次。
    unsafe { boundary::decode_bootstrap(&seed, outcome)? };
    if seed.context != retained.context
        || seed.api != retained.api
        || seed.lease.object.source != retained.lease.object.source
        || seed.lease.object.slot != retained.lease.object.slot
        || seed.lease.object.generation != retained.lease.object.generation
        || !same_round(&seed.lease.initialization, &retained.lease.initialization)
        || retained.lease.holding == 0
        || seed.lease.holding == retained.lease.holding
    {
        return Err(operation_error(
            ErrorKind::Corrupted,
            "bootstrap retained object",
        ));
    }

    let forwarded_management = ManagementAddress {
        context: management.context,
        table: management.table,
    };
    let target = object_target(&retained);
    Ok(Connection {
        binding: Arc::new(Binding {
            management,
            forwarded_management,
            initialization: retained.lease.initialization,
            protection: Protection::Input(InputHolding { target }),
            state: Mutex::new(BindingState::Live),
        }),
    })
}

pub(super) unsafe fn connection_check_peer(receiver: &Connection, peer: ObjectRefV1) -> Result<()> {
    validate_seed(&peer)?;
    if !same_round(&receiver.binding.initialization, &peer.lease.initialization) {
        return Err(operation_error(ErrorKind::Conflict, "check peer round"));
    }
    let operation = receiver.next_operation()?;
    let call = receiver.enter(object_target(&peer), operation, 1)?;
    // SAFETY: 外层借用及本次真实调用同时保护该来源，查询后才比较管理视图（management view）。
    let checked = unsafe { query_management(&peer) }.and_then(|(view, _)| {
        let expected = &receiver.binding.forwarded_management;
        if view.context == expected.context && view.table == expected.table {
            Ok(())
        } else {
            Err(operation_error(
                ErrorKind::Conflict,
                "check peer management view",
            ))
        }
    });

    let finished = call.finish();
    match (checked, finished) {
        (Err(primary), Err(secondary)) => Err(primary.attach(secondary)),
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

pub(super) fn connection_initialization(receiver: &Connection) -> InitIdV1 {
    let initialization = &receiver.binding.initialization;
    InitIdV1 {
        module_epoch: initialization.module_epoch,
        round: initialization.round,
    }
}

pub(super) fn connection_next_operation(receiver: &Connection) -> Result<OperationIdV1> {
    live_binding(&receiver.binding, "next operation")?;
    next_operation(
        &receiver.binding.management,
        receiver.binding.initialization.module_epoch,
    )
}

pub(super) fn connection_enter(
    receiver: &Connection,
    target: CallTargetV1,
    operation: OperationIdV1,
    rights: u64,
) -> Result<Call> {
    live_binding(&receiver.binding, "enter call")?;
    check_target(&receiver.binding, &target, "enter call")?;
    if rights & !7 != 0 || operation.sequence == 0 {
        return Err(operation_error(ErrorKind::InvalidInput, "enter call"));
    }
    let source = target.lease.object.source;
    let address = &receiver.binding.management;
    let mut token = CallTokenV1 { source: 0, call: 0 };
    // SAFETY: 当前连接保护管理表（management table）。宿主先核完整登记，
    // 传出的来源地址在此处不解引用。
    let status = unsafe {
        let enter = (*address.table)
            .enter_call
            .ok_or_else(|| operation_error(ErrorKind::Corrupted, "enter call"))?;
        enter(address.context, target, operation, rights, &mut token)
    };
    status_result(status, "enter call")?;
    if token.source != source || token.call == 0 {
        return Err(operation_error(ErrorKind::Corrupted, "enter call token").attach(token.call));
    }
    Ok(Call {
        binding: Arc::clone(&receiver.binding),
        token,
        ended: false,
    })
}

pub(super) fn connection_track(receiver: &Connection, target: CallTargetV1) -> Result<()> {
    live_binding(&receiver.binding, "track holding")?;
    check_target(&receiver.binding, &target, "track holding")?;
    let address = &receiver.binding.management;
    // SAFETY: 当前连接保护管理表（management table）。只登记传输字段，由宿主管理回调核对。
    let status = unsafe {
        let track = (*address.table)
            .track_holding
            .ok_or_else(|| operation_error(ErrorKind::Corrupted, "track holding"))?;
        track(address.context, target)
    };
    status_result(status, "track holding")
}

pub(super) fn connection_submit_release(receiver: &Connection, target: CallTargetV1) -> u32 {
    if !matches!(*receiver.binding.state.lock(), BindingState::Live) {
        return boundary::encode_kind(ErrorKind::InvalidState);
    }
    if !same_round(
        &receiver.binding.initialization,
        &target.lease.initialization,
    ) {
        return boundary::encode_kind(ErrorKind::Conflict);
    }
    submit_release(&receiver.binding.management, target)
}

pub(super) fn connection_register_raw(
    receiver: &Connection,
    operation: OperationIdV1,
    descriptor: SourceDescriptorV1,
    out: &mut u64,
) -> OutcomeV1 {
    if !matches!(*receiver.binding.state.lock(), BindingState::Live) {
        return limited_outcome(ErrorKind::InvalidState);
    }
    let address = &receiver.binding.management;
    let mut registered = 0;
    // SAFETY: 当前连接持有管理表。descriptor 由准备阶段的真实来源代码及上下文保护。
    let outcome = unsafe {
        match (*address.table).register_source {
            Some(register) => register(address.context, operation, descriptor, &mut registered),
            None => return limited_outcome(ErrorKind::Corrupted),
        }
    };
    if outcome.kind == 0
        && outcome.diagnostic_failure_kind == 0
        && diagnostic_is_empty(&outcome.diagnostic)
    {
        *out = registered;
    }
    outcome
}

pub(super) fn connection_retire_raw(
    receiver: &Connection,
    source: u64,
    operation: OperationIdV1,
) -> OutcomeV1 {
    if !matches!(*receiver.binding.state.lock(), BindingState::Live) {
        return limited_outcome(ErrorKind::InvalidState);
    }
    let address = &receiver.binding.management;
    // SAFETY: 当前连接保持管理表及管理上下文，由宿主核对原来源登记和权威（authority）。
    unsafe {
        match (*address.table).retire_source {
            Some(retire) => retire(address.context, source, operation),
            None => limited_outcome(ErrorKind::Corrupted),
        }
    }
}

pub(super) fn connection_clone(receiver: &Connection) -> Connection {
    Connection {
        binding: Arc::clone(&receiver.binding),
    }
}

pub(super) fn binding_drop(receiver: &mut Binding) {
    match &receiver.protection {
        Protection::Input(holding) => {
            let status = submit_release(&receiver.management, copy_target(&holding.target));
            *receiver.state.get_mut() = if status == 0 {
                BindingState::ReleaseSubmitted
            } else {
                BindingState::Quarantined
            };
        }
        Protection::Native(lifetime) => {
            // 字段析构（field drop）在这里返回后结束本地保活。
            let _ = lifetime;
        }
    }
}

pub(super) fn call_finish(mut receiver: Call) -> Result<()> {
    let status = leave_call(&receiver);
    if status == 0 {
        receiver.ended = true;
        return Ok(());
    }

    // 显式归还（release）已提交。让宿主延续不确定的保活责任，避免析构路径再次调用。
    let call = ManuallyDrop::new(receiver);
    // SAFETY: Call 余下字段仅为传输数据，唯一 Arc（shared owner）在此移出一次。
    drop(unsafe { std::ptr::read(&call.binding) });
    status_result(status, "leave call")
}

pub(super) fn call_drop(receiver: &mut Call) {
    if !receiver.ended {
        receiver.ended = leave_call(receiver) == 0;
    }
}

fn leave_call(call: &Call) -> u32 {
    let address = &call.binding.management;
    // SAFETY: Call 自身持有连接，管理表在归还保活（release call protection）返回前有效。
    unsafe {
        match (*address.table).leave_call {
            Some(leave) => leave(address.context, call.token),
            None => boundary::encode_kind(ErrorKind::Corrupted),
        }
    }
}

fn submit_release(address: &ManagementAddress, target: CallTargetV1) -> u32 {
    // SAFETY: 调用者持有 Binding（connection binding），或正在销毁其中仍有效的输入持有。
    unsafe {
        match (*address.table).request_release {
            Some(release) => release(address.context, target),
            None => boundary::encode_kind(ErrorKind::Corrupted),
        }
    }
}

fn next_operation(address: &ManagementAddress, module_epoch: u64) -> Result<OperationIdV1> {
    let mut operation = OperationIdV1 {
        actor: 0,
        sequence: 0,
    };
    // SAFETY: 管理视图受当前连接或引导输入（bootstrap input）的外层保活保护。
    let status = unsafe {
        let next = (*address.table)
            .next_operation
            .ok_or_else(|| operation_error(ErrorKind::Corrupted, "next operation"))?;
        next(address.context, &mut operation)
    };
    status_result(status, "next operation")?;
    if operation.sequence == 0 || (operation.actor != 0 && operation.actor != module_epoch) {
        return Err(
            operation_error(ErrorKind::Corrupted, "next operation identity")
                .attach(operation.actor)
                .attach(operation.sequence),
        );
    }
    Ok(operation)
}

unsafe fn query_management(seed: &ObjectRefV1) -> Result<(ManagementAddress, &SourceApiV1)> {
    let invalid = || operation_error(ErrorKind::Corrupted, "bootstrap source table");
    if seed.api.is_null() || !seed.api.is_aligned() {
        return Err(invalid());
    }
    // SAFETY: 真实来源已受保护，先检查固定表头（API header）才借用完整表。
    let header = unsafe { &*seed.api.cast::<ApiHeaderV1>() };
    if header.major != 1 || header.size as usize != size_of::<SourceApiV1>() {
        return Err(invalid());
    }
    // SAFETY: 前置条件确保此地址是完整已匹配的只读来源表（source table）。
    let source = unsafe { &*seed.api };
    let query = source.query_interface.ok_or_else(invalid)?;
    if source.validate.is_none() || source.retain.is_none() || source.release.is_none() {
        return Err(invalid());
    }
    let mut view = InterfaceViewV1 {
        context: std::ptr::null_mut(),
        table: std::ptr::null(),
    };
    let request = InterfaceRequestV1 {
        interface_id: HOST_CONTROL_ID,
        major: HOST_CONTROL_MAJOR,
        size: size_of::<HostControlApiV1>() as u32,
    };
    // SAFETY: 原持有及来源代码在对应调用保护内；独占输出槽在调用后仅按成功读取。
    let outcome = unsafe { query(seed.context, copy_lease(&seed.lease), request, &mut view) };
    // SAFETY: 来源诊断（source diagnostic）在同一外层保护内唯一接管。
    unsafe { boundary::decode_bootstrap(seed, outcome)? };
    // SAFETY: 查询已确认来自受控来源，借出的管理表随原持有受保护。
    let address = unsafe { management_address(view)? };
    Ok((address, source))
}

unsafe fn management_address(view: InterfaceViewV1) -> Result<ManagementAddress> {
    let invalid = || operation_error(ErrorKind::Corrupted, "management view");
    let table = view.table.cast::<HostControlApiV1>();
    if view.context.is_null() || table.is_null() || !table.is_aligned() {
        return Err(invalid());
    }
    // SAFETY: 外层保护提供了合法的视图及固定表头（API header）。
    let header = unsafe { &*table.cast::<ApiHeaderV1>() };
    if header.major != HOST_CONTROL_MAJOR || header.size as usize != size_of::<HostControlApiV1>() {
        return Err(invalid());
    }
    // SAFETY: 在匹配版本、尺寸及保活后，借用固定管理表（management table）。
    let entries = unsafe { &*table };
    if entries.next_operation.is_none()
        || entries.register_source.is_none()
        || entries.track_holding.is_none()
        || entries.enter_call.is_none()
        || entries.leave_call.is_none()
        || entries.request_release.is_none()
        || entries.retire_source.is_none()
    {
        return Err(invalid());
    }
    Ok(ManagementAddress {
        context: view.context,
        table,
    })
}

fn validate_seed(seed: &ObjectRefV1) -> Result<()> {
    if seed.context.is_null()
        || seed.api.is_null()
        || seed.lease.object.source == 0
        || seed.lease.object.slot == 0
        || seed.lease.object.generation == 0
        || seed.lease.initialization.module_epoch == 0
        || seed.lease.initialization.round == 0
        || seed.lease.holding == 0
    {
        return Err(operation_error(ErrorKind::InvalidInput, "bootstrap input"));
    }
    Ok(())
}

fn check_target(binding: &Binding, target: &CallTargetV1, operation: &'static str) -> Result<()> {
    if !same_round(&binding.initialization, &target.lease.initialization) {
        return Err(operation_error(ErrorKind::Conflict, operation));
    }
    if target.context.is_null()
        || target.api.is_null()
        || target.lease.object.source == 0
        || target.lease.object.slot == 0
        || target.lease.object.generation == 0
        || target.lease.holding == 0
        || !matches!(target.kind, TARGET_OBJECT | TARGET_DIAGNOSTIC)
    {
        return Err(operation_error(ErrorKind::InvalidInput, operation));
    }
    Ok(())
}

fn live_binding(binding: &Binding, operation: &'static str) -> Result<()> {
    if matches!(*binding.state.lock(), BindingState::Live) {
        Ok(())
    } else {
        Err(operation_error(ErrorKind::InvalidState, operation))
    }
}

fn status_result(code: u32, operation: &'static str) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(operation_error(boundary::decode_kind(code), operation).attach(code))
    }
}

fn operation_error(kind: ErrorKind, operation: &'static str) -> Error {
    Error::new(kind).attach(pi_result::attachment::Operation(operation))
}

fn same_round(left: &InitIdV1, right: &InitIdV1) -> bool {
    left.module_epoch == right.module_epoch && left.round == right.round
}

fn object_target(raw: &ObjectRefV1) -> CallTargetV1 {
    CallTargetV1 {
        context: raw.context,
        api: raw.api.cast(),
        lease: copy_lease(&raw.lease),
        kind: TARGET_OBJECT,
    }
}

fn copy_target(source: &CallTargetV1) -> CallTargetV1 {
    CallTargetV1 {
        context: source.context,
        api: source.api,
        lease: copy_lease(&source.lease),
        kind: source.kind,
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

fn limited_outcome(kind: ErrorKind) -> OutcomeV1 {
    OutcomeV1 {
        kind: boundary::encode_kind(kind),
        diagnostic_failure_kind: boundary::encode_kind(ErrorKind::InvalidState),
        diagnostic: DiagnosticRefV1 {
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
        },
    }
}

fn diagnostic_is_empty(diagnostic: &DiagnosticRefV1) -> bool {
    diagnostic.context.is_null()
        && diagnostic.api.is_null()
        && diagnostic.lease.object.source == 0
        && diagnostic.lease.object.slot == 0
        && diagnostic.lease.object.generation == 0
        && diagnostic.lease.initialization.module_epoch == 0
        && diagnostic.lease.initialization.round == 0
        && diagnostic.lease.holding == 0
}

pub(super) fn connection_forwarded_view(
    receiver: &Connection,
) -> Result<pi_ffi_abi::InterfaceViewV1> {
    live_binding(&receiver.binding, "forward management view")?;
    let address = &receiver.binding.forwarded_management;
    Ok(InterfaceViewV1 {
        context: address.context,
        table: address.table.cast(),
    })
}
