// 复用本模块真实管理表与加载制品，只验证独立数据交接，不伪造 P2P 终端。
// 单独建立本轮共同连接，未调用不接收业务输入的元信息测试制品的初始化入口。
use pi_ffi_abi::ObjectRefV1;
use pi_ffi_adapters::data::{DataInput, PublishedData};
use std::cell::Cell;

/// 历史三参数产物在装入核对时即拒绝，不进入初始化（initialization）。
#[cfg(not(feature = "init_v1"))]
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
                &compile_fixture(&fixture_source().replace(VERSION, version)),
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
            reserve_round_record(&mut state, epoch, &image, fixture_version()).unwrap()
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

/// 明确启用旧版时，真实三参数制品（artifact）应在装入前核验中获得接纳。
#[cfg(feature = "init_v1")]
#[test]
fn compatibility_accepts_three_argument_artifact() {
    let legacy = FIXTURE.replacen("pi_ffi_initialize_v2", "pi_ffi_initialize_v1", 1)
        .replacen("    _: *const ObjectRefV1,\n) -> InitReplyV1", ") -> InitReplyV1", 1);
    let artifact = Artifact::from_binary(&compile_fixture(&legacy), VERSION);
    let package = artifact.package(ACTUAL_UUID);
    let result = loader::verify_files(&package, &build());
    assert!(result.is_ok(), "{:#?}", result.err());
    assert!(!artifact.mapped());
}

/// 缺指定真实入口（entrypoint）必须先于轮次预留及共享输入核验拒绝。
#[cfg(all(feature = "init_v1", feature = "init_v2"))]
#[test]
fn compatibility_missing_entry_is_unsupported_before_reservation() {
    let artifact = Artifact::new();
    let package = artifact.package(ACTUAL_UUID);
    let manager = Manager::process(build()).unwrap();
    let plugin = manager.load(&package, &request(ACTUAL_UUID)).unwrap();
    let epoch = loaded_epoch(&package);
    let error = AUTHORITY.reserve_round(epoch, InitializationVersion::V1, false).err().unwrap();
    assert_eq!(*error.current_context(), ErrorKind::Unsupported);
    assert!(AUTHORITY.current_round(epoch).unwrap().is_none());
    plugin.unload().unwrap();
}

/// 已允许的损坏旧导出不能被有效新版掩盖。
#[cfg(all(feature = "init_v1", feature = "init_v2"))]
#[test]
fn compatibility_invalid_allowed_export_is_not_hidden() {
    let binary = compile_fixture(&(FIXTURE.to_owned() + "\n#[no_mangle]\npub static pi_ffi_initialize_v1: u64 = 0;\n"));
    let artifact = Artifact::from_binary(&binary, VERSION);
    let failure = loader::verify_files(&artifact.package(ACTUAL_UUID), &build()).err().unwrap();
    assert_eq!(*failure.current_context(), ErrorKind::Corrupted);
    assert!(pi_result::debug_message(&failure).contains("pi_ffi_initialize_v1"));
    assert!(!artifact.mapped());
}

