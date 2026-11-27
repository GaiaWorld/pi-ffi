//! 真实插件（plugin）宿主（host），只测试三个标准接口（APIs）及输入边界（input boundary）。
use base64::{engine::general_purpose::STANDARD, Engine as _};
use pi_ffi::config::pi_config::{ConfigBuilder, TableEntry, TableMap, Value};
use pi_ffi::host::*;
use pi_p2p::{AuthMode, HostId, KvEntry, P2pConfig, RegistryQuery};
use pi_p2p_service::{P2pServiceBuilder, P2pServiceTerminal, ServiceStatusSync, registry_sync::ServiceRecordWire};
use pi_service::{MultiLayerCodec, Service, ServiceAccessMode, ServiceContexts, ServiceError,
    ServiceId, ServicePort, ServiceRuntime, ServiceStatus};
use pi_result::ErrorKind;
use sha2::{Digest, Sha256};
mod observation;
use std::{fs, io::Write, os::unix::fs::MetadataExt, path::{Path, PathBuf},
    sync::{Arc, atomic::{AtomicUsize, Ordering}}, thread, time::{Duration, Instant}};
const UUID: [u8;16] = [0x29,0x57,0,0x98,0xaa,0x48,0x46,0x8c,0xb5,0x12,0x63,0xc2,0x6b,0x24,0xa9,0x31];
const VERSION: &str = "1.5.0+build.alpha";
const STAGE_TIMEOUT: Duration = Duration::from_secs(20);
const CONTROL_PORT: u16 = 1902;
type InitializationData = (AtomicUsize, Arc<AtomicUsize>, Option<Box<dyn Send + Sync>>);
type Terminal = P2pServiceTerminal<MultiLayerCodec>;
fn wait_for<T>(what: &str, mut observe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + STAGE_TIMEOUT;
    loop {
        if let Some(value) = observe() {
            return value;
        }
        assert!(Instant::now() < deadline, "等待超时（timeout） {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn signal(root: &Path, name: &str, value: &str) {
    let pending = root.join(format!("{name}.pending"));
    fs::write(&pending, value).unwrap();
    fs::rename(pending, root.join(name)).unwrap();
}

fn wait_signal(root: &Path, name: &str) -> String {
    wait_for(name, || match fs::read_to_string(root.join(name)) {
        Ok(value) => Some(value),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("{name} {error}"),
    })
}

fn digest(path: &Path) -> [u8; 32] {
    Sha256::digest(fs::read(path).unwrap()).into()
}

fn build_file(root: &Path, name: &str) -> BuildFile {
    BuildFile {
        path: name.into(),
        sha256: digest(&root.join(name)),
    }
}

fn build_manifest(root: &Path, version: Version, host: &HostBuild) -> BuildManifest {
    let rust_std = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .find(|name| name.starts_with("libstd-") && name.ends_with(".so"))
        .unwrap();
    let dependencies = ["libpi_ffi_adapters.so", rust_std.as_str()]
        .into_iter()
        .map(|name| BuildDependency {
            file: build_file(root, name),
            load_name: name.to_owned(),
            usage: DependencyUse::SharedRust,
        })
        .collect();
    BuildManifest {
        target: Target::LinuxX86_64Gnu,
        uuid: UUID,
        version,
        library: build_file(root, "libcache_ffi_plugin.so"),
        dependencies,
        rust_build_sha256: host.rust_build_sha256,
        allocator: host.allocator.clone(),
        static_declarations: StaticDeclarations::PrivateInventory,
    }
}

fn mapping_and_file_handles(file: &Path) -> (bool, usize) {
    let artifact = fs::metadata(file).unwrap();
    let is_artifact = |path: &Path| {
        fs::metadata(path).is_ok_and(|observed| {
            (observed.dev(), observed.ino()) == (artifact.dev(), artifact.ino())
        })
    };
    let mapped = fs::read_to_string("/proc/self/maps").unwrap().lines().any(|line| {
        line.find('/').is_some_and(|index| is_artifact(Path::new(&line[index..])))
    });
    let handles = fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter(|entry| is_artifact(&entry.as_ref().unwrap().path()))
        .count();
    (mapped, handles)
}

fn finish_round(round: &Initialization) {
    wait_for("原轮关闭及实际输入归还", || match round.close() {
        Ok(()) => Some(()),
        Err(error) if *error.current_context() == ErrorKind::Busy => None,
        Err(error) => panic!("{error:?}"),
    });
}


/// 独立宿主（host）服务用于证明插件关闭（plugin close）没有关闭共享输入（shared inputs）。
struct Control;
static CONTROL: Control = Control;
impl Service for Control {
    type Codec = MultiLayerCodec;
    fn id(&self) -> ServiceId { "/core/cache-ffi-control".into() }
    fn port(&self) -> ServicePort { CONTROL_PORT.into() }
    fn layer(&self) -> &'static str { "core" }
    fn mode(&self) -> ServiceAccessMode { ServiceAccessMode::Rpc }
    fn version(&self) -> pi_service::ServiceVersion { "1.0.0".into() }
    fn bind_params(&self, _: &ServiceContexts) -> Result<Arc<dyn std::any::Any + Send + Sync>, ServiceError> { Ok(Arc::new(())) }
    fn init(&self, _: &ServiceRuntime<MultiLayerCodec>, _: Arc<dyn std::any::Any + Send + Sync>) -> Result<(), ServiceError> { Ok(()) }
    fn handle_send(&self, _: ServiceRuntime<MultiLayerCodec>, _: Arc<dyn std::any::Any + Send + Sync>, _: Vec<u8>)
        -> futures::future::BoxFuture<'static, Result<(), ServiceError>> { Box::pin(async { Ok(()) }) }
    fn handle_rpc(&self, _: ServiceRuntime<MultiLayerCodec>, _: Arc<dyn std::any::Any + Send + Sync>, input: Vec<u8>)
        -> futures::future::BoxFuture<'static, Result<Vec<u8>, ServiceError>> {
        let mut result = b"control/".to_vec(); result.extend(input); Box::pin(async move { Ok(result) })
    }
}

fn terminal(root: &Path, port: u16) -> Arc<Terminal> {
    let mut config = P2pConfig::default();
    config.identity.keypair_path = root.join("host.key").to_string_lossy().into_owned();
    config.listen_addrs = vec![format!("/ip4/127.0.0.1/udp/{port}/quic-v1")];
    config.mdns.enabled = false; config.mdns.auto_dial = false;
    config.metrics.enabled = false; config.metrics.runtime_toggle = false; config.logs.enabled = false;
    config.auth.mode = AuthMode::Disabled; config.governance.enabled = false;
    config.liveness.phi.enabled = false; config.liveness.ping.enabled = false;
    config.swarm_runtime.worker_threads = Some(2); config.lane.count = Some(2);
    config.ipc.enabled = true; config.ipc.shared_root = root.join("ipc").to_string_lossy().into_owned();
    let codec = MultiLayerCodec::default();
    codec.register_layer_fory("core", Arc::new(fory::Fory::default()));
    let status = Arc::new(ServiceStatusSync::new());
    let runtime = pi_service::ServiceRuntimeBuilder::<MultiLayerCodec>::new()
        .custom_codec(codec).status_listener(status.clone()).build();
    runtime.registry().register(&CONTROL).unwrap();
    runtime.registry().init(&runtime, CONTROL_PORT.into()).unwrap();
    let terminal = futures::executor::block_on(P2pServiceBuilder::new(runtime, config)
        .with_status_listener(status).auto_ipc(true).prefer_ipc(true).with_debug_label("ffi-host")
        .build_with_terminal()).unwrap();
    Arc::new(terminal)
}

/// 在初始化（initialization）返回后立即读取本机注册表（registry），核对每个固定端口的发布状态。
fn local_cache_publication(terminal: &Terminal, peer: HostId) -> KvEntry {
    let prefix = format!("service/{peer}/shared_cache/");
    let snapshot = terminal.registry().snapshot(RegistryQuery {
        prefix: Some(prefix), peer_id: Some(peer), host_names: None,
        include_deleted: false, include_internal: false, key_predicate: None, limit: None, offset: 0,
    });
    let mut codec = fory::Fory::default();
    codec.register::<ServiceRecordWire>(1).unwrap();
    let mut ports = Vec::with_capacity(snapshot.entries.len());
    let mut get_meta = None;
    for entry in snapshot.entries {
        let bytes = STANDARD.decode(&entry.value).unwrap();
        let wire: ServiceRecordWire = codec.deserialize_from(&mut fory::Reader::new(&bytes)).unwrap();
        assert_eq!(wire.peer_id, peer.to_string());
        assert_eq!(wire.status_code, i8::from(ServiceStatus::Running), "{} 状态须为运行中（Running）", entry.key);
        ports.push(wire.port);
        if wire.port == 3959 {
            assert_eq!(entry.key, format!("service/{peer}/shared_cache/shared_container/table/get_meta"));
            get_meta = Some(entry);
        }
    }
    ports.sort_unstable();
    assert_eq!(ports, (3941..=3999).collect::<Vec<u16>>());
    get_meta.expect("本机注册表（registry）缺少端口 3959")
}

/// 客户端（client）完成业务后，核对同一注册表（registry）条目的版本与内容仍一致。
fn assert_local_publication_preserved(terminal: &Terminal, expected: &KvEntry) {
    let entry = terminal.registry().get_versioned(&expected.key).expect("本机发布记录必须保留");
    assert!(!entry.deleted);
    assert_eq!(entry.peer_id, expected.peer_id);
    assert_eq!(entry.version, expected.version);
    assert_eq!(entry.updated_at_ms, expected.updated_at_ms);
    assert_eq!(entry.value, expected.value);
}


/// 每次真实调用标准元信息接口（metadata API），不把管理器（manager）缓存查询计入次数。
fn metadata_queries(artifact: &Path, effects: &Path, phase: &str) -> usize {
    let before = match fs::read_to_string(effects) { Ok(v) => v, Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(), Err(e) => panic!("{e}") };
    // SAFETY: 同批可信构建（matching trusted build），额外系统引用（system reference）仅保活当前调用。
    let library = unsafe { libloading::Library::new(artifact) }.unwrap();
    // SAFETY: 三个标准符号（standard symbols）的冻结签名（frozen signatures）与实际插件（plugin）一致。
    let metadata = unsafe { library.get::<pi_ffi::abi::MetadataFnV1>(b"pi_ffi_metadata_v1\0") }.unwrap();
    for _ in 0..2 {
        let mut output = std::mem::MaybeUninit::<pi_ffi::abi::MetadataV1>::uninit();
        // SAFETY: 当前库引用（library reference）保活输出字节（output bytes），输出槽（output slot）独占且有效。
        let outcome = unsafe { metadata(output.as_mut_ptr()) };
        assert_eq!(outcome.kind, 0); assert_eq!(outcome.diagnostic_failure_kind, 0);
        // SAFETY: 成功结果（successful result）保证标准输出（standard output）完整初始化。
        let value = unsafe { output.assume_init() };
        assert_eq!(value.uuid.bytes, UUID);
        assert_eq!(value.version_len, VERSION.len() as u64);
        assert!(!value.version.is_null());
        // SAFETY: 可信标准输出（trusted standard output）指向当前仍保活映像（live image）的定长不可变字节。
        let bytes = unsafe { std::slice::from_raw_parts(value.version, value.version_len as usize) };
        assert_eq!(bytes, VERSION.as_bytes());
    }
    drop(metadata); drop(library);
    let after = match fs::read_to_string(effects) { Ok(v) => v, Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(), Err(e) => panic!("{e}") };
    assert_eq!(before, after, "元信息查询（metadata query）不得触发生命周期副作用（lifecycle side effects）");
    println!("host metadata_api_phase={phase} actual_calls=2 uuid_ok=true full_semver_ok=true side_effects=false");
    2
}

fn configuration(label: &str, fail: bool) -> pi_ffi::config::pi_config::Config {
    let mut values = TableMap::new(Default::default());
    values.insert("probe".into(), TableEntry::plain(Value::Str(label.into())));
    values.insert("fail_initialization".into(), TableEntry::plain(Value::Bool(fail)));
    ConfigBuilder::from_base_value(Value::Table(values)).build().unwrap()
}

/// 按原轮明确选版（version selection）；v1 分支从未构造 D。
fn prepare_version(plugin: &Plugin, terminal: &Arc<Terminal>, label: &str, fail: bool, version: u8, ordinal: usize)
    -> (Initialization, Option<Arc<AtomicUsize>>) {
    // SAFETY: 同一配套构建（matching build）的真实终端，宿主持续持有。
    let input = unsafe { TerminalInput::new(terminal.clone()) };
    println!("host selected_initialization_version={version} fail={fail}");
    match version {
        1 => {
            #[cfg(not(feature = "optional-runtime"))]
            let round = plugin.prepare_v1_without_runtime(input, configuration(label, fail)).unwrap();
            #[cfg(feature = "optional-runtime")]
            let round = plugin.prepare_v1::<_, pi_async_runtime::Handle>(input, configuration(label, fail), None).unwrap();
            (round, None)
        }
        2 => {
            let shared = Arc::new(AtomicUsize::new(0));
            let data: InitializationData = (AtomicUsize::new(100 + ordinal), shared.clone(), None);
            #[cfg(not(feature = "optional-runtime"))]
            let round = plugin.prepare_without_runtime(input, configuration(label, fail), data).unwrap();
            #[cfg(feature = "optional-runtime")]
            let round = plugin.prepare::<_, pi_async_runtime::Handle, _>(input, configuration(label, fail), None, data).unwrap();
            (round, Some(shared))
        }
        _ => panic!("unsupported initialization sequence"),
    }
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let root = PathBuf::from(&args[0]); let deployment = PathBuf::from(&args[1]);
    let port: u16 = args[2].parse().unwrap();
    let effects = root.join("input-effects");
    // 本进程尚未启动其它线程（threads）。
    std::env::set_var("PI_FFI_ROUND_OBSERVE_FILE", &effects);
    let host = HostBuild {rust_build_sha256: digest(&deployment.join("build-id")), allocator: "system".into()};
    let version = Version::parse(VERSION).unwrap();
    let manifest = build_manifest(&deployment, version.clone(), &host);
    // SAFETY: 宿主（host）和插件（plugin）为同批可信构建，配套文件（matching files）由本测试独占布置。
    let package = unsafe { PluginPackage::from_verified_build(deployment.clone(), manifest) }.unwrap();
    let manager = Manager::process(host).unwrap();
    let request = PluginRequest {uuid: UUID, version: VersionReq::parse("^1.2").unwrap()};
    let artifact = deployment.join("libcache_ffi_plugin.so");
    assert_eq!(mapping_and_file_handles(&artifact), (false, 0));
    let terminal = terminal(&root, port);
    if let Some(mode) = args.get(3) {
        data_boundary_case(&manager, &package, &request, &terminal, &effects, mode);
        return;
    }
    let peer = terminal.registry().local_peer_id();
    signal(&root, "control-port", &CONTROL_PORT.to_string());
    signal(&root, "host-ready", &format!("{} {peer} {port}", std::process::id()));
    let sequence: Vec<u8> = std::env::var("PI_FFI_INITIALIZATION_SEQUENCE").unwrap().split(',').map(|item| item.parse().unwrap()).collect();
    assert!(sequence == [1,2,1] || sequence == [2,1,2]);
    let mut metadata_calls = 0;
    let mut old: Option<(Plugin, Initialization)> = None;
    for (index, label) in ["red", "blue"].into_iter().enumerate() {
        let plugin = manager.load(&package, &request).unwrap();
        assert_eq!(plugin.metadata().uuid, UUID); assert_eq!(plugin.metadata().version, version);
        metadata_calls += metadata_queries(&artifact, &effects, &format!("before-init-{index}"));
        let (round, shared) = prepare_version(&plugin, &terminal, label, false, sequence[index], index);
        round.initialize().unwrap();
        let publication = local_cache_publication(&terminal, peer);
        assert_eq!(publication.version, 2 + index as u64);
        println!("host local_cache_publication round={index} running=59 version_3959={}", publication.version);
        round.initialize().unwrap();
        if let Some(shared) = &shared {
            assert_eq!(shared.load(Ordering::SeqCst), 1);
            assert_eq!(Arc::strong_count(shared), 2);
        }
        metadata_calls += metadata_queries(&artifact, &effects, &format!("initialized-{index}"));
        if let Some((old_plugin, old_round)) = &old {
            let before = fs::read_to_string(&effects).unwrap();
            old_round.close().unwrap(); old_plugin.unload().unwrap();
            assert_eq!(before, fs::read_to_string(&effects).unwrap());
            assert_ne!(old_round.id().module_epoch, round.id().module_epoch);
            assert_eq!(plugin.current_initialization().unwrap().unwrap().id().round, round.id().round);
        }
        signal(&root, &format!("open-{index}"), "initialized");
        let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            wait_signal(&root, &format!("read-{index}"))
        }));
        if let Err(payload) = observed {
            if let Err(error) = observation::save(&root, &format!("host-timeout-{index}"), &terminal, peer) {
                let _ = writeln!(std::io::stderr(), "只读观察（read-only observation）保存失败：{error}");
            }
            std::panic::resume_unwind(payload);
        }
        assert_local_publication_preserved(&terminal, &publication);
        println!("host local_cache_publication_preserved round={index} version_3959={}", publication.version);
        finish_round(&round); round.close().unwrap();
        if let Some(shared) = &shared {
            assert_eq!(shared.load(Ordering::SeqCst), 1);
            assert_eq!(Arc::strong_count(shared), 1);
            println!("host initialization_data non_clone=true shared_instance=true consumed_once=true released=true");
        }
        metadata_calls += metadata_queries(&artifact, &effects, &format!("closed-{index}"));
        assert_eq!(Arc::strong_count(&terminal), 1);
        plugin.unload().unwrap(); plugin.unload().unwrap();
        let (mapped, handles) = mapping_and_file_handles(&artifact);
        assert!(mapped); assert_eq!(handles, 0);
        println!("host ffi_round={index} success=true input_arc_count=1 retained_mapping=true file_handles=0");
        signal(&root, &format!("closed-{index}"), "closed");
        wait_signal(&root, &format!("control-{index}"));
        old = Some((plugin, round));
    }
    let plugin = manager.load(&package, &request).unwrap();
    metadata_calls += metadata_queries(&artifact, &effects, "before-failed-init");
    let (round, shared) = prepare_version(&plugin, &terminal, "invalid", true, sequence[2], 2);
    for _ in 0..2 {
        let error = round.initialize().expect_err("插件实际返回失败（actual plugin failure）必须保留");
        assert_eq!(error.current_context(), &ErrorKind::InvalidInput);
        assert!(pi_result::debug_message(&error).contains("explicit fixture initialization failure"));
    }
    metadata_calls += metadata_queries(&artifact, &effects, "failed-initialization");
    finish_round(&round); round.close().unwrap();
    metadata_calls += metadata_queries(&artifact, &effects, "closed-failed-initialization");
    if let Some(shared) = &shared {
        assert_eq!(shared.load(Ordering::SeqCst), 1);
        assert_eq!(Arc::strong_count(shared), 1);
    }
    assert_eq!(Arc::strong_count(&terminal), 1);
    plugin.unload().unwrap();
    assert_eq!(metadata_calls, 18);
    assert_eq!(fs::read_to_string(&effects).unwrap(), "initialize:red\nclose\ninitialize:blue\nclose\ninitialize_fail\nclose\n");
    signal(&root, "negative-closed", "finished");
    wait_signal(&root, "client-done");
    println!("host ffi_assertions_complete=true actual_metadata_calls={metadata_calls} input_return=true failed_init_preserved=true");
}


