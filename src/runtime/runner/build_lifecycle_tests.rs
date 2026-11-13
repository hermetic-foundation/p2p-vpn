use super::*;
use crate::{
    identity::NodeIdentity,
    runtime::tun::{PacketRead, PacketWrite},
};

struct EmptyPackets;

impl PacketRead for EmptyPackets {
    fn read_packet(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }
}

impl PacketWrite for EmptyPackets {
    fn write_packet(&mut self, packet: &[u8]) -> io::Result<usize> {
        Ok(packet.len())
    }
}

fn platform() -> RuntimePlatform {
    RuntimePlatform::new(
        PacketIo::new(EmptyPackets, EmptyPackets),
        PreconfiguredTunRoutes,
    )
}

#[test]
fn embedding_default_is_passive_in_every_build() {
    assert!(!platform().experimental_demand_lifecycle);
}

#[test]
fn application_build_policy_selects_feature_without_losing_platform_state() {
    let identity = NodeIdentity::generate_ed25519().unwrap();
    let config: Config = serde_json::from_value(serde_json::json!({
        "network": {"name": "trial", "private_key": identity.private_key}
    }))
    .unwrap();
    let installed = TunRuntimeConfig::from_config(&config).unwrap();
    let (_, control) = super::super::control_socket::runtime_control_channel();
    let platform = platform()
        .with_installed_tun(installed.clone())
        .with_control(control)
        .with_build_lifecycle_policy();
    assert_eq!(
        platform.experimental_demand_lifecycle,
        cfg!(feature = "demand-lifecycle-trial")
    );
    assert_eq!(platform.installed_tun, Some(installed));
    assert!(platform.control.is_some());
}

#[test]
fn build_policy_preserves_explicit_embedding_opt_in_and_is_idempotent() {
    let platform = platform()
        .with_experimental_demand_lifecycle()
        .with_build_lifecycle_policy()
        .with_build_lifecycle_policy();
    assert!(platform.experimental_demand_lifecycle);
}