/// 三种宿主能力（host capabilities）各自真实核对三种制品（artifact）提供集合。
#[test]
fn compatibility_load_capability_matrix() {
    let legacy = FIXTURE.replacen("pi_ffi_initialize_v2", "pi_ffi_initialize_v1", 1)
        .replacen("    _: *const ObjectRefV1,\n) -> InitReplyV1", ") -> InitReplyV1", 1);
    let begin = legacy.find("#[no_mangle]\nextern \"C\" fn pi_ffi_initialize_v1").unwrap();
    let end = legacy[begin..].find("#[no_mangle]\nextern \"C\" fn pi_ffi_close_v1").unwrap() + begin;
    let both = FIXTURE.to_owned() + &legacy[begin..end];
    let manager = Manager::process(build()).unwrap();
    for (name, source, has_v1, has_v2) in [("v1", legacy.as_str(), true, false), ("v2", FIXTURE, false, true), ("both", both.as_str(), true, true)] {
        let artifact = Artifact::from_binary(&compile_fixture(source), VERSION);
        let package = artifact.package(ACTUAL_UUID);
        let allowed = (cfg!(feature = "init_v1") && has_v1) || (cfg!(feature = "init_v2") && has_v2);
        let checked = loader::verify_files(&package, &build());
        if !allowed {
            let error = checked.err().unwrap();
            assert_eq!(*error.current_context(), ErrorKind::NotFound);
            assert!(!artifact.mapped());
            println!("matrix host_v1={} host_v2={} artifact={name}: NotFound before mapping", cfg!(feature = "init_v1"), cfg!(feature = "init_v2"));
            continue;
        }
        assert!(checked.is_ok());
        let plugin = manager.load(&package, &request(ACTUAL_UUID)).unwrap();
        let epoch = loaded_epoch(&package);
        let image = {
            let state = AUTHORITY.state.lock();
            Arc::clone(ready_for_prepare(&state, epoch).unwrap().0)
        };
        #[cfg(feature = "init_v1")]
        {
            let selected = image.entries.initialize.select(InitializationVersion::V1);
            assert_eq!(selected.is_ok(), has_v1);
            if !has_v1 {
                assert_eq!(*selected.err().unwrap().current_context(), ErrorKind::Unsupported);
                assert_eq!(*AUTHORITY.reserve_round(epoch, InitializationVersion::V1, false).err().unwrap().current_context(), ErrorKind::Unsupported);
            } else {
                assert!(matches!(selected, Ok(crate::loader::InitializationEntry::V1(_))));
            }
        }
        #[cfg(feature = "init_v2")]
        {
            let selected = image.entries.initialize.select(InitializationVersion::V2);
            assert_eq!(selected.is_ok(), has_v2);
            if !has_v2 {
                assert_eq!(*selected.err().unwrap().current_context(), ErrorKind::Unsupported);
                assert_eq!(*AUTHORITY.reserve_round(epoch, InitializationVersion::V2, false).err().unwrap().current_context(), ErrorKind::Unsupported);
            } else {
                assert!(matches!(selected, Ok(crate::loader::InitializationEntry::V2(_))));
            }
        }
        assert!(AUTHORITY.current_round(epoch).unwrap().is_none());
        let v1_status = if cfg!(feature = "init_v1") { if has_v1 { "Available" } else { "Unsupported" } } else { "NotCompiled" };
        let v2_status = if cfg!(feature = "init_v2") { if has_v2 { "Available" } else { "Unsupported" } } else { "NotCompiled" };
        println!("matrix host_v1={} host_v2={} artifact={name}: mapped; selected_v1={v1_status}; selected_v2={v2_status}; no round reserved", cfg!(feature = "init_v1"), cfg!(feature = "init_v2"));
        drop(image);
        plugin.unload().unwrap();
    }
}

/// 用同次实际构建（matching build）的文件生成清单条目，不复制生产核验逻辑。
fn library_address_test_dependency(path: PathBuf, usage: DependencyUse) -> BuildDependency {
    let path = path.canonicalize().unwrap();
    BuildDependency {
        load_name: path.file_name().unwrap().to_str().unwrap().to_owned(),
        file: BuildFile {
            sha256: Sha256::digest(fs::read(&path).unwrap()).into(),
            path,
        },
        usage,
    }
}

/// 实际测试程序（test executable）旁的适配库（adapter crate）及该库准确导入的标准库（standard library）。
/// 标准库（standard library）路径来自本次编译器，不以另一份复制文件冒充当前映射（mapping）。
fn library_address_test_dependencies(usage: DependencyUse) -> Vec<BuildDependency> {
    let adapter = std::env::current_exe().unwrap().parent().unwrap()
        .join("libpi_ffi_adapters.so").canonicalize().unwrap();
    library_address_test_dependencies_from(adapter, usage)
}

