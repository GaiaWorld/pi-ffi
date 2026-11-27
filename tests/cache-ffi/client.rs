//! 原库只读的真实客户端（native client）双进程（two-process）业务基线（baseline）。
//! 仅证明原生业务及进程间通信（IPC），不代替外部函数接口（FFI）关闭（close）验收。
use std::{fs, io::Write, path::{Path, PathBuf}, sync::Arc, thread, time::{Duration, Instant}};
use pi_p2p::{AuthMode, HostId, P2pConfig, SeedhostEntry};
use pi_p2p_service::{EventHub, P2pServiceBuilder, P2pServiceTerminal, ServiceStatusSync};
use pi_service::{MultiLayerCodec, ServicePort, ServiceRuntime, ServiceStatus};
use pi_shared_cache_client::*;
use pi_shared_cache_service::{CacheRegistry, ContainerRegistry, storage::CacheBusinessRuntime, service::*};
mod observation;

type Terminal = P2pServiceTerminal<MultiLayerCodec>;

/// 协调文件（coordination files）只传递身份和阶段，不承载业务结果。
fn signal(root: &Path, name: &str, value: &str) {
    let pending = root.join(format!("{name}.pending"));
    fs::write(&pending, value).unwrap();
    fs::rename(pending, root.join(name)).unwrap();
}

