//! 未改原库的共享缓存插件（shared cache plugin），显式提供三类标准 C 接口（C APIs）的四个版本符号。
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use pi_ffi::{abi::*, plugin::{Declaration, Exports, Inputs, InputsV1}};
use pi_p2p_service::P2pServiceTerminal;
use pi_service::{CollectedService, MultiLayerCodec, ServiceError};
use pi_result::{Error, ErrorKind, Result};

type InitializationData = (AtomicUsize, Arc<AtomicUsize>, Option<Box<dyn Send + Sync>>);
#[cfg(feature = "optional-runtime")]
type Execution = pi_async_runtime::Handle;
#[cfg(not(feature = "optional-runtime"))]
type Execution = ();
type LegacyInputs = InputsV1<P2pServiceTerminal<MultiLayerCodec>, Execution>;
type PluginInputs = Inputs<P2pServiceTerminal<MultiLayerCodec>, Execution, InitializationData>;
enum State {
    V1(LegacyInputs),
    V2(PluginInputs),
}
const VERSION: &str = env!("CARGO_PKG_VERSION");
const fn version_bytes() -> [u8; VERSION.len()] {
    let mut bytes = [0; VERSION.len()];
    let mut i = 0;
    while i < bytes.len() { bytes[i] = VERSION.as_bytes()[i]; i += 1; }
    bytes
}
#[unsafe(link_section = ".pi_ffi.version")]
static ARTIFACT_VERSION: [u8; VERSION.len()] = version_bytes();
const DECLARATION: Declaration = Declaration {
    uuid: [0x29, 0x57, 0, 0x98, 0xaa, 0x48, 0x46, 0x8c, 0xb5, 0x12, 0x63, 0xc2, 0x6b, 0x24, 0xa9, 0x31],
    version: match std::str::from_utf8(&ARTIFACT_VERSION) { Ok(value) => value, Err(_) => panic!("UTF-8") },
};
#[cfg(not(feature = "macro-exports"))]
static EXPORTS: Exports<State> = Exports::new(DECLARATION);

/// 观察记录（observation record）只标记标准回调（standard callbacks）的实际执行。
fn event(value: &str) {
    use std::io::Write;
    let file = std::env::var_os("PI_FFI_ROUND_OBSERVE_FILE").expect("观察文件（observation file）");
    let mut output = std::fs::OpenOptions::new().create(true).append(true).open(file).unwrap();
    writeln!(output, "{value}").unwrap();
}

fn initialize_v1(inputs: LegacyInputs, state: &mut Option<State>) -> Result<()> {
    *state = Some(State::V1(inputs));
    let Some(State::V1(active)) = state.as_mut() else { unreachable!() };
    println!("plugin initialization_callback_version=1 data_input=false");
    initialize_common(active.terminal.native(), &mut active.config)
}

fn initialize_v2(inputs: PluginInputs, state: &mut Option<State>) -> Result<()> {
    *state = Some(State::V2(inputs));
    let Some(State::V2(active)) = state.as_mut() else { unreachable!() };
    assert!(active.data.0.load(Ordering::SeqCst) >= 100);
    assert_eq!(active.data.1.fetch_add(1, Ordering::SeqCst), 0);
    println!("plugin initialization_callback_version=2 non_clone_data=true");
    initialize_common(active.terminal.native(), &mut active.config)
}

