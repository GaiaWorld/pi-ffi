// 复用本模块真实管理表与加载制品，只验证独立数据交接，不伪造 P2P 终端。
// 单独建立本轮共同连接，未调用不接收业务输入的元信息测试制品的初始化入口。
use pi_ffi_abi::ObjectRefV1;
use pi_ffi_adapters::data::{DataInput, PublishedData};
use std::cell::Cell;

/// 历史三参数产物在装入核对时即拒绝，不进入初始化（initialization）。
#[test]
fn initialization_v2_rejects_historical_three_argument_artifact() {
    let legacy = FIXTURE.replacen("pi_ffi_initialize_v2", "pi_ffi_initialize_v1", 1)
        .replacen("    _: *const ObjectRefV1,\n) -> InitReplyV1", ") -> InitReplyV1", 1);
    let artifact = Artifact::from_binary(&compile_fixture(&legacy), VERSION);
    let package = artifact.package(ACTUAL_UUID);
    let result = loader::verify_files(&package, &build());
    let failure = result.err().expect("三参数旧产物必须在调用前拒绝");
    assert_eq!(*failure.current_context(), ErrorKind::NotFound);
    assert!(pi_result::debug_message(&failure).contains("pi_ffi_initialize_v2"));
    assert!(!artifact.mapped());
}

/// 持有真实已加载制品和本轮共同连接；调用者先销毁交接对象，再显式 finish。
struct DataRound {
    plugin: crate::host::Plugin,
    id: InitIdV1,
    connection: Connection,
    _artifact: Artifact,
}

impl DataRound {
    /// 默认使用生产管理表（control table），只为本轮数据验证建立记录。
    fn new() -> Self {
        Self::with_table(VERSION, &CONTROL)
    }

    /// 用指定实际版本和外部管理表（control table）构建独占轮次。
    /// 测试表只在标准边界上改变返回结果，不补充生产能力。
    fn with_table(version: &'static str, table: &'static HostControlApiV1) -> Self {
        let artifact = if version == VERSION {
            Artifact::new()
        } else {
            Artifact::from_binary(
                &compile_fixture(&FIXTURE.replace(VERSION, version)),
                version,
            )
        };
        let package = artifact.package(ACTUAL_UUID);
        let manager = Manager::process(build()).unwrap();
        let plugin = manager.load(&package, &request(ACTUAL_UUID)).unwrap();
        let epoch = loaded_epoch(&package);
        let id = {
            let mut state = AUTHORITY.state.lock();
            let image = Arc::clone(ready_for_prepare(&state, epoch).unwrap().0);
            reserve_round_record(&mut state, epoch, &image).unwrap()
        };
        let scopes = Arc::new(CallbackScopes {
            host: CallbackScope {
                authority: &AUTHORITY,
                actor: 0,
                id: round_id(&id),
                owner: CodeOwner::Host,
            },
            plugin: CallbackScope {
                authority: &AUTHORITY,
                actor: id.module_epoch,
                id: round_id(&id),
                owner: CodeOwner::Plugin {
                    epoch: id.module_epoch,
                },
            },
        });
        let host = InterfaceViewV1 {
            context: std::ptr::from_ref(&scopes.host).cast_mut().cast(),
            table: std::ptr::from_ref(table).cast(),
        };
        let plugin_view = InterfaceViewV1 {
            context: std::ptr::from_ref(&scopes.plugin).cast_mut().cast(),
            table: std::ptr::from_ref(table).cast(),
        };
        // SAFETY: 两个视图来自真实本轮权威，稳定静态表和 scopes 持有覆盖全部调用。
        let connection =
            unsafe { Connection::from_native(host, plugin_view, round_id(&id), scopes) }.unwrap();
        {
            let mut state = AUTHORITY.state.lock();
            let record = state
                .rounds
                .iter_mut()
                .find(|r| same_round(&r.id, &id))
                .unwrap();
            record.connection = Some(connection.clone());
        }
        Self {
            plugin,
            id,
            connection,
            _artifact: artifact,
        }
    }