fn wait<T>(name: &str, mut read: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(value) = read() { return value; }
        assert!(Instant::now() < deadline, "等待超时（timeout） {name}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn read_signal(root: &Path, name: &str) -> String {
    wait(name, || match fs::read_to_string(root.join(name)) {
        Ok(value) => Some(value),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("{error}"),
    })
}

fn codec() -> MultiLayerCodec {
    let codec = MultiLayerCodec::default();
    let mut fory = fory::Fory::default();
    register_cache_service_types(&mut fory).unwrap();
    codec.register_layer_fory("shared_cache", Arc::new(fory));
    codec
}

/// 按当前配套配置（matching configuration）启用原生进程间通信（IPC）。
fn terminal(root: &Path, role: &str, port: u16, server: bool)
    -> (Arc<Terminal>, Arc<EventHub>, MultiLayerCodec)
{
    let mut config = P2pConfig::default();
    config.identity.keypair_path = root.join(format!("{role}.key")).to_string_lossy().into_owned();
    config.listen_addrs = vec![format!("/ip4/127.0.0.1/udp/{port}/quic-v1")];
    config.mdns.enabled = false;
    config.mdns.auto_dial = false;
    config.metrics.enabled = false;
    config.metrics.runtime_toggle = false;
    config.logs.enabled = false;
    config.auth.mode = AuthMode::Disabled;
    config.governance.enabled = false;
    config.liveness.phi.enabled = false;
    config.liveness.ping.enabled = false;
    config.swarm_runtime.worker_threads = Some(2);
    config.lane.count = Some(2);
    config.ipc.enabled = true;
    config.ipc.shared_root = root.join("ipc").to_string_lossy().into_owned();
    let codec = codec();
    let status = Arc::new(ServiceStatusSync::new());
    let runtime = pi_service::ServiceRuntimeBuilder::<MultiLayerCodec>::new()
        .custom_codec(codec.clone()).status_listener(status.clone()).build();
    if server {
        // 原公开构造器（constructors）及原回调（callbacks），没有替代业务实现（implementation）。
        let business = pi_async_rt::rt::AsyncRuntimeBuilder::default_multi_thread(None, None, None, None);
        runtime.contexts().insert_only_read(Arc::new(CacheRegistry::from_global()));
        runtime.contexts().insert_only_read(Arc::new(ContainerRegistry::new()));
        runtime.contexts().insert_only_read(Arc::new(CacheBusinessRuntime::new(business)));
        runtime.load_services_from_inventory(&["shared_cache"]).unwrap();
        runtime.init_all();
        for port in 3941..=3999u16 {
            assert!(matches!(runtime.get_status(ServicePort::from(port)), Some(ServiceStatus::Running)), "{port}");
        }
    }
    let hub = Arc::new(EventHub::new());
    let terminal = futures::executor::block_on(P2pServiceBuilder::new(runtime, config)
        .with_event_hub(hub.clone()).with_status_listener(status)
        .auto_ipc(true).prefer_ipc(true).with_debug_label(role).build_with_terminal()).unwrap();
    (Arc::new(terminal), hub, codec)
}

fn host(root: &Path, port: u16) {
    let (terminal, _hub, _codec) = terminal(root, "host", port, true);
    let peer = terminal.registry().local_peer_id();
    signal(root, "host-ready", &format!("{} {peer} {port}", std::process::id()));
    read_signal(root, "client-done");
    println!("host baseline_complete=true static_services_running=59 pid={}", std::process::id());
}

/// 仅使用原 CacheTerminal 的具名接口（named APIs）；预期值（expected values）独立给出。
async fn exercise(client: &CacheTerminal<MultiLayerCodec>, peer: HostId, ports: &mut Vec<u16>, kind: u8) {
    let name = format!("native-cache-kind-{kind}");
    macro_rules! call {
        ($method:ident, $request:expr, $output:ident, $port:ident) => {{
            ports.push($port);
            match client.$method(&peer, $request).await.unwrap() {
                $output::Ok(value) => value,
                other => panic!("原生业务结果（native business result） {other:?}"),
            }
        }};
    }
    assert!(call!(table_create, TableCreateRequest {table_name: name.clone(), table_type: kind, persistent: false}, TableCreateOutput, PORT_TABLE_CREATE).success);
    assert!(call!(table_exists, TableExistsRequest {table_name: name.clone()}, TableExistsOutput, PORT_TABLE_EXISTS).exists);
    assert!(call!(is_empty, IsEmptyRequest {table_name: name.clone()}, IsEmptyOutput, PORT_IS_EMPTY).is_empty);
    let listed = call!(table_list, TableListRequest {}, TableListOutput, PORT_TABLE_LIST).tables;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].table_name, name);
    assert_eq!(listed[0].table_type, kind);
    assert!(!listed[0].persistent);
    let first_version = call!(insert, InsertRequest {table_name: name.clone(), key: vec![1], value: b"red".to_vec()}, InsertOutput, PORT_INSERT).version;
    assert_eq!(call!(lookup, LookupRequest {table_name: name.clone(), key: vec![1]}, LookupOutput, PORT_LOOKUP).value, Some(b"red".to_vec()));
    assert_eq!(call!(get_version, GetVersionRequest {table_name: name.clone(), key: vec![1]}, GetVersionOutput, PORT_GET_VERSION).version, Some(first_version));
    call!(insert, InsertRequest {table_name: name.clone(), key: vec![2], value: b"second".to_vec()}, InsertOutput, PORT_INSERT);
    let updated = call!(insert, InsertRequest {table_name: name.clone(), key: vec![1], value: b"blue".to_vec()}, InsertOutput, PORT_INSERT).version;
    assert!(updated > first_version);
    assert_eq!(call!(lookup, LookupRequest {table_name: name.clone(), key: vec![1]}, LookupOutput, PORT_LOOKUP).value, Some(b"blue".to_vec()));
    assert_eq!(call!(get_version, GetVersionRequest {table_name: name.clone(), key: vec![1]}, GetVersionOutput, PORT_GET_VERSION).version, Some(updated));
    assert_eq!(call!(size, SizeRequest {table_name: name.clone()}, SizeOutput, PORT_SIZE).size, 2.0);
    if kind == 0 {
        assert_eq!(call!(first, FirstRequest {table_name: name.clone()}, FirstOutput, PORT_FIRST).key, Some(vec![1]));
        assert_eq!(call!(next, NextRequest {table_name: name.clone(), key: vec![1]}, NextOutput, PORT_NEXT).next_key, Some(vec![2]));
        assert_eq!(call!(next, NextRequest {table_name: name.clone(), key: vec![2]}, NextOutput, PORT_NEXT).next_key, None);
    }
    ports.push(PORT_TABLE_CREATE);
    let duplicate = client.table_create(&peer, TableCreateRequest {table_name: name.clone(), table_type: kind, persistent: false}).await.unwrap();
    assert!(matches!(duplicate, TableCreateOutput::Err(3)), "实际重复建表结果（actual duplicate result） {duplicate:?}");
    ports.push(PORT_TABLE_CREATE);
    let invalid = client.table_create(&peer, TableCreateRequest {table_name: "invalid".into(), table_type: 255, persistent: false}).await.unwrap();
    assert!(matches!(invalid, TableCreateOutput::Err(5)), "实际非法类型结果（actual invalid type result） {invalid:?}");
    assert!(call!(delete, DeleteRequest {table_name: name.clone(), key: vec![2]}, DeleteOutput, PORT_DELETE).deleted);
    assert!(!call!(delete, DeleteRequest {table_name: name.clone(), key: vec![2]}, DeleteOutput, PORT_DELETE).deleted);
    assert_eq!(call!(lookup, LookupRequest {table_name: name.clone(), key: vec![2]}, LookupOutput, PORT_LOOKUP).value, None);
    assert!(call!(clear, ClearRequest {table_name: name.clone()}, ClearOutput, PORT_CLEAR).cleared);
    assert!(call!(is_empty, IsEmptyRequest {table_name: name.clone()}, IsEmptyOutput, PORT_IS_EMPTY).is_empty);
    assert_eq!(call!(size, SizeRequest {table_name: name.clone()}, SizeOutput, PORT_SIZE).size, 0.0);
    assert!(call!(table_drop, TableDropRequest {table_name: name.clone()}, TableDropOutput, PORT_TABLE_DROP).dropped);
    assert!(!call!(table_drop, TableDropRequest {table_name: name.clone()}, TableDropOutput, PORT_TABLE_DROP).dropped);
    assert!(!call!(table_exists, TableExistsRequest {table_name: name.clone()}, TableExistsOutput, PORT_TABLE_EXISTS).exists);
    ports.push(PORT_LOOKUP);
    let missing = client.lookup(&peer, LookupRequest {table_name: name, key: vec![1]}).await.unwrap();
    assert!(matches!(missing, LookupOutput::Err(2)), "实际缺表结果（actual missing table result） {missing:?}");
}

