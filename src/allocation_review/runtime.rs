use super::{delta, snapshot};
use crate::{
    config::Config,
    identity::NodeIdentity,
    runtime::{
        runner::{ShutdownReason, run_config_until_with_packet_io},
        tun::{PacketIo, PacketRead, PacketWrite},
    },
};
use std::{
    io,
    sync::mpsc,
    time::{Duration, Instant},
};

mod autonat;
mod initialization;

struct IdleDevice;

struct IdleReader(mpsc::Receiver<()>);

impl PacketRead for IdleReader {
    fn read_packet(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        self.0.recv().unwrap();
        Err(io::ErrorKind::Interrupted.into())
    }
}

impl PacketWrite for IdleDevice {
    fn write_packet(&mut self, packet: &[u8]) -> io::Result<usize> {
        Ok(packet.len())
    }
}

fn rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

fn isolated_config() -> Config {
    let identity = NodeIdentity::generate_ed25519().unwrap();
    let bootstrap = NodeIdentity::generate_ed25519().unwrap();
    serde_json::from_value(serde_json::json!({
        "network": {
            "name": "allocation-review",
            "private_key": identity.private_key,
            "listen_addresses": ["/ip4/127.0.0.1/tcp/0"],
            "bootstrap_peers": [{
                "id": bootstrap.peer_id,
                "address": format!("/ip4/127.0.0.1/tcp/9/p2p/{}", bootstrap.peer_id),
            }],
            "discovery": {"mdns": false},
            "packet_plane": {"listen": ["127.0.0.1:0"]},
        }
    }))
    .unwrap()
}

fn require_isolated_network() {
    // Reject the host namespace before constructing any networking objects.
    let devices = std::fs::read_to_string("/proc/net/dev").unwrap();
    let interfaces: Vec<_> = devices
        .lines()
        .skip(2)
        .map(|line| line.split_once(':').unwrap().0.trim())
        .collect();
    assert_eq!(interfaces, ["lo"]);
    drop(interfaces);
    drop(devices);
}

#[test]
#[ignore = "allocation review: fresh isolated network namespace, one test thread"]
fn measure_runtime_teardown_allocations() {
    require_isolated_network();
    let smoke = match std::env::var("P2P_VPN_REVIEW_RUNTIME_SMOKE").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("0") => false,
        Ok("1") => true,
        other => panic!("invalid runtime smoke setting: {other:?}"),
    };
    let cycles = if smoke { 1 } else { 10 };
    let dwell = Duration::from_secs(if smoke { 2 } else { 30 });
    let config = isolated_config();
    config.validate_runtime().unwrap();
    // Observer capacity and fixture identity remain outside measured runtime ownership.
    let mut rows = Vec::with_capacity(cycles);
    let thread_count = || std::fs::read_dir("/proc/self/task").unwrap().count();
    let uninitialized_threads = thread_count();
    let before_timer = snapshot();
    futures::executor::block_on(futures_timer::Delay::new(Duration::from_millis(1)));
    let after_timer = snapshot();
    let initial_threads = thread_count();
    assert_eq!(initial_threads, uninitialized_threads + 1);
    let initial = snapshot();
    for cycle in 1..=cycles {
        let rss_before = rss_kib();
        let before = snapshot();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let mut active = None;
        let (stop_reader, reader_stopped) = mpsc::channel();
        runtime
            .block_on(run_config_until_with_packet_io(
                config.clone(),
                PacketIo::new(IdleReader(reader_stopped), IdleDevice),
                None,
                None,
                None,
                async {
                    tokio::time::sleep(dwell).await;
                    active = Some(snapshot());
                    stop_reader.send(()).unwrap();
                    ShutdownReason::Terminate
                },
            ))
            .expect("runtime must shut down cleanly");
        let returned = snapshot();
        drop(stop_reader);
        // Tokio joins its workers; the packet reader is a separate production-owned thread.
        drop(runtime);
        let deadline = Instant::now() + Duration::from_secs(2);
        while thread_count() != initial_threads && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let final_threads = thread_count();
        let dropped = snapshot();
        let rss_after = rss_kib();
        rows.push((
            cycle,
            before,
            active.unwrap(),
            returned,
            dropped,
            rss_before,
            rss_after,
            final_threads,
        ));
        if final_threads != initial_threads {
            break;
        }
    }
    let final_snapshot = snapshot();
    let rows: Vec<_> = rows
        .into_iter()
        .map(
            |(cycle, before, active, returned, dropped, rss_before, rss_after, final_threads)| {
                serde_json::json!({
                    "cycle": cycle,
                    "active_from_cycle_start": delta(before, active),
                    "runner_returned_from_cycle_start": delta(before, returned),
                    "runtime_dropped_from_cycle_start": delta(before, dropped),
                    "runtime_dropped_from_initial": delta(initial, dropped),
                    "rss_before_kib": rss_before,
                    "rss_after_kib": rss_after,
                    "threads_after": final_threads,
                })
            },
        )
        .collect();
    let complete = rows.len() == cycles
        && rows
            .iter()
            .all(|row| row["threads_after"] == initial_threads);
    let remaining_threads: Vec<_> = std::fs::read_dir("/proc/self/task")
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            serde_json::json!({
                "tid": path.file_name().unwrap().to_string_lossy(),
                "name": std::fs::read_to_string(path.join("comm")).unwrap(),
                "wait": std::fs::read_to_string(path.join("wchan")).unwrap(),
            })
        })
        .collect();
    eprintln!(
        "runtime_allocation_sample {}",
        serde_json::json!({
            "schema_version": 1,
            "proof_eligible": !smoke,
            "complete": complete,
            "threads_before": initial_threads,
            "threads_before_timer": uninitialized_threads,
            "global_timer_initialization": delta(before_timer, after_timer),
            "remaining_threads": remaining_threads,
            "cycles": rows,
            "dwell_seconds": dwell.as_secs(),
            "workers": 2,
            "overlay_peers": 0,
            "unavailable_loopback_bootstrap_peers": 1,
            "unreachable_default_public_bootstrap_peers": 5,
            "whole_capture": delta(initial, final_snapshot),
        })
    );
    assert!(
        complete,
        "runtime thread did not retire; partial evidence retained"
    );
}