/// 显式文件路径（file path）仅用于本模块的整库静态链接（static linking）对照，仍核对实际文件。
fn library_address_test_dependencies_from(adapter: PathBuf, usage: DependencyUse) -> Vec<BuildDependency> {
    use object::Object as _;

    let bytes = fs::read(&adapter).unwrap();
    let binary = object::File::parse(bytes.as_slice()).unwrap();
    let std_name = binary.import_libraries().unwrap()
        .map(|entry| entry.unwrap())
        .find(|entry| entry.name().starts_with(b"libstd-"))
        .map(|entry| std::str::from_utf8(entry.name()).unwrap().to_owned()).unwrap();
    let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(compiler).args(["--print", "target-libdir"]).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let standard = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim()).join(std_name);
    vec![
        library_address_test_dependency(adapter, usage),
        library_address_test_dependency(standard, DependencyUse::Plugin),
    ]
}

/// 清单（manifest）只列本测试制品及同次实际共享文件；这些准确路径的共同根为文件系统根。
/// 不装配终端（terminal）或接收业务输入，仅触达生产装入及准备前核验。
fn library_address_test_package(artifact: &Artifact, dependencies: Vec<BuildDependency>) -> PluginPackage {
    let mut manifest = artifact.manifest(ACTUAL_UUID);
    manifest.dependencies = dependencies;
    // SAFETY: 主制品是既有无业务元信息（metadata）夹具；依赖是本次实际适配库和标准库，
    // 或同一夹具的独占副本。代码、分配器（allocator）及文件在全部核对和清理期间稳定，
    // 不交接 Rust 业务对象，也不调用夹具的初始化入口（initialization entrypoint）。
    unsafe { PluginPackage::from_verified_build(PathBuf::from("/"), manifest) }.unwrap()
}

/// 核准的实际适配库（adapter crate）经生产核验接纳，成功预留后沿原失败收尾出口归还轮次（round）。
#[test]
fn test_library_address_loader_accepts_approved_mapping() {
    let artifact = Artifact::new();
    let package = library_address_test_package(&artifact, library_address_test_dependencies(DependencyUse::SharedRust));
    let manager = Manager::process(build()).unwrap();
    let plugin = manager.load(&package, &request(ACTUAL_UUID)).unwrap();
    let epoch = loaded_epoch(&package);
    let image = {
        let state = AUTHORITY.state.lock();
        Arc::clone(ready_for_prepare(&state, epoch).unwrap().0)
    };
    loader::check_inputs(&image, false).unwrap();
    let (id, connection) = AUTHORITY.reserve_round(epoch, fixture_version(), false).unwrap();
    {
        let state = AUTHORITY.state.lock();
        let round = state.rounds.iter().find(|round| same_round(&round.id, &id)).unwrap();
        assert!(matches!(round.phase, RoundPhase::Preparing));
        assert!(round.connection.is_some());
        assert!(round.inputs.is_none());
    }
    // 本用例有意不发布输入，以生产准备失败出口结束 Preparing，不手动改写管理状态（state）。
    let failure = AUTHORITY.fail_prepare(round_id(&id), Error::new(ErrorKind::InvalidInput));
    assert_eq!(*failure.current_context(), ErrorKind::InvalidInput);
    drop(connection);
    plugin.current_initialization().unwrap().unwrap().close().unwrap();
    assert!(AUTHORITY.current_round(epoch).unwrap().is_none());
    drop(image);
    plugin.unload().unwrap();
}