/// 独立控制服务（control service）只验证共享终端（shared terminal）在插件关闭（plugin close）后仍可使用。
fn control(terminal: &Terminal, peer: HostId, port: u16, label: &str) {
    let input = format!("after-close-{label}");
    let result = futures::executor::block_on(terminal.service_terminal().rpc(
        pi_service::ServiceAddress::new(pi_service::ServiceHost::Remote(peer), port.into()),
        input.as_bytes().to_vec(), Some(Duration::from_secs(2)), None,
    )).unwrap();
    assert_eq!(result, format!("control/{input}").as_bytes());
}

/// 公开发现状态（service discovery status）只决定何时开始业务，不判断标准初始化结果。
fn wait_services(root: &Path, terminal: &Terminal, client: &CacheTerminal<MultiLayerCodec>, peer: HostId) {
    let started = Instant::now();
    let mut observed_delay = false;
    wait("原生服务发现（service discovery）", || {
        client.refresh_services_from_discovery(Some(&[peer]));
        let snapshot = client.snapshot_peer(&peer);
        if !observed_delay && started.elapsed() >= Duration::from_secs(10) {
            observed_delay = true;
            if let Err(error) = observation::save(root, "client-delay", terminal, peer) {
                let _ = writeln!(std::io::stderr(), "只读观察（read-only observation）保存失败：{error}");
            }
            if let Err(error) = fs::write(root.join("observation-client-snapshot.txt"), format!("{snapshot:#?}")) {
                let _ = writeln!(std::io::stderr(), "客户端快照（client snapshot）保存失败：{error}");
            }
        }
        snapshot.filter(|snapshot| {
            (3941..=3999).all(|port| snapshot.services.iter().any(|record| {
                record.port == port && matches!(record.status, ServiceStatus::Running)
            }))
        }).map(|_| ())
    });
    println!("client discovered_services_running=59");
}

