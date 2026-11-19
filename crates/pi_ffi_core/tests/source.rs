use parking_lot::Mutex;
use pi_ffi_abi::{
    ApiHeaderV1, CallTargetV1, CallTokenV1, DiagnosticApiV1, DiagnosticInfoV1, DiagnosticRefV1,
    HostControlApiV1, InitIdV1, InterfaceRequestV1, InterfaceViewV1, LeaseV1, ObjectKeyV1,
    ObjectRefV1, OperationIdV1, OutcomeV1, SourceApiV1, SourceDescriptorV1, UuidV1,
    TARGET_DIAGNOSTIC, TARGET_OBJECT,
};
use pi_ffi_core::{
    control::Connection,
    diagnostic,
    object::{AbiInterface, Object},
    source::{ExportedInterface, Registry},
};
use pi_result::{Error, ErrorKind, Result};
use std::{
    ffi::c_void,
    sync::{
        atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
};

const SOURCE_ID: u64 = 19;
const EPOCH: u64 = 2;

struct Scope {
    endpoint: Arc<Endpoint>,
    actor: u64,
}

struct Scopes {
    host: Scope,
    plugin: Scope,
}

#[derive(Default)]
struct Endpoint {
    sequence: AtomicU64,
    next_failure: AtomicU32,
    registry: Mutex<Fixture>,
}

/// 按共同 C 合同（common C contract）驱动一个实际 Registry，供验证来源侧行为。
/// 调用保活（call protection）和已提交义务在这里仅作为测试夹具（test fixture）的观察点。
#[derive(Default)]
struct Fixture {
    source: Option<SourceDescriptorV1>,
    holds: Vec<(CallTargetV1, bool)>,
    calls: Vec<u64>,
    retire_requests: Vec<(u64, u64)>,
    retire_controls: usize,
}

// SAFETY: 地址仅来自本进程的实际 Registry 和不可变操作表（operation table），
// 其上下文由已登记的来源持有（source holding）保护，夹具只在调用期间访问。
unsafe impl Send for Fixture {}

static CONTROL: HostControlApiV1 = HostControlApiV1 {
    header: ApiHeaderV1 {
        major: 1,
        size: size_of::<HostControlApiV1>() as u32,
    },
    next_operation: Some(next_operation),
    register_source: Some(register_source),
    track_holding: Some(track_holding),
    enter_call: Some(enter_call),
    leave_call: Some(leave_call),
    request_release: Some(request_release),
    retire_source: Some(retire_source),
};

impl Endpoint {
    fn connection(self: &Arc<Self>) -> Result<Connection> {
        let scopes = Arc::new(Scopes {
            host: Scope {
                endpoint: self.clone(),
                actor: 0,
            },
            plugin: Scope {
                endpoint: self.clone(),
                actor: EPOCH,
            },
        });
        let view = |scope: &Scope| InterfaceViewV1 {
            context: std::ptr::from_ref(scope).cast_mut().cast(),
            table: std::ptr::from_ref(&CONTROL).cast(),
        };
        // SAFETY: 两项视图（view）属于同一 Endpoint，scopes 保活两个地址。
        unsafe {
            Connection::from_native(view(&scopes.host), view(&scopes.plugin), init_id(), scopes)
        }
    }

    fn next(&self, actor: u64) -> OperationIdV1 {
        OperationIdV1 {
            actor,
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed) + 1,
        }
    }

    fn drain_requested(&self) -> usize {
        let released = {
            let mut state = self.registry.lock();
            let holds = std::mem::take(&mut state.holds);
            let (released, retained) = holds
                .into_iter()
                .partition::<Vec<_>, _>(|(_, pending)| *pending);
            state.holds = retained;
            released
        };
        let count = released.len();
        for (target, _) in released {
            match target.kind {
                TARGET_OBJECT => {
                    // SAFETY: 用本夹具此前实际保存的来源操作表及持有，同一来源仍存活。
                    let outcome = unsafe {
                        ((*target.api.cast::<pi_ffi_abi::SourceApiV1>())
                            .release
                            .unwrap())(
                            target.context, target.lease, self.next(0)
                        )
                    };
                    assert_eq!(outcome.kind, 0);
                }
                TARGET_DIAGNOSTIC => {
                    // 诊断归还（diagnostic release）会重放提交，夹具在回调期间仍保留其记录。
                    let callback = unsafe {
                        (*target.api.cast::<pi_ffi_abi::DiagnosticApiV1>())
                            .release
                            .unwrap()
                    };
                    let context = target.context;
                    let lease = copy_lease(&target.lease);
                    self.registry.lock().holds.push((target, true));
                    // SAFETY: 同一来源的预登记诊断（registered diagnostic）在回调期间继续受保护。
                    assert_eq!(unsafe { callback(context, lease) }, 0);
                    self.registry.lock().holds.pop();
                }
                _ => panic!("unexpected target kind"),
            }
        }
        count
    }
}