/// 另一真实动态映射（dynamic mapping）不能冒充 SharedRust；拒绝发生在轮次（round）与输入发布之前。
#[test]
fn test_library_address_loader_rejects_other_shared_mapping_before_reservation() {
    let mut wrong = Artifact::new();
    let path = wrong.dir.0.join("wrong-shared-adapter.so");
    fs::rename(&wrong.path, &path).unwrap();
    wrong.path = path;
    let wrong_files = loader::verify_files(&wrong.package(ACTUAL_UUID), &build()).unwrap();
    let wrong_module = loader::open(&wrong_files).unwrap();
    assert!(wrong.mapped());

    let artifact = Artifact::new();
    let mut dependencies = library_address_test_dependencies(DependencyUse::Plugin);
    dependencies.push(library_address_test_dependency(wrong.path.clone(), DependencyUse::SharedRust));
    let package = library_address_test_package(&artifact, dependencies);
    let manager = Manager::process(build()).unwrap();
    let plugin = manager.load(&package, &request(ACTUAL_UUID)).unwrap();
    let epoch = loaded_epoch(&package);
    let image = {
        let state = AUTHORITY.state.lock();
        Arc::clone(ready_for_prepare(&state, epoch).unwrap().0)
    };
    assert!(loader::mapped_identity(&wrong_module).unwrap() != image.identity);
    let failure = loader::check_inputs(&image, false).err().unwrap();
    assert_eq!(*failure.current_context(), ErrorKind::Dependency);
    assert!(pi_result::debug_message(&failure).contains("host shared Rust adapter"));
    let before = {
        let state = AUTHORITY.state.lock();
        (state.next_identifier, state.rounds.len(), state.sources.len(), state.holdings.len())
    };
    let failure = AUTHORITY.reserve_round(epoch, fixture_version(), false).err().unwrap();
    assert_eq!(*failure.current_context(), ErrorKind::Dependency);
    assert!(pi_result::debug_message(&failure).contains("host shared Rust adapter"));
    assert!(AUTHORITY.current_round(epoch).unwrap().is_none());
    {
        let state = AUTHORITY.state.lock();
        assert_eq!(before, (state.next_identifier, state.rounds.len(), state.sources.len(), state.holdings.len()));
    }
    drop(image);
    plugin.unload().unwrap();
    loader::close(wrong_module).unwrap();
}

/// 只向系统查询地址归属（address ownership），不读取所给对象；复制系统返回的名称后即结束借用。
fn library_address_test_owner(address: *const core::ffi::c_void) -> (PathBuf, usize) {
    use std::os::unix::ffi::OsStrExt;

    let mut info = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
    // SAFETY: dladdr 按数值查询受保护映射（mapping）中的地址，输出槽（output slot）大小准确、独占可写。
    assert_ne!(unsafe { libc::dladdr(address, info.as_mut_ptr()) }, 0);
    // SAFETY: 成功返回后系统已初始化完整 Dl_info；映射（mapping）在整个测试期间存续。
    let info = unsafe { info.assume_init() };
    assert!(!info.dli_fname.is_null());
    // SAFETY: 成功记录的名称由系统提供，以零结尾；在当前映射（mapping）保护内立即复制，不保留其指针。
    let name = unsafe { std::ffi::CStr::from_ptr(info.dli_fname) };
    (PathBuf::from(std::ffi::OsStr::from_bytes(name.to_bytes())).canonicalize().unwrap(), info.dli_fbase.addr())
}

/// 查询的数据地址属于实际适配库（adapter crate）；测试私有静态对象（static object）属于宿主测试映像（image）。
/// 后者只是静态数据归属对照，不代表整库静态链接（static linking）反例。
#[test]
fn test_library_address_owner_matches_actual_adapter_file() {
    use std::os::unix::fs::MetadataExt;

    static PI_FFI_TEST_LIBRARY_ADDRESS: u8 = 0;
    let expected = library_address_test_dependencies(DependencyUse::SharedRust).remove(0).file.path;
    let (actual, adapter_base) = library_address_test_owner(pi_ffi_adapters::library_address());
    println!("适配库（adapter crate）地址归属：{}；核准文件：{}", actual.display(), expected.display());
    let expected = fs::metadata(expected).unwrap();
    let actual = fs::metadata(actual).unwrap();
    assert_eq!((actual.dev(), actual.ino()), (expected.dev(), expected.ino()));
    let (host, host_base) = library_address_test_owner(core::ptr::addr_of!(PI_FFI_TEST_LIBRARY_ADDRESS).cast());
    let host = fs::metadata(host).unwrap();
    let executable = fs::metadata(std::env::current_exe().unwrap()).unwrap();
    assert_eq!((host.dev(), host.ino()), (executable.dev(), executable.ino()));
    assert_ne!((host.dev(), host.ino()), (actual.dev(), actual.ino()));
    assert_ne!(host_base, adapter_base);
}