    /// 调用前所有借用及发布对象已退出。沿未开始轮次的真实收尾路径关闭并退役管理代。
    fn finish(self) {
        let _reason =
            AUTHORITY.fail_prepare(round_id(&self.id), Error::new(ErrorKind::InvalidInput));
        drop(self.connection);
        let initialization = self.plugin.current_initialization().unwrap().unwrap();
        initialization.close().unwrap();
        self.plugin.unload().unwrap();
    }
}

/// 未实现克隆（Clone）的拥有值，记录最终析构（Drop）次数与原缓冲区。
struct OwnedData {
    bytes: Vec<u8>,
    dropped: Arc<AtomicUsize>,
}

impl Drop for OwnedData {
    /// 不调用外部代码，仅记录本值的实际析构（Drop）。
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

/// 未实现克隆（Clone）的值只移动一次，原缓冲区地址和内容保持，最终只析构（Drop）一次。
#[test]
fn initialization_data_moves_non_clone_original_once() {
    let round = DataRound::new();
    let dropped = Arc::new(AtomicUsize::new(0));
    let value = OwnedData {
        bytes: vec![3, 1, 4, 1, 5],
        dropped: dropped.clone(),
    };
    let address = value.bytes.as_ptr();
    // SAFETY: 同一本测试构建的实际 D、分配器和代码，真实本轮共同连接保持有效。
    let data = unsafe { PublishedData::publish(round.connection.clone(), value) }.unwrap();
    let mut received = None;
    data.with_input(|input| {
        // SAFETY: 同步借出原地址，连接、类型和配套构建满足约定。
        let first = unsafe { DataInput::<OwnedData>::from_input(&round.connection, input) }?;
        first.take_into(&mut received)?;
        // SAFETY: 仍借用同一真实同步输入；再次取值应由实现拒绝。
        let second = unsafe { DataInput::<OwnedData>::from_input(&round.connection, input) }?;
        let mut duplicate = None;
        let failure = second.take_into(&mut duplicate).err().unwrap();
        assert_eq!(*failure.current_context(), ErrorKind::InvalidState);
        assert!(duplicate.is_none());
        Ok(())
    })
    .unwrap();
    let received = received.unwrap();
    assert_eq!(received.bytes, [3, 1, 4, 1, 5]);
    assert_eq!(received.bytes.as_ptr(), address);
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    drop(received);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    drop(data);
    round.finish();
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

/// 相同尺寸的不同实际类型在消费前拒绝；随后正确类型仍能取得原值。
#[test]
fn initialization_data_rejects_wrong_type_before_consumption() {
    struct Left(u64);
    struct Right(u64);
    assert_eq!(std::mem::size_of::<Left>(), std::mem::size_of::<Right>());
    let round = DataRound::new();
    // SAFETY: 同一构建的 Left；后面的错类型只请求预核，不按错误类型解释值。
    let data = unsafe { PublishedData::publish(round.connection.clone(), Left(73)) }.unwrap();
    data.with_input(|input| {
        // SAFETY: 原始输入受保护，合法预核必须拒绝不匹配类型。
        let failure = unsafe { DataInput::<Right>::from_input(&round.connection, input) }
            .err()
            .unwrap();
        assert_eq!(*failure.current_context(), ErrorKind::Conflict);
        let right = Right(0);
        assert_eq!(right.0, 0);
        // SAFETY: 实际类型、连接和同步借用匹配。
        let valid = unsafe { DataInput::<Left>::from_input(&round.connection, input) }?;
        let mut output = None;
        valid.take_into(&mut output)?;
        assert_eq!(output.unwrap().0, 73);
        Ok(())
    })
    .unwrap();
    drop(data);
    round.finish();
}

/// 共享（Arc）交接指向同一对象，不额外要求被共享类型实现 Clone。
#[test]
fn initialization_data_preserves_shared_instance() {
    let round = DataRound::new();
    let value = Arc::new(AtomicUsize::new(9));
    // SAFETY: 原 Arc 来自同一构建和分配器，代码在整个测试期间有效。
    let data = unsafe { PublishedData::publish(round.connection.clone(), value.clone()) }.unwrap();
    data.with_input(|input| {
        // SAFETY: 当前原始借用及实际 Arc 类型匹配。
        let received =
            unsafe { DataInput::<Arc<AtomicUsize>>::from_input(&round.connection, input) }?;
        let mut output = None;
        received.take_into(&mut output)?;
        let output = output.unwrap();
        assert!(Arc::ptr_eq(&output, &value));
        output.store(17, Ordering::SeqCst);
        Ok(())
    })
    .unwrap();
    assert_eq!(value.load(Ordering::SeqCst), 17);
    drop(data);
    round.finish();
}

/// 空数据也使用合法第四输入；拒绝覆盖非空输出，不消费原槽。
#[test]
fn initialization_data_unit_and_nonempty_output() {
    let round = DataRound::new();
    // SAFETY: 合法本轮连接及同一构建的单元值。
    let data = unsafe { PublishedData::publish(round.connection.clone(), ()) }.unwrap();
    data.with_input(|input| {
        // SAFETY: 当前真实同步输入，配套类型匹配。
        let received = unsafe { DataInput::<()>::from_input(&round.connection, input) }?;
        let mut occupied = Some(());
        assert_eq!(
            *received
                .take_into(&mut occupied)
                .err()
                .unwrap()
                .current_context(),
            ErrorKind::InvalidState
        );
        assert_eq!(occupied, Some(()));
        // SAFETY: 仍在同一原始借用内，先前目标非空不应消费。
        let received = unsafe { DataInput::<()>::from_input(&round.connection, input) }?;
        let mut empty = None;
        received.take_into(&mut empty)?;
        assert_eq!(empty, Some(()));
        Ok(())
    })
    .unwrap();
    drop(data);
    round.finish();
}

/// 复制描述不构成原始借用；嵌套借用退出后恢复上一层合法输入。
#[test]
fn initialization_data_requires_original_address_and_restores_nested_scope() {
    let round = DataRound::new();
    // SAFETY: 两份同一构建的值由真实本轮来源保管。
    let data = unsafe { PublishedData::publish(round.connection.clone(), 41u64) }.unwrap();
    let nested = unsafe { PublishedData::publish(round.connection.clone(), 42u64) }.unwrap();
    data.with_input(|input| {
        let copy = ObjectRefV1 {
            context: input.context,
            api: input.api,
            lease: holding_lease(&input.lease),
        };
        // SAFETY: 描述及原持有仍有效，只有借用地址不同，入口应在取值前拒绝。
        let copied = unsafe { DataInput::<u64>::from_input(&round.connection, &copy) }
            .err()
            .unwrap();
        assert_eq!(*copied.current_context(), ErrorKind::Conflict);
        nested.with_input(|nested_input| {
            // SAFETY: 外层借用仍存续，但不是当前最内层，必须拒绝。
            let hidden = unsafe { DataInput::<u64>::from_input(&round.connection, input) }
                .err()
                .unwrap();
            assert_eq!(*hidden.current_context(), ErrorKind::Conflict);
            // SAFETY: 最内层真实借用与类型匹配。
            let valid = unsafe { DataInput::<u64>::from_input(&round.connection, nested_input) }?;
            let mut output = None;
            valid.take_into(&mut output)?;
            assert_eq!(output, Some(42));
            Ok(())
        })?;
        // SAFETY: 内层退出后外层恢复，原持有仍受保护。
        let valid = unsafe { DataInput::<u64>::from_input(&round.connection, input) }?;
        let mut output = None;
        valid.take_into(&mut output)?;
        assert_eq!(output, Some(41));
        Ok(())
    })
    .unwrap();
    drop(nested);
    drop(data);
    round.finish();
}

/// 尚未接收的拥有值沿真实来源清理路径销毁一次。
#[test]
fn initialization_data_cancel_drops_unreceived_value_once() {
    let round = DataRound::new();
    let dropped = Arc::new(AtomicUsize::new(0));
    let value = OwnedData {
        bytes: vec![2, 7],
        dropped: dropped.clone(),
    };
    // SAFETY: 本轮实际来源和同一构建的拥有值。
    let data = unsafe { PublishedData::publish(round.connection.clone(), value) }.unwrap();
    drop(data);
    round.finish();
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

thread_local! {
    // 只影响当前测试线程的一次外部表返回，不增加生产状态或互斥锁。
    static PI_FFI_TEST_DATA_CONTROL_MODE: Cell<u8> = const { Cell::new(0) };
}

/// 外部宿主管理表（host control table）：其余能力继续调用真实生产入口。
static DATA_TEST_CONTROL: HostControlApiV1 = HostControlApiV1 {
    header: ApiHeaderV1 {
        major: HOST_CONTROL_MAJOR,
        size: std::mem::size_of::<HostControlApiV1>() as u32,
    },
    next_operation: CONTROL.next_operation,
    register_source: CONTROL.register_source,
    track_holding: CONTROL.track_holding,
    enter_call: Some(data_test_enter),
    leave_call: Some(data_test_leave),
    request_release: CONTROL.request_release,
    retire_source: CONTROL.retire_source,
};

/// 仅在请求 TAKE 时拒绝一次权限（permission），不创建实际调用资格。
unsafe extern "C" fn data_test_enter(
    context: *mut c_void,
    target: CallTargetV1,
    operation: OperationIdV1,
    rights: u64,
    out: *mut CallTokenV1,
) -> u32 {
    if rights == 2 && PI_FFI_TEST_DATA_CONTROL_MODE.with(|mode| mode.get() == 2) {
        PI_FFI_TEST_DATA_CONTROL_MODE.with(|mode| mode.set(0));
        return diagnostic::status_code(ErrorKind::PermissionDenied);
    }
    // SAFETY: 来源及输出由真实共同调用保护传入，本表只转交同一参数。
    unsafe { enter_call(context, target, operation, rights, out) }
}

/// 真实结束本次调用，再模拟一次错误返回；不在测试表中完成取值或补充释放能力。
unsafe extern "C" fn data_test_leave(context: *mut c_void, token: CallTokenV1) -> u32 {
    // SAFETY: 同一真实权威发出的调用标识。
    let status = unsafe { leave_call(context, token) };
    if status == 0 && PI_FFI_TEST_DATA_CONTROL_MODE.with(|mode| mode.replace(0) == 1) {
        diagnostic::status_code(ErrorKind::Internal)
    } else {
        status
    }
}

/// 原值已取走、归还调用保护失败时，原缓冲区及一次析构（Drop）责任仍在输出。
#[test]
fn initialization_data_retains_output_on_leave_failure() {
    let round = DataRound::with_table(VERSION, &DATA_TEST_CONTROL);
    let dropped = Arc::new(AtomicUsize::new(0));
    let value = OwnedData {
        bytes: vec![8, 6],
        dropped: dropped.clone(),
    };
    let address = value.bytes.as_ptr();
    // SAFETY: 同一构建的实际 D 和本轮管理表，受控测试只改变一次返回结果。
    let data = unsafe { PublishedData::publish(round.connection.clone(), value) }.unwrap();
    let mut output = None;
    data.with_input(|input| {
        // SAFETY: 类型及当前原始同步借用匹配。
        let received = unsafe { DataInput::<OwnedData>::from_input(&round.connection, input) }?;
        PI_FFI_TEST_DATA_CONTROL_MODE.with(|mode| mode.set(1));
        assert_eq!(
            *received
                .take_into(&mut output)
                .err()
                .unwrap()
                .current_context(),
            ErrorKind::Internal
        );
        assert!(output.is_some());
        // SAFETY: 同一原始输入仍有效，实际空槽必须拒绝二次消费。
        let repeated = unsafe { DataInput::<OwnedData>::from_input(&round.connection, input) }?;
        let mut extra = None;
        assert_eq!(
            *repeated
                .take_into(&mut extra)
                .err()
                .unwrap()
                .current_context(),
            ErrorKind::InvalidState
        );
        assert!(extra.is_none());
        Ok(())
    })
    .unwrap();
    let output = output.unwrap();
    assert_eq!(output.bytes.as_ptr(), address);
    assert_eq!(output.bytes, [8, 6]);
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    drop(output);
    drop(data);
    round.finish();
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

/// 权限（permission）被拒不取值；之后合法请求仍能得到原值。
#[test]
fn initialization_data_denied_take_preserves_value() {
    let round = DataRound::with_table(VERSION, &DATA_TEST_CONTROL);
    // SAFETY: 本轮真实来源及同一构建的原值。
    let data = unsafe { PublishedData::publish(round.connection.clone(), 53u64) }.unwrap();
    data.with_input(|input| {
        // SAFETY: 当前原始借用和实际类型匹配。
        let received = unsafe { DataInput::<u64>::from_input(&round.connection, input) }?;
        PI_FFI_TEST_DATA_CONTROL_MODE.with(|mode| mode.set(2));
        let mut output = None;
        assert_eq!(
            *received
                .take_into(&mut output)
                .err()
                .unwrap()
                .current_context(),
            ErrorKind::PermissionDenied
        );
        assert!(output.is_none());
        // SAFETY: 同一原始输入仍有效，先前未消费。
        unsafe { DataInput::<u64>::from_input(&round.connection, input) }?
            .take_into(&mut output)?;
        assert_eq!(output, Some(53));
        Ok(())
    })
    .unwrap();
    drop(data);
    round.finish();
}

/// 不同轮次、不同受限管理上下文和其它线程均不能消费原输入。
#[test]
fn initialization_data_rejects_other_round_context_and_thread() {
    let round = DataRound::new();
    let other = DataRound::with_table("1.2.3+data-peer", &CONTROL);
    let foreign_context = round_connection(&AUTHORITY, &round.id).unwrap();
    // SAFETY: 原值来自当前合法轮次；负面请求始终保护真实只读输入。
    let data = unsafe { PublishedData::publish(round.connection.clone(), 61u64) }.unwrap();
    data.with_input(|input| {
        // SAFETY: 两项连接实际有效，预核必须拒绝不同管理作用域，不消费 D。
        assert!(unsafe { DataInput::<u64>::from_input(&other.connection, input) }.is_err());
        assert!(unsafe { DataInput::<u64>::from_input(&foreign_context, input) }.is_err());
        let address = std::ptr::from_ref(input) as usize;
        let connection = round.connection.clone();
        std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    // SAFETY: 只读描述的原借用仍保活，线程在作用域内结束；此线程没有借出记录，取用应拒绝。
                    let failure = unsafe {
                        DataInput::<u64>::from_input(&connection, &*(address as *const ObjectRefV1))
                    }
                    .err()
                    .unwrap();
                    assert_eq!(*failure.current_context(), ErrorKind::Conflict);
                })
                .join()
                .unwrap();
        });
        // SAFETY: 回到原线程及管理作用域，原输入未消费。
        let mut output = None;
        unsafe { DataInput::<u64>::from_input(&round.connection, input) }?
            .take_into(&mut output)?;
        assert_eq!(output, Some(61));
        Ok(())
    })
    .unwrap();
    drop(foreign_context);
    drop(data);
    other.finish();
    round.finish();
}