fn initialize_common(terminal: &P2pServiceTerminal<MultiLayerCodec>, config: &mut pi_ffi::config::Config) -> Result<()> {
    use pi_ffi::config::pi_config::Value;
    if matches!(config.get("fail_initialization"), Some(Value::Bool(true))) {
        event("initialize_fail");
        return Err(Error::new(ErrorKind::InvalidInput).attach("explicit fixture initialization failure"));
    }
    let runtime = P2pServiceTerminal::service_runtime(terminal).ok_or_else(||
        Error::new(ErrorKind::Internal).attach("terminal has no local ServiceRuntime"))?;
    let convert = |error: ServiceError| Error::new(ErrorKind::Internal).attach(error.to_string());
    // 只借用同一服务运行时（ServiceRuntime），没有新建或留存其克隆（clone）。
    // 下列服务注册（service registration）完全位于插件（plugin）内部。
    let mut fory = fory::Fory::default();
    pi_shared_cache_service::service::register_cache_service_types(&mut fory)
        .map_err(|error| Error::new(ErrorKind::Internal).attach(format!("{error:?}")))?;
    runtime.codec().register_layer_fory("shared_cache", Arc::new(fory));
    if runtime.contexts().get_only_read::<pi_shared_cache_service::CacheRegistry>().is_none() {
        runtime.contexts().insert_only_read(Arc::new(pi_shared_cache_service::CacheRegistry::from_global()));
        runtime.contexts().insert_only_read(Arc::new(pi_shared_cache_service::ContainerRegistry::new()));
        let business = pi_async_rt::rt::AsyncRuntimeBuilder::default_multi_thread(None, None, None, None);
        runtime.contexts().insert_only_read(Arc::new(pi_shared_cache_service::storage::CacheBusinessRuntime::new(business)));
    }
    let services: Vec<_> = pi_service::inventory::iter::<CollectedService<MultiLayerCodec>>
        .into_iter().map(|item| item.svc).filter(|service| service.layer() == "shared_cache").collect();
    if services.len() != 59 {
        return Err(Error::new(ErrorKind::Internal).attach(format!("expected original 59 cache services, got {}", services.len())));
    }
    for service in services {
        let port = service.port();
        if let Some(metadata) = runtime.get_meta(port) {
            if metadata.id != service.id() {
                return Err(Error::new(ErrorKind::Conflict).attach(format!("static port conflict: {port}")));
            }
        } else {
            runtime.registry().register(service).map_err(convert)?;
        }
        runtime.registry().init(runtime, port).map_err(convert)?;
    }
    let label = match config.get("probe") { Some(Value::Str(value)) => value.as_str(), _ => "missing" };
    event(&format!("initialize:{label}"));
    Ok(())
}

/// 关闭（close）只处理本轮持有；传入的共享运行时（runtime）和终端（terminal）不关闭。
/// 静态服务（static services）及其内部资源按原库语义管理，对宿主（host）保持透明。
fn close(state: &mut Option<State>) -> Result<()> {
    drop(state.take());
    event("close");
    Ok(())
}

