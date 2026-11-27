//! 仅在等待迟延或失败时保存只读观察（read-only observation），不更新服务状态。
use std::{fs, path::Path, time::SystemTime};
use pi_p2p::{HostId, RegistryQuery};
use pi_p2p_service::{P2pServiceTerminal, discovery::{self, ServiceQuery}};
use pi_service::{MultiLayerCodec, ServicePort};

/// 各层分别读取，不承诺原子快照（atomic snapshot）；写入失败不得覆盖原测试错误。
pub fn save(root: &Path, phase: &str, terminal: &P2pServiceTerminal<MultiLayerCodec>, peer: HostId)
    -> std::io::Result<()>
{
    let started = SystemTime::now();
    let local: Vec<_> = (3941..=3999u16).map(|port| {
        (port, terminal.service_runtime().and_then(|runtime| runtime.get_status(ServicePort::from(port))))
    }).collect();
    let registry = terminal.registry().snapshot(RegistryQuery {
        prefix: Some(format!("service/{peer}/")), peer_id: None, host_names: None,
        include_deleted: true, include_internal: false, key_predicate: None, limit: None, offset: 0,
    });
    let discovery = discovery::query(&ServiceQuery {
        peer_ids: Some(vec![peer]), ..ServiceQuery::default()
    });
    fs::write(root.join(format!("observation-{phase}.txt")), format!(
        "pid={} started={started:?} not_atomic=true\nlocal={local:#?}\nregistry={registry:#?}\ndiscovery={discovery:#?}\nfinished={:?}\n",
        std::process::id(), SystemTime::now()))
}