unsafe extern "C" fn next_operation(context: *mut c_void, out: *mut OperationIdV1) -> u32 {
    // SAFETY: Connection::from_native 的受控视图（controlled view）提供这两个准确地址。
    let scope = unsafe { &*context.cast::<Scope>() };
    let failure = scope.endpoint.next_failure.swap(0, Ordering::Relaxed);
    if failure != 0 {
        return failure;
    }
    unsafe { out.write(scope.endpoint.next(scope.actor)) };
    0
}

unsafe extern "C" fn register_source(
    context: *mut c_void,
    _operation: OperationIdV1,
    descriptor: SourceDescriptorV1,
    out: *mut u64,
) -> OutcomeV1 {
    // SAFETY: 管理表只由相应 Scope（actor scope）的连接持有调用，out 可独占写入。
    let scope = unsafe { &*context.cast::<Scope>() };
    let mut state = scope.endpoint.registry.lock();
    state.source = Some(descriptor);
    unsafe { out.write(SOURCE_ID) };
    diagnostic::success_outcome()
}

unsafe extern "C" fn track_holding(context: *mut c_void, target: CallTargetV1) -> u32 {
    // SAFETY: 该管理入口（control entry）由有效的 Scope（actor scope）提供。
    let scope = unsafe { &*context.cast::<Scope>() };
    scope.endpoint.registry.lock().holds.push((target, false));
    0
}

unsafe extern "C" fn enter_call(
    context: *mut c_void,
    target: CallTargetV1,
    operation: OperationIdV1,
    _rights: u64,
    out: *mut CallTokenV1,
) -> u32 {
    // SAFETY: 该管理入口（control entry）由有效的 Scope（actor scope）提供。
    let scope = unsafe { &*context.cast::<Scope>() };
    let mut state = scope.endpoint.registry.lock();
    if !state
        .holds
        .iter()
        .any(|(record, pending)| !pending && same_target(record, &target))
    {
        return diagnostic::status_code(ErrorKind::NotFound);
    }
    state.calls.push(operation.sequence);
    // SAFETY: out 是调用方提供的一次调用令牌（call token）输出槽（output slot）。
    unsafe {
        out.write(CallTokenV1 {
            source: SOURCE_ID,
            call: operation.sequence,
        })
    };
    0
}

unsafe extern "C" fn leave_call(context: *mut c_void, token: CallTokenV1) -> u32 {
    // SAFETY: 该管理入口（control entry）由有效的 Scope（actor scope）提供。
    let scope = unsafe { &*context.cast::<Scope>() };
    scope
        .endpoint
        .registry
        .lock()
        .calls
        .retain(|id| *id != token.call);
    0
}

unsafe extern "C" fn request_release(context: *mut c_void, target: CallTargetV1) -> u32 {
    // SAFETY: 该管理入口（control entry）由有效的 Scope（actor scope）提供。
    let scope = unsafe { &*context.cast::<Scope>() };
    let mut state = scope.endpoint.registry.lock();
    if let Some((_, pending)) = state
        .holds
        .iter_mut()
        .find(|(record, _)| same_target(record, &target))
    {
        *pending = true;
        return 0;
    }
    diagnostic::status_code(ErrorKind::NotFound)
}

unsafe extern "C" fn retire_source(
    context: *mut c_void,
    id: u64,
    operation: OperationIdV1,
) -> OutcomeV1 {
    // SAFETY: 该管理入口（control entry）由有效的 Scope（actor scope）提供。
    let scope = unsafe { &*context.cast::<Scope>() };
    let source = {
        let mut state = scope.endpoint.registry.lock();
        state
            .retire_requests
            .push((operation.actor, operation.sequence));
        if !state.calls.is_empty() {
            return diagnostic::limited_outcome(ErrorKind::Busy, ErrorKind::InvalidState);
        }
        state.retire_controls += 1;
        state.source
    };
    let Some(source) = source else {
        return diagnostic::limited_outcome(ErrorKind::NotFound, ErrorKind::InvalidState);
    };
    // SAFETY: 夹具此处独占控制调用（source control call），来源上下文仍由登记保护。
    let control = unsafe { &*source.control };
    let Some(retire) = control.try_retire else {
        return diagnostic::limited_outcome(ErrorKind::Corrupted, ErrorKind::InvalidState);
    };
    let outcome = unsafe { retire(source.context, id, operation) };
    if outcome.kind == 0 {
        let Some(release) = control.release_context else {
            return diagnostic::limited_outcome(ErrorKind::Corrupted, ErrorKind::InvalidState);
        };
        // SAFETY: 真实来源已确认可释放上下文（context），夹具此后移除登记。
        if unsafe { release(source.context, id) } != 0 {
            return diagnostic::limited_outcome(ErrorKind::Internal, ErrorKind::InvalidState);
        }
        scope.endpoint.registry.lock().source = None;
    }
    outcome
}

fn same_target(left: &CallTargetV1, right: &CallTargetV1) -> bool {
    left.context == right.context
        && left.api == right.api
        && left.kind == right.kind
        && left.lease.object.source == right.lease.object.source
        && left.lease.object.slot == right.lease.object.slot
        && left.lease.object.generation == right.lease.object.generation
        && left.lease.initialization.module_epoch == right.lease.initialization.module_epoch
        && left.lease.initialization.round == right.lease.initialization.round
        && left.lease.holding == right.lease.holding
}