// SAFETY: 遵约宿主（host）保活同一动态库（dynamic library），提供有效且独占的输出槽（output slot）。
#[cfg(not(feature = "macro-exports"))]
#[unsafe(no_mangle)]
unsafe extern "C" fn pi_ffi_metadata_v1(out: *mut MetadataV1) -> OutcomeV1 {
    unsafe { EXPORTS.metadata(out) }
}
// SAFETY: 遵约宿主（host）在标准调用（standard call）及交接期间保活四个输入（inputs）。
#[cfg(not(feature = "macro-exports"))]
#[unsafe(no_mangle)]
unsafe extern "C" fn pi_ffi_initialize_v2(runtime: *const ObjectRefV1, terminal: *const ObjectRefV1, config: *const ObjectRefV1, data: *const ObjectRefV1) -> InitReplyV1 {
    let fault = std::env::var("PI_FFI_DATA_FAULT").unwrap_or_default();
    if fault == "preconsumed-config" || fault == "data-drop-failure" {
        // SAFETY: 本测试宿主同步保活原始输入；故意预先取空配置，验证后续真实部分消费收尾。
        let consumed = unsafe { pi_ffi::config::ConfigInput::from_input(&*config) }.unwrap().take().unwrap();
        drop(consumed);
    }
    let data = if fault == "missing-data" { std::ptr::null() } else { data };
    // SAFETY: 同一真实宿主调用仍保护全部输入；重入只用于验证生产接入的 Busy 处理。
    let mut busy_checked = false;
    let first = unsafe { EXPORTS.initialize_without_runtime(runtime, terminal, config, data, |inputs, state| {
        let busy = unsafe { EXPORTS.initialize_without_runtime(runtime, terminal, config, data, initialize_v2) };
        assert_eq!(busy.outcome.kind, pi_ffi_core::diagnostic::status_code(ErrorKind::Busy));
        assert!(busy.outcome.diagnostic.context.is_null());
        check_close_busy(runtime, terminal, config);
        busy_checked = true;
        initialize_v2(inputs, state)
    }) };
    if data.is_null() {
        // SAFETY: 缺失必传项的重复调用仍受原模块保护；此分支从未接纳轮次，不能测试已绑定输入替换。
        let repeated = unsafe { EXPORTS.initialize_without_runtime(runtime, terminal, config, data, initialize_v2) };
        assert_eq!(first.progress, repeated.progress);
        assert_eq!(first.outcome.kind, repeated.outcome.kind);
        assert!(repeated.outcome.diagnostic.context.is_null());
        println!("plugin c_entry replay_stable=true in_flight_checked=false replacement_checked=false not_admitted=true");
        return first;
    }
    // SAFETY: 当前原始配置借用仍有效；此处只建立连接，不再次取配置。
    let connection = unsafe { pi_ffi::config::ConfigInput::from_input(&*config) }.unwrap();
    // SAFETY: 保活原模块、真实轮次和所有输入，直接复验插件接入自身的稳定结果。
    let repeated = unsafe { EXPORTS.initialize_without_runtime(runtime, terminal, config, data, initialize_v2) };
    assert_eq!(first.progress, repeated.progress);
    assert_eq!(first.outcome.kind, repeated.outcome.kind);
    // SAFETY: 独立诊断来自本轮当前仍保活的模块与来源，在归还输入前接收并归还一次。
    let replay = unsafe { pi_ffi_core::diagnostic::decode_management(Some(connection.connection()), repeated.outcome) };
    if first.outcome.kind == 0 { replay.unwrap(); }
    else { assert_eq!(pi_ffi_core::diagnostic::status_code(*replay.unwrap_err().current_context()), first.outcome.kind); }
    let cross = unsafe { EXPORTS.initialize_v1_without_runtime(runtime, terminal, config, initialize_v1) };
    check_failure(connection.connection(), cross.outcome, ErrorKind::Conflict);
    println!("plugin cross_version_conflict=true primary=2");
    let replacement: InitializationData = (AtomicUsize::new(999), Arc::new(AtomicUsize::new(0)), None);
    // SAFETY: 同一轮的合法受限连接及同一实际 D，独立登记另一份输入用于验证替换拒绝。
    let replacement = unsafe { pi_ffi::data::PublishedData::publish(connection.connection().clone(), replacement) }.unwrap();
    replacement.with_input(|changed| {
        // SAFETY: 四项真实输入均被同步保活，唯一变化是同轮另一份数据租约。
        let conflict = unsafe { EXPORTS.initialize_without_runtime(runtime, terminal, config, changed, initialize_v2) };
        assert_eq!(conflict.outcome.kind, pi_ffi_core::diagnostic::status_code(ErrorKind::Conflict));
        assert!(conflict.outcome.diagnostic.context.is_null());
        Ok(())
    }).unwrap();
    println!("plugin c_entry replay_stable=true in_flight_checked={busy_checked} replacement_rejected=true");
    first
}
// SAFETY: 遵约宿主（host）保活模块（module），并使用真实本轮标识（round identifier）。
#[cfg(not(feature = "macro-exports"))]
#[unsafe(no_mangle)]
unsafe extern "C" fn pi_ffi_close_v1(id: InitIdV1) -> OutcomeV1 {
    unsafe { EXPORTS.close(id, close) }
}
#[cfg(not(feature = "macro-exports"))]
const _: MetadataFnV1 = pi_ffi_metadata_v1;
#[cfg(not(feature = "macro-exports"))]
const _: InitializeFnV2 = pi_ffi_initialize_v2;
#[cfg(not(feature = "macro-exports"))]
const _: CloseFnV1 = pi_ffi_close_v1;


#[cfg(not(feature = "macro-exports"))]
fn check_failure(connection: &pi_ffi_core::control::Connection, outcome: OutcomeV1, expected: ErrorKind) {
    // SAFETY: 诊断（diagnostic）来自原轮当前保活的模块，仍在实际输入借用期内。
    let failure = unsafe { pi_ffi_core::diagnostic::decode_management(Some(connection), outcome) }.unwrap_err();
    assert_eq!(*failure.current_context(), expected);
}