/// 连接另一真实进程，经原生发现（discovery）建立进程间通信（IPC）通道。
fn connect(root: &Path) -> (Arc<Terminal>, CacheTerminal<MultiLayerCodec>, HostId) {
    let ready = read_signal(root, "host-ready");
    let fields: Vec<_> = ready.split_whitespace().collect();
    assert_ne!(fields[0].parse::<u32>().unwrap(), std::process::id());
    let peer: HostId = fields[1].parse().unwrap();
    let port: u16 = fields[2].parse().unwrap();
    let (terminal, hub, codec) = terminal(root, "client", 0, false);
    assert_ne!(terminal.registry().local_peer_id(), peer);
    terminal.p2p_terminal().add_seedhost(SeedhostEntry {peer_id: peer,
        multiaddrs: vec![format!("/ip4/127.0.0.1/udp/{port}/quic-v1").parse().unwrap()],
        priority: 0, tags: vec![], enabled: true}).unwrap();
    wait("原生进程间通信（IPC）", || terminal.p2p_terminal().is_ipc_ready(peer).then_some(()));
    let client = futures::executor::block_on(CacheTerminalBuilder::new()
        .with_codec(codec).with_p2p_terminal(terminal.clone()).with_event_hub(hub)
        .with_peers(vec![peer]).with_rpc_timeout_ms(2000).with_auto_reconnect(false).with_auto_ipc(true).build()).unwrap();
    (terminal, client, peer)
}

fn client(root: &Path, ffi: bool) {
    let (terminal, client, peer) = connect(root);
    let mut ports = Vec::new();
    if ffi {
        let port: u16 = read_signal(root, "control-port").parse().unwrap();
        for index in 0..2 {
            read_signal(root, &format!("open-{index}"));
            wait_services(root, &terminal, &client, peer);
            futures::executor::block_on(async {
                exercise(&client, peer, &mut ports, 0).await;
                exercise(&client, peer, &mut ports, 1).await;
            });
            signal(root, &format!("read-{index}"), "finished");
            read_signal(root, &format!("closed-{index}"));
            control(&terminal, peer, port, &index.to_string());
            ports.push(port);
            signal(root, &format!("control-{index}"), "alive");
        }
        read_signal(root, "negative-closed");
        control(&terminal, peer, port, "negative");
        ports.push(port);
    } else {
        wait_services(root, &terminal, &client, peer);
        futures::executor::block_on(async {
            exercise(&client, peer, &mut ports, 0).await;
            exercise(&client, peer, &mut ports, 1).await;
        });
    }
    fs::write(root.join("expected-ports.txt"), ports.iter().map(u16::to_string).collect::<Vec<_>>().join(",")).unwrap();
    println!("client baseline_complete=true cache_rpc_calls={} remote={peer} pid={}", ports.len(), std::process::id());
    signal(root, "client-done", "finished");
}

/// 原生客户端（client）创建临时表、写入并读回已知值，再验证宿主关闭后的控制服务。
#[cfg(windows)]
fn windows_basic(root: &Path) {
    let (terminal, client, peer) = connect(root);
    wait_services(root, &terminal, &client, peer);
    futures::executor::block_on(async {
        let table_name = "pi-ffi-windows-basic".to_owned();
        let created = client.table_create(&peer, TableCreateRequest {
            table_name: table_name.clone(), table_type: 0, persistent: false,
        }).await.unwrap();
        assert!(matches!(created, TableCreateOutput::Ok(value) if value.success));

        let value = b"pi-ffi-windows-basic".to_vec();
        let inserted = client.insert(&peer, InsertRequest {
            table_name: table_name.clone(), key: vec![1], value: value.clone(),
        }).await.unwrap();
        assert!(matches!(inserted, InsertOutput::Ok(_)));
        let read = client.lookup(&peer, LookupRequest { table_name, key: vec![1] }).await.unwrap();
        assert!(matches!(read, LookupOutput::Ok(found) if found.value == Some(value)));
    });

    signal(root, "windows-business-done", "finished");
    read_signal(root, "windows-closed");
    let port: u16 = read_signal(root, "control-port").parse().unwrap();
    control(&terminal, peer, port, "windows");
    let ports = [PORT_TABLE_CREATE, PORT_INSERT, PORT_LOOKUP, port];
    fs::write(root.join("expected-ports.txt"), ports.iter().map(u16::to_string).collect::<Vec<_>>().join(",")).unwrap();
    println!("client windows_basic_complete=true value_ok=true after_close_control=true");
    signal(root, "client-done", "finished");
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let root = PathBuf::from(&args[1]);
    match args[0].as_str() {
        "host" => host(&root, args[2].parse().unwrap()),
        "client" => client(&root, false),
        "ffi-client" => client(&root, true),
        #[cfg(windows)]
        "windows-basic" => windows_basic(&root),
        _ => panic!("进程角色（process role）无效"),
    }
}
