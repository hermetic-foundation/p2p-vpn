# Namespace E2E Smoke

This file records privileged Linux namespace smoke-test evidence for local
Hyprspace-style overlay behavior. These tests require a Linux host that permits
user namespaces, network namespaces, veth setup, and `/dev/net/tun`.

## 2026-08-04

Command:

```sh
nix run .#tun-e2e -- -- --ignored --nocapture
```

Result:

```text
running 6 tests
test tun_namespace_ping_crosses_relay_overlay ... ok
test tun_namespace_invite_import_crosses_relay_overlay ... ok
test tun_namespace_ping_crosses_mdns_discovered_overlay ... ok
test tun_namespace_relay_overlay_promotes_to_direct_path ... ok
test tun_namespace_ping_crosses_two_node_overlay ... ok
test tun_namespace_ping_crosses_dht_discovered_overlay ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 18.52s
```

Coverage proven by this run:

- Direct static-peer overlay ping and routed-prefix ping.
- mDNS-discovered overlay packet forwarding.
- DHT/bootstrap-discovered overlay packet forwarding.
- Circuit-relay fallback packet forwarding.
- Signed-invite onboarding over a relayed inviter address.
- Relay-to-direct promotion with DCUtR and AutoNAT enabled, followed by
  packet-plane datagram forwarding.