/// 拥有型测试数据（owned test data）的真实析构（Drop）由宿主代码实现，经原虚表执行一次。
struct FailingDrop(Arc<AtomicUsize>);
impl Drop for FailingDrop {
    fn drop(&mut self) { self.0.fetch_add(1, Ordering::SeqCst); panic!("initialization data cleanup failure"); }
}

/// 使用真实 P2P 终端和标准管理入口验证合法描述的拒绝及部分消费，不检查插件业务状态。
fn data_boundary_case(manager: &Manager, package: &PluginPackage, request: &PluginRequest,
    terminal: &Arc<Terminal>, effects: &Path, mode: &str) {
    let plugin = manager.load(package, request).unwrap();
    let shared = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    // SAFETY: 与正常真实案例相同的配套类型、实例和有效持有。
    let input = unsafe { TerminalInput::new(terminal.clone()) };
    let round = if mode == "wrong-type" {
        plugin.prepare_without_runtime(input, configuration("negative", false), (100u64, shared.clone(), None::<Box<dyn Send + Sync>>)).unwrap()
    } else {
        let payload: Option<Box<dyn Send + Sync>> = if mode == "data-drop-failure" { Some(Box::new(FailingDrop(dropped.clone()))) } else { None };
        let data: InitializationData = (AtomicUsize::new(103), shared.clone(), payload);
        plugin.prepare_without_runtime(input, configuration("negative", false), data).unwrap()
    };
    let expected = match mode { "wrong-type" => ErrorKind::Conflict, "missing-data" => ErrorKind::InvalidInput,
        "preconsumed-config" | "data-drop-failure" => ErrorKind::InvalidState, _ => panic!("unknown data test") };
    for _ in 0..2 {
        let failure = round.initialize().expect_err("真实负面输入必须拒绝");
        assert_eq!(*failure.current_context(), expected);
    }
    assert_eq!(shared.load(Ordering::SeqCst), 0, "初始化业务函数不得执行");
    if mode == "data-drop-failure" {
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        for _ in 0..2 {
            let failure = round.close().expect_err("数据清理失败不能被空关闭覆盖");
            assert_eq!(*failure.current_context(), ErrorKind::Internal);
            assert!(pi_result::debug_message(&failure).contains("initialization data cleanup failure"));
        }
        assert_eq!(dropped.load(Ordering::SeqCst), 1, "不能重跑未知析构");
        assert!(!effects.exists(), "不得调用插件业务或空关闭");
    } else {
        finish_round(&round); round.close().unwrap();
        assert_eq!(Arc::strong_count(&shared), 1);
        if mode == "preconsumed-config" { assert_eq!(fs::read_to_string(effects).unwrap(), "close\n"); }
        else { assert!(!effects.exists()); }
        plugin.unload().unwrap();
    }
    println!("host data_boundary={mode} rejected=true hook_calls=0 replay_stable=true cleanup_drop_count={}", dropped.load(Ordering::SeqCst));
}