/// 仅在隔离副本把实际适配库（adapter crate）整库静态链接（static linking）时显式运行。
/// 即使原产品动态库（dylib）真实映射且核准，静态副本的数据地址仍不能通过原准备前核验。
#[test]
#[ignore = "需要实际适配库整库 rlib 构建及 PI_FFI_TEST_APPROVED_ADAPTER_FILE 指向原产品 dylib"]
fn test_library_address_static_adapter_is_rejected_before_reservation() {
    use std::os::unix::fs::MetadataExt;

    let adapter = PathBuf::from(std::env::var_os("PI_FFI_TEST_APPROVED_ADAPTER_FILE").unwrap()).canonicalize().unwrap();
    // SAFETY: 文件来自同次真实构建的共享适配库（shared adapter library），依赖及标准库（standard library）
    // 与本测试一致。只建立系统持有（system reference），不解析或调用其 Rust 入口，不交接对象，
    // 不跨该库边界构造或消费原生输入（native input）。系统返回的名称即时复制为自有路径（path）。
    // 持有覆盖文件／地址核验及插件（plugin）清理；末尾已无其代码／数据借用，再显式关闭。
    let shared = unsafe { libloading::Library::new(&adapter) }.unwrap();
    let (owner, _) = library_address_test_owner(pi_ffi_adapters::library_address());
    let owner = fs::metadata(owner).unwrap();
    let executable = fs::metadata(std::env::current_exe().unwrap()).unwrap();
    let approved = fs::metadata(&adapter).unwrap();
    assert_eq!((owner.dev(), owner.ino()), (executable.dev(), executable.ino()));
    assert_ne!((owner.dev(), owner.ino()), (approved.dev(), approved.ino()));

    let artifact = Artifact::new();
    let package = library_address_test_package(&artifact, library_address_test_dependencies_from(adapter, DependencyUse::SharedRust));
    let manager = Manager::process(build()).unwrap();
    let plugin = manager.load(&package, &request(ACTUAL_UUID)).unwrap();
    let epoch = loaded_epoch(&package);
    let image = {
        let state = AUTHORITY.state.lock();
        Arc::clone(ready_for_prepare(&state, epoch).unwrap().0)
    };
    let failure = loader::check_inputs(&image, false).err().unwrap();
    assert_eq!(*failure.current_context(), ErrorKind::Dependency);
    assert!(pi_result::debug_message(&failure).contains("host shared Rust adapter"));
    let before = {
        let state = AUTHORITY.state.lock();
        (state.next_identifier, state.rounds.len(), state.sources.len(), state.holdings.len())
    };
    let failure = AUTHORITY.reserve_round(epoch, fixture_version(), false).err().unwrap();
    assert_eq!(*failure.current_context(), ErrorKind::Dependency);
    assert!(pi_result::debug_message(&failure).contains("host shared Rust adapter"));
    assert!(AUTHORITY.current_round(epoch).unwrap().is_none());
    {
        let state = AUTHORITY.state.lock();
        assert_eq!(before, (state.next_identifier, state.rounds.len(), state.sources.len(), state.holdings.len()));
    }
    drop(image);
    plugin.unload().unwrap();
    shared.close().unwrap();
}