fn init_id() -> InitIdV1 {
    InitIdV1 {
        module_epoch: EPOCH,
        round: 1,
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

struct Example;

const INTERFACE_ID: UuidV1 = UuidV1 { bytes: [3; 16] };

fn request() -> InterfaceRequestV1 {
    InterfaceRequestV1 {
        interface_id: INTERFACE_ID,
        major: 1,
        size: size_of::<ApiHeaderV1>() as u32,
    }
}

static HEADER: ApiHeaderV1 = ApiHeaderV1 {
    major: 1,
    size: size_of::<ApiHeaderV1>() as u32,
};

// SAFETY: ApiHeaderV1 仅包含固定 C 表头（API header），真实访问通过 Registry 核验。
unsafe impl AbiInterface for Example {
    type Table = ApiHeaderV1;
    const ID: UuidV1 = INTERFACE_ID;
    const MAJOR: u32 = 1;
    const SUPPORTED_RIGHTS: u64 = 3;
}

struct Value {
    drop_count: Arc<AtomicUsize>,
    payload: u32,
}

impl Drop for Value {
    fn drop(&mut self) {
        self.drop_count.fetch_add(1, Ordering::Relaxed);
    }
}

/// 原始持有（original holding）用于同步引导。独立持有、权限和延期清理走真实来源及对象接口。
#[test]
fn test_registry_bootstrap_access_and_deferred_release() -> Result<()> {
    let endpoint = Arc::new(Endpoint::default());
    let connection = endpoint.connection()?;
    let registry = Registry::attach(connection)?;
    let closed = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    // SAFETY: 该声明只代表约定的固定首部。下面的读写使用真实来源访问（source access）。
    let declaration = unsafe { ExportedInterface::new::<Example>(&HEADER) }?;
    let count = closed.clone();
    let object = registry.export(
        Value {
            drop_count: dropped.clone(),
            payload: 17,
        },
        vec![declaration],
        move |_| {
            if count.fetch_add(1, Ordering::Relaxed) == 0 {
                Err(Error::new(ErrorKind::Busy))
            } else {
                Ok(())
            }
        },
    )?;
    let alias = object.clone();
    object.release()?;
    assert_eq!(endpoint.drain_requested(), 0);

    let (original, independent) = alias.with_borrowed(|borrowed| {
        // SAFETY: with_borrowed 为整个引导及取得持有（retain）提供所需的来源保护。
        let plugin = unsafe {
            Connection::from_borrowed(ObjectRefV1 {
                context: borrowed.context,
                api: borrowed.api,
                lease: copy_lease(&borrowed.lease),
            })
        }?;
        let independent = unsafe { Object::retain_borrowed(&plugin, borrowed) }?;
        Ok((copy_lease(&borrowed.lease), independent))
    })?;
    let interface = independent.interface::<Example>()?;
    let mut outdated = copy_lease(&original);
    outdated.initialization.round += 1;
    assert_eq!(
        registry
            .with_access::<Value, ()>(outdated, request(), 1, |_| panic!("stale access"))
            .unwrap_err()
            .current_context(),
        &ErrorKind::Conflict,
    );
    let payload = interface.with_call(2, |call| {
        assert_ne!(call.lease().holding, original.holding);
        registry.with_access::<Value, _>(call.lease(), request(), 2, |value| Ok(value.payload))
    })?;
    assert_eq!(payload, 17);

    // 协调两个线程（thread），让撤销（revocation）命中已经取得访问的真实调用。
    std::thread::scope(|threads| -> Result<()> {
        let (entered, wait_for_entry) = std::sync::mpsc::channel();
        let (proceed, wait_to_proceed) = std::sync::mpsc::channel();
        let registry = &registry;
        let interface = &interface;
        let accessing = threads.spawn(move || {
            interface.with_call(2, |call| {
                registry.with_access::<Value, _>(call.lease(), request(), 2, |value| {
                    entered.send(()).unwrap();
                    wait_to_proceed
                        .recv_timeout(std::time::Duration::from_secs(3))
                        .unwrap();
                    Ok(value.payload)
                })
            })
        });
        wait_for_entry
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        registry.revoke(original.object)?;
        assert_eq!(
            interface
                .with_call(2, |call| {
                    registry.with_access::<Value, ()>(call.lease(), request(), 2, |_| {
                        panic!("revoked work")
                    })
                })
                .unwrap_err()
                .current_context(),
            &ErrorKind::InvalidState,
        );
        proceed.send(()).unwrap();
        assert_eq!(accessing.join().unwrap()?, 17);
        Ok(())
    })?;
    assert_eq!(
        interface.with_call(1, |call| {
            registry.with_access::<Value, _>(call.lease(), request(), 1, |value| Ok(value.payload))
        })?,
        17
    );

    alias.release()?;
    independent.release()?;
    assert_eq!(endpoint.drain_requested(), 1);
    assert_eq!(registry.maintain()?, 1);
    assert_eq!(closed.load(Ordering::Relaxed), 0);

    drop(interface);
    assert_eq!(endpoint.drain_requested(), 2);
    assert_eq!(registry.maintain()?, 1);
    assert_eq!(closed.load(Ordering::Relaxed), 1);
    assert_eq!(dropped.load(Ordering::Relaxed), 0);
    assert_eq!(registry.maintain()?, 0);
    assert_eq!(closed.load(Ordering::Relaxed), 2);
    assert_eq!(dropped.load(Ordering::Relaxed), 1);
    assert_eq!(registry.maintain()?, 0);
    registry.try_retire()?;
    assert!(endpoint.registry.lock().source.is_none());
    Ok(())
}

/// 实际来源诊断（source diagnostic）为每个交接建立独立持有，宿主保留三段原文。
#[test]
fn test_registry_diagnostic_can_outlive_source() -> Result<()> {
    let endpoint = Arc::new(Endpoint::default());
    let connection = endpoint.connection()?;
    let registry = Registry::attach(connection.clone())?;
    let original = Error::new(ErrorKind::Dependency).attach("来源诊断（source diagnostic）");
    let failure = diagnostic::ManagementFailure::capture(&original);
    let first = registry.publish_management_failure(&failure);
    let second = registry.publish_management_failure(&failure);
    assert_ne!(
        first.diagnostic.lease.holding,
        second.diagnostic.lease.holding
    );

    // SAFETY: 诊断（diagnostic）来自当前登记的真实来源，连接及其当前持有均有效。
    let copied = unsafe { diagnostic::decode_management(Some(&connection), first) }.unwrap_err();
    // SAFETY: 本次返回的另一份独立持有（independent holding）首次由宿主接管。
    let repeated = unsafe { diagnostic::decode_management(Some(&connection), second) }.unwrap_err();
    assert_eq!(copied.current_context(), &ErrorKind::Dependency);
    assert_eq!(repeated.current_context(), &ErrorKind::Dependency);

    assert_eq!(endpoint.drain_requested(), 2);
    assert_eq!(registry.maintain()?, 1);
    assert_eq!(registry.maintain()?, 0);
    registry.try_retire()?;
    assert!(endpoint.registry.lock().source.is_none());

    for error in [&copied, &repeated] {
        let saved = diagnostic::copied_diagnostic(error).unwrap();
        assert_eq!(saved.raw_kind, 15);
        assert_eq!(saved.user, pi_result::user_message(&original));
        assert_eq!(saved.verbose, pi_result::verbose_message(&original));
        assert_eq!(saved.debug, pi_result::debug_message(&original));
    }
    Ok(())
}

/// 输入引导（input bootstrap）在实际来源保护内先拒绝矛盾分类，同时归还诊断持有。
#[test]
fn test_bootstrap_rejects_mismatched_kind_before_copy() {
    let source = DiagnosticWire::new(15, 5);
    let failure = source.bootstrap();
    assert_eq!(failure.current_context(), &ErrorKind::Corrupted);
    assert_eq!(source.info_calls.load(Ordering::Relaxed), 1);
    assert_eq!(source.copy_calls.load(Ordering::Relaxed), 0);
    assert_eq!(source.release_calls.load(Ordering::Relaxed), 1);
    assert_eq!(source.released.load(Ordering::Relaxed), 1);
    assert_eq!(source.endpoint.drain_requested(), 1);
    assert_eq!(source.release_calls.load(Ordering::Relaxed), 2);
    assert_eq!(source.released.load(Ordering::Relaxed), 1);
    let message = pi_result::debug_message(&failure);
    assert!(message.contains("（operation kind）15"), "{message}");
    assert!(message.contains("（diagnostic kind）5"), "{message}");
}

/// 标准管理入口（standard management entry）比较原编号，即使两编号均投影为 Other。
#[test]
fn test_management_rejects_mismatched_raw_kinds_before_copy() -> Result<()> {
    let source = DiagnosticWire::new(88_601, 99_702);
    let failure = source.managed()?;
    assert_eq!(failure.current_context(), &ErrorKind::Corrupted);
    assert_eq!(source.info_calls.load(Ordering::Relaxed), 1);
    assert_eq!(source.copy_calls.load(Ordering::Relaxed), 0);
    assert_eq!(source.release_calls.load(Ordering::Relaxed), 0);
    assert_eq!(source.released.load(Ordering::Relaxed), 0);
    assert!(source.endpoint.registry.lock().calls.is_empty());
    assert_eq!(source.endpoint.drain_requested(), 1);
    assert_eq!(source.endpoint.drain_requested(), 0);
    assert_eq!(source.release_calls.load(Ordering::Relaxed), 1);
    assert_eq!(source.released.load(Ordering::Relaxed), 1);
    let message = pi_result::debug_message(&failure);
    assert!(message.contains("（operation kind）88601"), "{message}");
    assert!(message.contains("（diagnostic kind）99702"), "{message}");
    Ok(())
}

/// 正常读取失败（read failure）作为次级诊断，两个入口均保留原操作分类和归还义务。
#[test]
fn test_diagnostic_read_failure_preserves_operation_kind() -> Result<()> {
    for bootstrap in [true, false] {
        let mut source = DiagnosticWire::new(15, 15);
        source.info_status = diagnostic::status_code(ErrorKind::Io);
        let failure = if bootstrap {
            source.bootstrap()
        } else {
            source.managed()?
        };
        assert_eq!(failure.current_context(), &ErrorKind::Dependency);
        let message = pi_result::debug_message(&failure);
        assert!(message.contains("diagnostic.info"), "{message}");
        assert_eq!(source.info_calls.load(Ordering::Relaxed), 1);
        assert_eq!(source.copy_calls.load(Ordering::Relaxed), 0);
        assert_eq!(source.endpoint.drain_requested(), 1);
        assert_eq!(source.released.load(Ordering::Relaxed), 1);
    }
    Ok(())
}

/// 提供可观察的独立 C 诊断（independent C diagnostic）和既有归还记录。
struct DiagnosticWire {
    endpoint: Arc<Endpoint>,
    operation_kind: u32,
    reported_kind: u32,
    info_status: u32,
    info_calls: AtomicUsize,
    copy_calls: AtomicUsize,
    release_calls: AtomicUsize,
    released: AtomicUsize,
}

impl DiagnosticWire {
    fn new(operation_kind: u32, reported_kind: u32) -> Self {
        Self {
            endpoint: Arc::new(Endpoint::default()),
            operation_kind,
            reported_kind,
            info_status: 0,
            info_calls: AtomicUsize::new(0),
            copy_calls: AtomicUsize::new(0),
            release_calls: AtomicUsize::new(0),
            released: AtomicUsize::new(0),
        }
    }

    fn bootstrap(&self) -> Error {
        let seed = ObjectRefV1 {
            context: std::ptr::from_ref(self).cast_mut().cast(),
            api: &WIRE_OBJECT,
            lease: wire_lease(1),
        };
        // SAFETY: 夹具提供有效的只读 C 表、上下文和同来源诊断，保持到调用返回。
        unsafe { Connection::from_borrowed(seed) }.err().unwrap()
    }

    fn managed(&self) -> Result<Error> {
        let connection = self.endpoint.connection()?;
        // SAFETY: 夹具预登记本轮来源的准确表、上下文和本次唯一诊断持有。
        Ok(
            unsafe { diagnostic::decode_management(Some(&connection), self.outcome()) }
                .unwrap_err(),
        )
    }

    fn outcome(&self) -> OutcomeV1 {
        let context = std::ptr::from_ref(self).cast_mut().cast();
        let lease = wire_lease(2);
        self.endpoint.registry.lock().holds.push((
            CallTargetV1 {
                context,
                api: std::ptr::from_ref(&WIRE_DIAGNOSTIC).cast(),
                lease: copy_lease(&lease),
                kind: TARGET_DIAGNOSTIC,
            },
            false,
        ));
        OutcomeV1 {
            kind: self.operation_kind,
            diagnostic_failure_kind: 0,
            diagnostic: DiagnosticRefV1 {
                context,
                api: &WIRE_DIAGNOSTIC,
                lease,
            },
        }
    }
}

fn wire_lease(slot: u64) -> LeaseV1 {
    LeaseV1 {
        object: ObjectKeyV1 {
            source: SOURCE_ID,
            slot,
            generation: 1,
        },
        initialization: init_id(),
        holding: slot,
    }
}

static WIRE_OBJECT: SourceApiV1 = SourceApiV1 {
    header: ApiHeaderV1 {
        major: 1,
        size: size_of::<SourceApiV1>() as u32,
    },
    validate: Some(wire_validate),
    retain: Some(wire_retain),
    release: Some(wire_release_object),
    query_interface: Some(wire_query),
};

static WIRE_DIAGNOSTIC: DiagnosticApiV1 = DiagnosticApiV1 {
    header: ApiHeaderV1 {
        major: 1,
        size: size_of::<DiagnosticApiV1>() as u32,
    },
    info: Some(wire_info),
    copy: Some(wire_copy),
    release: Some(wire_release_diagnostic),
};

unsafe extern "C" fn wire_validate(
    _: *mut c_void,
    _: LeaseV1,
    _: InterfaceRequestV1,
    _: u64,
) -> OutcomeV1 {
    diagnostic::limited_outcome(ErrorKind::Unsupported, ErrorKind::InvalidState)
}

unsafe extern "C" fn wire_retain(
    _: *mut c_void,
    _: LeaseV1,
    _: OperationIdV1,
    _: *mut ObjectRefV1,
) -> OutcomeV1 {
    diagnostic::limited_outcome(ErrorKind::Unsupported, ErrorKind::InvalidState)
}

unsafe extern "C" fn wire_release_object(
    _: *mut c_void,
    _: LeaseV1,
    _: OperationIdV1,
) -> OutcomeV1 {
    diagnostic::limited_outcome(ErrorKind::Unsupported, ErrorKind::InvalidState)
}

unsafe extern "C" fn wire_query(
    context: *mut c_void,
    _: LeaseV1,
    _: InterfaceRequestV1,
    _: *mut InterfaceViewV1,
) -> OutcomeV1 {
    // SAFETY: 引导（bootstrap）调用者在借用存续期提供实际 DiagnosticWire。
    unsafe { &*context.cast::<DiagnosticWire>() }.outcome()
}

unsafe extern "C" fn wire_info(
    context: *mut c_void,
    _: LeaseV1,
    out: *mut DiagnosticInfoV1,
) -> u32 {
    // SAFETY: 本次调用受来源保活保护，输出槽（output slot）由调用者独占。
    let source = unsafe { &*context.cast::<DiagnosticWire>() };
    source.info_calls.fetch_add(1, Ordering::Relaxed);
    if source.info_status != 0 {
        return source.info_status;
    }
    unsafe {
        out.write(DiagnosticInfoV1 {
            kind: source.reported_kind,
            user_len: 0,
            verbose_len: 0,
            debug_len: 0,
        })
    };
    0
}

unsafe extern "C" fn wire_copy(
    context: *mut c_void,
    _: LeaseV1,
    _: *mut u8,
    _: u64,
    written: *mut u64,
) -> u32 {
    // SAFETY: 本次调用受来源保活保护，诊断内容为空且写入计数输出槽有效。
    let source = unsafe { &*context.cast::<DiagnosticWire>() };
    source.copy_calls.fetch_add(1, Ordering::Relaxed);
    unsafe { written.write(0) };
    0
}

unsafe extern "C" fn wire_release_diagnostic(context: *mut c_void, lease: LeaseV1) -> u32 {
    // SAFETY: 引导保护或宿主已登记的归还（registered release）覆盖同一来源。
    let source = unsafe { &*context.cast::<DiagnosticWire>() };
    let target = CallTargetV1 {
        context,
        api: std::ptr::from_ref(&WIRE_DIAGNOSTIC).cast(),
        lease,
        kind: TARGET_DIAGNOSTIC,
    };
    source.release_calls.fetch_add(1, Ordering::Relaxed);
    let mut state = source.endpoint.registry.lock();
    for (entry, pending) in &mut state.holds {
        if same_target(entry, &target) {
            *pending = true;
            // 先提交既有宿主责任，只有首次归还改变诊断持有状态。
            if source.released.load(Ordering::Relaxed) == 0 {
                source.released.fetch_add(1, Ordering::Relaxed);
            }
            return 0;
        }
    }
    diagnostic::status_code(ErrorKind::NotFound)
}

/// 来源仍有活动调用（active call）时，重复退役请求应使用同一编号，宿主暂不调用来源控制表。
#[test]
fn test_retry_keeps_retirement_identity_when_source_is_busy() -> Result<()> {
    let endpoint = Arc::new(Endpoint::default());
    let registry = Registry::attach(endpoint.connection()?)?;
    let object = registry.export((), Vec::new(), |_| Ok(()))?;
    object.with_borrowed(|_| {
        for _ in 0..2 {
            let error = registry.try_retire().unwrap_err();
            assert_eq!(error.current_context(), &ErrorKind::Busy);
        }
        let state = endpoint.registry.lock();
        assert_eq!(state.retire_controls, 0);
        assert_eq!(state.retire_requests.len(), 2);
        assert_eq!(state.retire_requests[0], state.retire_requests[1]);
        Ok(())
    })?;
    let failure = registry
        .export((), Vec::new(), |_| Ok(()))
        .err()
        .expect("new business is disabled after submitting retirement");
    assert_eq!(failure.current_context(), &ErrorKind::InvalidState);
    object.release()?;
    assert_eq!(endpoint.drain_requested(), 1);
    assert_eq!(registry.maintain()?, 0);
    registry.try_retire()?;
    let state = endpoint.registry.lock();
    assert_eq!(state.retire_controls, 1);
    assert_eq!(state.retire_requests.len(), 3);
    assert_eq!(state.retire_requests[0], state.retire_requests[2]);
    assert!(state.source.is_none());
    drop(state);
    registry.try_retire()?;
    assert_eq!(endpoint.registry.lock().retire_requests.len(), 3);
    Ok(())
}

/// 取号暂失败时不提交退役或关闭新业务，之后可重新取得合法请求号。
#[test]
fn test_retirement_request_can_retry_failed_assignment() -> Result<()> {
    let endpoint = Arc::new(Endpoint::default());
    let registry = Registry::attach(endpoint.connection()?)?;
    endpoint
        .next_failure
        .store(diagnostic::status_code(ErrorKind::Io), Ordering::Relaxed);
    assert_eq!(
        registry.try_retire().unwrap_err().current_context(),
        &ErrorKind::Io
    );
    assert!(endpoint.registry.lock().retire_requests.is_empty());
    let object = registry.export((), Vec::new(), |_| Ok(()))?;
    object.release()?;
    assert_eq!(endpoint.drain_requested(), 1);
    assert_eq!(registry.maintain()?, 0);
    registry.try_retire()?;
    let state = endpoint.registry.lock();
    assert_eq!(state.retire_requests.len(), 1);
    assert_eq!(state.retire_controls, 1);
    assert!(state.source.is_none());
    Ok(())
}

#[derive(Debug)]
struct CleanupPanics;

impl core::fmt::Display for CleanupPanics {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("test attachment")
    }
}

impl Drop for CleanupPanics {
    fn drop(&mut self) {
        panic!("controlled Rust test failure during cleanup");
    }
}

/// 原报告处理失败保留原分类，并且没有新的诊断登记。已有读权限和实际归还依然有效。
#[test]
fn test_error_report_cleanup_precedes_diagnostic_registration() -> Result<()> {
    let endpoint = Arc::new(Endpoint::default());
    let registry = Registry::attach(endpoint.connection()?)?;
    let declaration = unsafe { ExportedInterface::new::<Example>(&HEADER) }?;
    let object = registry.export(17u32, vec![declaration], |_| Ok(()))?;
    let interface = object.interface::<Example>()?;
    let failure = interface
        .with_call(1, |call| {
            // SAFETY: 本次访问由同一实际来源及已核对的对象持有（holding）保护。
            let outcome = unsafe {
                Registry::dispatch::<u32>(call.context, call.lease(), request(), 1, |_| {
                    Err(Error::new(ErrorKind::Io).attach(CleanupPanics))
                })
            };
            assert_eq!(outcome.kind, diagnostic::status_code(ErrorKind::Io));
            assert_eq!(
                outcome.diagnostic_failure_kind,
                diagnostic::status_code(ErrorKind::Internal)
            );
            assert!(outcome.diagnostic.context.is_null());
            // SAFETY: 仅接管当前受保护调用的完整结果一次。
            unsafe { call.decode(outcome) }
        })
        .unwrap_err();
    let registered = endpoint
        .registry
        .lock()
        .holds
        .iter()
        .filter(|(target, _)| target.kind == TARGET_DIAGNOSTIC)
        .count();
    assert_eq!(
        registered, 0,
        "cleanup failed before diagnostic publication"
    );
    assert_eq!(failure.current_context(), &ErrorKind::Io);
    let value = interface.with_call(1, |call| {
        registry.with_access(call.lease(), request(), 1, |value: &u32| Ok(*value))
    })?;
    assert_eq!(value, 17);
    drop(interface);
    object.release()?;
    assert_eq!(endpoint.drain_requested(), 1);
    assert_eq!(
        registry.maintain().unwrap_err().current_context(),
        &ErrorKind::Internal
    );
    assert_eq!(
        registry.try_retire().unwrap_err().current_context(),
        &ErrorKind::Internal
    );
    assert!(endpoint.registry.lock().source.is_some());
    assert_eq!(endpoint.drain_requested(), 1);
    Ok(())
}

/// 处理恐慌负载（panic payload）也失败时，保留本地来源的失败状态。
#[test]
fn test_panic_payload_cleanup_failure_remains_observable() -> Result<()> {
    let endpoint = Arc::new(Endpoint::default());
    let registry = Registry::attach(endpoint.connection()?)?;
    let declaration = unsafe { ExportedInterface::new::<Example>(&HEADER) }?;
    let object = registry.export(17u32, vec![declaration], |_| Ok(()))?;
    let interface = object.interface::<Example>()?;
    let failure = interface
        .with_call(1, |call| {
            // SAFETY: 同一真实来源和本次借用覆盖闭包、返回和归还。
            let outcome = unsafe {
                Registry::dispatch::<u32>(call.context, call.lease(), request(), 1, |_| {
                    std::panic::panic_any(CleanupPanics)
                })
            };
            unsafe { call.decode(outcome) }
        })
        .unwrap_err();
    assert_eq!(failure.current_context(), &ErrorKind::Internal);
    drop(interface);
    object.release()?;
    endpoint.drain_requested();
    assert!(
        registry.maintain().is_err(),
        "uncertain cleanup was forgotten by the source"
    );
    Ok(())
}

struct FormattingPanics;

impl core::fmt::Display for FormattingPanics {
    fn fmt(&self, _: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        std::panic::panic_any(CleanupPanics)
    }
}

impl core::fmt::Debug for FormattingPanics {
    fn fmt(&self, _: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        std::panic::panic_any(CleanupPanics)
    }
}

/// 格式化错误报告时捕获的值也必须完成清理。未确认时持久保留，并允许原持有继续归还。
#[test]
fn test_snapshot_payload_cleanup_failure_remains_observable() -> Result<()> {
    let endpoint = Arc::new(Endpoint::default());
    let registry = Registry::attach(endpoint.connection()?)?;
    // SAFETY: Example 使用由同一来源保活的实际固定表（static table）。
    let declaration = unsafe { ExportedInterface::new::<Example>(&HEADER) }?;
    let object = registry.export(17u32, vec![declaration], |_| Ok(()))?;
    let interface = object.interface::<Example>()?;
    let failure = interface
        .with_call(1, |call| {
            // SAFETY: 原对象的真实持有（holding）和调用（call）覆盖报告格式化。
            let outcome = unsafe {
                Registry::dispatch::<u32>(call.context, call.lease(), request(), 1, |_| {
                    Err(Error::new(ErrorKind::Io).attach(FormattingPanics))
                })
            };
            assert_eq!(outcome.kind, diagnostic::status_code(ErrorKind::Io));
            assert_eq!(
                outcome.diagnostic_failure_kind,
                diagnostic::status_code(ErrorKind::Internal)
            );
            assert!(outcome.diagnostic.context.is_null());
            // SAFETY: 当前实际活动调用独占接管返回结果。
            unsafe { call.decode(outcome) }
        })
        .unwrap_err();
    assert_eq!(failure.current_context(), &ErrorKind::Io);
    drop(interface);
    object.release()?;
    assert_eq!(endpoint.drain_requested(), 1);
    assert_eq!(
        registry.maintain().unwrap_err().current_context(),
        &ErrorKind::Internal
    );
    assert_eq!(
        registry.try_retire().unwrap_err().current_context(),
        &ErrorKind::Internal
    );
    assert!(endpoint.registry.lock().source.is_some());
    assert_eq!(endpoint.drain_requested(), 1);
    Ok(())
}

struct CountedBusyReport {
    formatted: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
}

impl core::fmt::Debug for CountedBusyReport {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(self, f)
    }
}