#[cfg(not(feature = "macro-exports"))]
fn check_close_busy(_runtime: *const ObjectRefV1, _terminal: *const ObjectRefV1, config: *const ObjectRefV1) {
    // SAFETY: 标准回调（standard callback）执行期间，宿主仍保活本轮原始配置输入。
    let input = unsafe { &*config };
    let id = InitIdV1 { module_epoch: input.lease.initialization.module_epoch, round: input.lease.initialization.round };
    let connection = unsafe { pi_ffi::config::ConfigInput::from_input(input) }.unwrap();
    let outcome = unsafe { EXPORTS.close(id, close) };
    check_failure(connection.connection(), outcome, ErrorKind::Busy);
    println!("plugin close_in_flight_busy=true");
}

// SAFETY: 真实宿主（host）同步保活三项原始输入及本模块，只有运行时（runtime）可空。
#[cfg(not(feature = "macro-exports"))]
#[unsafe(no_mangle)]
unsafe extern "C" fn pi_ffi_initialize_v1(runtime: *const ObjectRefV1, terminal: *const ObjectRefV1, config: *const ObjectRefV1) -> InitReplyV1 {
    let mut busy_checked = false;
    let first = unsafe { EXPORTS.initialize_v1_without_runtime(runtime, terminal, config, |inputs, state| {
        let connection = unsafe { pi_ffi::config::ConfigInput::from_input(&*config) }.unwrap();
        let busy = unsafe { EXPORTS.initialize_v1_without_runtime(runtime, terminal, config, initialize_v1) };
        check_failure(connection.connection(), busy.outcome, ErrorKind::Busy);
        check_close_busy(runtime, terminal, config);
        busy_checked = true;
        initialize_v1(inputs, state)
    }) };
    let connection = unsafe { pi_ffi::config::ConfigInput::from_input(&*config) }.unwrap();
    let repeated = unsafe { EXPORTS.initialize_v1_without_runtime(runtime, terminal, config, initialize_v1) };
    assert_eq!(first.progress, repeated.progress);
    assert_eq!(first.outcome.kind, repeated.outcome.kind);
    if first.outcome.kind == 0 {
        // SAFETY: 该独立重放结果在原代码及来源保护中接收并归还。
        unsafe { pi_ffi_core::diagnostic::decode_management(Some(connection.connection()), repeated.outcome) }.unwrap();
    } else {
        let kind = if first.outcome.kind == pi_ffi_core::diagnostic::status_code(ErrorKind::InvalidInput) { ErrorKind::InvalidInput } else { panic!("unexpected v1 failure") };
        check_failure(connection.connection(), repeated.outcome, kind);
    }
    // 此数据仅属于明确拒绝的 v2 请求；原 v1 准备和绑定没有第四输入。
    let probe: InitializationData = (AtomicUsize::new(999), Arc::new(AtomicUsize::new(0)), None);
    let probe = unsafe { pi_ffi::data::PublishedData::publish(connection.connection().clone(), probe) }.unwrap();
    probe.with_input(|data| {
        let cross = unsafe { EXPORTS.initialize_without_runtime(runtime, terminal, config, data, initialize_v2) };
        check_failure(connection.connection(), cross.outcome, ErrorKind::Conflict);
        Ok(())
    }).unwrap();
    assert!(busy_checked);
    println!("plugin cross_version_conflict=true primary=1 replay_stable=true in_flight_checked=true");
    first
}
#[cfg(not(feature = "macro-exports"))]
const _: InitializeFnV1 = pi_ffi_initialize_v1;

#[cfg(all(feature = "macro-exports", not(feature = "optional-runtime")))]
pi_ffi_macros::export_plugin! { declaration: DECLARATION, state: State, versions: [metadata_v1, init_v1, init_v2, close_v1], initialize: { init_v1: initialize_v1, init_v2: initialize_v2 }, close: close, runtime: none }

#[cfg(feature = "optional-runtime")]
pi_ffi_macros::export_plugin! { declaration: DECLARATION, state: State, versions: [metadata_v1, init_v1, init_v2, close_v1], initialize: { init_v1: initialize_v1, init_v2: initialize_v2 }, close: close, runtime: optional(pi_async_runtime::Handle) }