/// 内层栈展开（stack unwinding）后恢复外层；整个调用退出后不遗留可用输入。
#[test]
fn initialization_data_restores_scope_after_unwind() {
    let round = DataRound::new();
    // SAFETY: 两个值均由真实本轮来源保管，测试期间代码和分配器匹配。
    let outer = unsafe { PublishedData::publish(round.connection.clone(), 71u64) }.unwrap();
    let nested = unsafe { PublishedData::publish(round.connection.clone(), 72u64) }.unwrap();
    outer
        .with_input(|input| {
            let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                nested.with_input(|_| -> Result<()> {
                    panic!("nested data scope");
                })
            }));
            assert!(failed.is_err());
            // SAFETY: 内层展开结束，原外层借用应已恢复。
            let mut output = None;
            unsafe { DataInput::<u64>::from_input(&round.connection, input) }?
                .take_into(&mut output)?;
            assert_eq!(output, Some(71));
            Ok(())
        })
        .unwrap();
    // SAFETY: 新借用必须仍合法，不能被已退出的内层记录遮蔽。
    nested
        .with_input(|input| {
            let mut output = None;
            unsafe { DataInput::<u64>::from_input(&round.connection, input) }?
                .take_into(&mut output)?;
            assert_eq!(output, Some(72));
            Ok(())
        })
        .unwrap();
    drop(nested);
    drop(outer);
    round.finish();
}