impl core::fmt::Display for CountedBusyReport {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.formatted.fetch_add(1, Ordering::Relaxed);
        f.write_str("observed Busy report")
    }
}

impl Drop for CountedBusyReport {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

/// 本地 Busy 报告无需格式化。处理完成后继续推进同一真实对象的关闭。
#[test]
fn test_clean_busy_report_allows_close_retry() -> Result<()> {
    let endpoint = Arc::new(Endpoint::default());
    let registry = Registry::attach(endpoint.connection()?)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let report_drops = Arc::new(AtomicUsize::new(0));
    let report_formats = Arc::new(AtomicUsize::new(0));
    let (observed_calls, observed_drops) = (calls.clone(), report_drops.clone());
    let observed_formats = report_formats.clone();
    let object = registry.export(17u32, Vec::new(), move |value| {
        assert_eq!(*value, 17);
        if observed_calls.fetch_add(1, Ordering::Relaxed) == 0 {
            let attachment = CountedBusyReport {
                dropped: observed_drops.clone(),
                formatted: observed_formats.clone(),
            };
            return Err(Error::new(ErrorKind::Busy).attach(attachment));
        }
        assert_eq!(observed_drops.load(Ordering::Relaxed), 1);
        Ok(())
    })?;
    object.release()?;
    assert_eq!(endpoint.drain_requested(), 1);
    assert_eq!(registry.maintain()?, 1);
    assert_eq!(report_drops.load(Ordering::Relaxed), 1);
    assert_eq!(report_formats.load(Ordering::Relaxed), 0);
    assert_eq!(registry.maintain()?, 0);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    registry.try_retire()?;
    assert!(endpoint.registry.lock().source.is_none());
    Ok(())
}

/// 处理 Busy 报告失误后停止重复执行关闭。来源控制接口完整交付实际清理错误及原回调类别。
#[test]
fn test_unclean_busy_report_reports_terminal_cleanup_failure() -> Result<()> {
    let endpoint = Arc::new(Endpoint::default());
    let connection = endpoint.connection()?;
    let registry = Registry::attach(connection.clone())?;
    let calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = calls.clone();
    let object = registry.export(17u32, Vec::new(), move |_| {
        observed_calls.fetch_add(1, Ordering::Relaxed);
        Err(Error::new(ErrorKind::Busy).attach(CleanupPanics))
    })?;
    object.release()?;
    assert_eq!(endpoint.drain_requested(), 1);
    let source = endpoint
        .registry
        .lock()
        .source
        .expect("source still registered");
    let operation = endpoint.next(0);
    let mut out = pi_ffi_abi::MaintenanceV1 { remaining: 777 };
    // SAFETY: 端点实际登记的存活来源拥有这张控制表（control table），维护结果槽独占可写。
    let outcome = unsafe {
        ((*source.control).maintain.unwrap())(source.context, SOURCE_ID, operation, &mut out)
    };
    assert_eq!(out.remaining, 777);
    assert_eq!(outcome.kind, diagnostic::status_code(ErrorKind::Internal));
    // SAFETY: 仍在实际来源登记内。唯一读出并归还上述控制调用交付的诊断。
    let failure = unsafe { diagnostic::decode_management(Some(&connection), outcome) }.unwrap_err();
    let copied = diagnostic::copied_diagnostic(&failure).expect("a complete cleanup diagnostic");
    assert!(
        copied.debug.to_lowercase().contains("busy"),
        "{}",
        copied.debug
    );
    assert_eq!(endpoint.drain_requested(), 1);
    assert_eq!(
        registry.maintain().unwrap_err().current_context(),
        &ErrorKind::Internal
    );
    let retired = registry.try_retire().unwrap_err();
    assert_eq!(retired.current_context(), &ErrorKind::Internal);
    let copied = diagnostic::copied_diagnostic(&retired).expect("a complete retirement diagnostic");
    assert!(
        copied.debug.to_lowercase().contains("busy"),
        "{}",
        copied.debug
    );
    assert_eq!(endpoint.drain_requested(), 1);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert!(endpoint.registry.lock().source.is_some());
    Ok(())
}
