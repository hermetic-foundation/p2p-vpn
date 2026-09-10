use super::{isolated_config, require_isolated_network};
use crate::{
    allocation_review::{delta, snapshot},
    runtime::p2p::{HostConfig, build_node},
};
use std::time::Duration;

#[test]
#[ignore = "allocation attribution: fresh isolated network namespace, one test thread"]
fn measure_runtime_initialization_components() {
    require_isolated_network();
    let config = isolated_config();
    config.validate_runtime().unwrap();
    let identity = config.identity().unwrap();
    let keypair = identity.keypair().unwrap();
    let host = HostConfig {
        identity,
        network_name: config.network.name.clone(),
        membership_tag: config.membership_tag().unwrap(),
        mtu: config.effective_packet_mtu(),
        max_concurrent_control_streams: config.resources.control_stream_limit(),
        max_concurrent_packet_streams: config.resources.packet_stream_limit(),
        listen_addresses: config.listen_multiaddrs().unwrap(),
        external_addresses: Vec::new(),
        // Match the public-default runner's startup before its LAN-first holdoff ends.
        bootstrap_peers: Vec::new(),
        known_peers: Vec::new(),
        relay_reservations: Vec::new(),
        relay_server: false,
        relay_resources: config.network.relay.resources,
        resources: config.resources,
        discovery: config.network.discovery.clone(),
    };
    let mut rows = Vec::with_capacity(40);
    let timer_before = snapshot();
    futures::executor::block_on(futures_timer::Delay::new(Duration::from_millis(1)));
    let timer_after = snapshot();
    for cycle in 1..=10 {
        for stage in ["tokio", "noise", "quic_config", "node"] {
            let before = snapshot();
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            {
                let _entered = runtime.enter();
                match stage {
                    "tokio" => (),
                    "noise" => drop(libp2p::noise::Config::new(&keypair).unwrap()),
                    "quic_config" => drop(libp2p::quic::Config::new(&keypair)),
                    "node" => drop(build_node(&host).unwrap()),
                    _ => unreachable!(),
                }
            }
            drop(runtime);
            let dropped = snapshot();
            std::thread::sleep(Duration::from_millis(100));
            let settled = snapshot();
            rows.push((cycle, stage, before, dropped, settled));
        }
    }
    // Formatting and JSON allocation are outside every measured stage.
    let rows: Vec<_> = rows
        .into_iter()
        .map(|(cycle, stage, before, dropped, settled)| {
            serde_json::json!({
                "cycle": cycle,
                "stage": stage,
                "dropped": delta(before, dropped),
                "settled": delta(before, settled),
            })
        })
        .collect();
    eprintln!(
        "runtime_initialization_sample {}",
        serde_json::json!({
            "schema_version": 1,
            "cycles": 10,
            "settle_millis": 100,
            "workers": 2,
            "global_timer_initialization": delta(timer_before, timer_after),
            "rows": rows,
        })
    );
}
