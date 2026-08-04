# Feature Matrix

This matrix tracks the Hyprspace-style feature-completeness goal against the
current Rust implementation. It is intentionally conservative: a row is marked
operational only when there is code, documentation, and a concrete verification
path.

## Status Key

- `operational`: implemented and covered by normal verification or a documented
  privileged end-to-end command.
- `partial`: implemented enough to use, but the evidence is narrower than the
  final goal or an important production concern remains.
- `blocked`: not currently possible through the selected dependency surface.

## Matrix

| Area | Status | Current evidence | Verification |
| --- | --- | --- | --- |
| Packet data plane | partial | `src/runtime/packet.rs`, `src/wire.rs`, and `src/runtime/runner.rs` implement a fixed binary packet frame over authenticated libp2p request-response streams. `src/config.rs`, `src/runtime/control.rs`, and `src/runtime/packet_plane.rs` expose owned packet-plane listener config, bind configured UDP listeners, advertise direct endpoint candidates, negotiate signed hello/accept handshakes over the authenticated control plane, derive directional ChaCha20-Poly1305 packet keys with ephemeral X25519 keys, register per-peer packet-plane sessions with endpoint/MTU/session metadata, seal/open encrypted datagram envelopes around the existing packet frame, exchange sealed frames over bound UDP sockets, let outbound queue draining prefer an established packet-plane datagram session when peer capabilities and path selection allow it, and accept inbound UDP frames from established sessions through the same route authorization/replay/TUN write path as stream packets. `src/queue.rs` and `src/runtime/runner.rs` add bounded per-peer queues and flow-sharded stream fallback. | `nix develop -c cargo test`; focused: `nix develop -c cargo test runtime::packet_plane::tests`; negotiation: `nix develop -c cargo test runtime::runner::tests::packet_plane_control_negotiation_establishes_sessions`; outbound daemon integration: `nix develop -c cargo test runtime::runner::tests::drain_outbound_queue_prefers_established_packet_plane_datagram_path`; inbound daemon integration: `nix develop -c cargo test runtime::runner::tests` |
| Native QUIC datagrams | blocked | `src/runtime/runner.rs` advertises local QUIC datagrams as unsupported because the locked `libp2p-quic`/`Swarm` surface does not expose an application datagram sender/receiver. Datagrams are modelled as a preferred path kind, but packets are not falsely reported as datagram-sent. | `nix develop -c cargo test runtime::runner::tests::local_packet_data_plane_is_identity_keyed_stream_fallback_only` |
| Stream fallback | operational | The fallback uses identity-keyed request-response streams, per-peer send windows, and flow shards so same-shard packets stay queued while unrelated shards can drain. | `nix develop -c cargo test runtime::runner::tests::drain_outbound_queue_gates_stream_fallback_by_flow_shard` |
| Queueing and backpressure | operational | `src/queue.rs` enforces packet count, byte count, packet age, per-peer fairness, expired drops, and blocked-peer retention. Runtime metrics expose queue drops and blocked reasons. | `nix develop -c cargo test queue::tests` |
| Control, packet, and service surfaces | operational | Separate codecs and behaviours exist in `src/runtime/control.rs`, `src/runtime/packet.rs`, and `src/runtime/service.rs`. Capability and service status exchanges are separate from packet forwarding. | `nix develop -c cargo test runtime::p2p::tests::two_nodes_exchange_packet_request`; full codec coverage: `nix develop -c cargo test` |
| Route ownership and source authorization | operational | `src/route.rs` and `src/runtime/forward.rs` authorize advertised prefixes, local sources, inbound sources, and local destinations. Capability validation rejects unauthorized advertised routes. | `nix develop -c cargo test route::tests`; focused: `nix develop -c cargo test runtime::runner::tests::capability_response_rejects_unauthorized_route_advertisements` |
| Membership and invite UX | operational | `src/invite.rs`, `src/config.rs`, and CLI commands implement signed invite export/import, membership tags, previous-tag compatibility, expiry, signature verification, and config generation. | `nix develop -c cargo test invite::tests`; focused: `nix develop -c cargo test runtime::runner::tests::capability_response_accepts_previous_membership_tag` |
| Replay and session expiry | operational | `src/runtime/forward.rs` keeps per-peer/session replay windows, bounds total replay windows, expires stale sessions, and exposes replay window count in daemon state. | `nix develop -c cargo test runtime::forward::tests::replay_window_expires_after_session_ttl`; full replay coverage: `nix develop -c cargo test runtime::forward::tests` |
| Resource limits and rate limits | operational | `src/config.rs`, `src/runtime/p2p.rs`, and `src/runtime/runner.rs` expose swarm connection limits, stream limits, relay limits, and per-peer inbound packet rate limiting. | `nix develop -c cargo test runtime::runner::tests::peer_packet_rate_limiter_caps_each_peer_independently`; full config coverage: `nix develop -c cargo test config::tests` |
| Path selection and promotion | operational | `src/path.rs` scores direct datagram, direct stream, TCP, and relay paths. Runtime records promotions/fallbacks and logs path-selection changes. | `nix develop -c cargo test path::tests` |
| NAT/DCUtR hole punching | operational on supported Linux hosts | The privileged namespace test starts a relay plus two peers, uses relay fallback, enables DCUtR and AutoNAT, observes a successful hole-punch result, promotes to a direct TCP path, and verifies TUN ping. | `nix run .#tun-e2e -- tun_namespace_relay_overlay_promotes_to_direct_path -- --ignored --exact --nocapture` |
| Discovery and public libp2p/IPFS bootstrap | partial | Private Kademlia, mDNS, configurable bootstrap peers, relay reservations, and an opt-in IPFS Kademlia protocol/bootstrap template exist. Public peers are documented as reachability infrastructure, not membership or route authority. | `nix develop -c cargo test runtime::p2p::tests::build_node_accepts_ipfs_compatible_kademlia_protocol`; DHT E2E: `nix run .#tun-e2e -- tun_namespace_ping_crosses_dht_discovered_overlay -- --ignored --exact --nocapture` |
| MTU and MSS behavior | partial | Effective packet MTU is bounded by the wire payload length and peer/path MTU. Linux route commands include MTU and `advmss` hints when possible. Oversized path-MTU drops generate local IPv4 fragmentation-needed or ICMPv6 packet-too-big feedback when the original packet is parseable. There is no Hyprspace-level fragmentation or active PMTUD. | `nix develop -c cargo test runtime::tun::tests::packet_too_big_builds_ipv4_fragmentation_needed`; full MTU coverage: `nix develop -c cargo test mtu` |
| Daemon lifecycle | operational | `src/runtime/runner.rs` handles shutdown reasons, structured runtime logs, metrics snapshots, control-socket shutdown, and orderly cleanup. | `nix develop -c cargo test runtime::runner::tests::runtime_control_shutdown_acknowledges_and_requests_stop`; full lifecycle coverage: `nix develop -c cargo test runtime::runner::tests` |
| Local daemon control CLI | operational | `src/runtime/control_socket.rs` and `src/main.rs` expose `daemon-status`, `daemon-state`, `daemon-peers`, `daemon-routes`, `daemon-paths`, `daemon-mtu`, `daemon-capabilities`, and `daemon-shutdown`. | `nix develop -c cargo test runtime::control_socket::tests::control_socket_serves_daemon_view_requests` |
| Remote status/control CLI | operational | `src/runtime/remote.rs` and `src/main.rs` query peers for service status, live paths, routes, capabilities, and MTU-related state without requiring daemon-local socket access. | `nix develop -c cargo test runtime::remote::tests::query_peer_status_exchanges_live_control_and_service_status`; CLI line coverage: `nix develop -c cargo test peer_live` |
| Security audit logging | operational | `src/runtime/runner.rs` emits structured audit fields for rejected packets without logging packet payload bytes. Metrics classify rejection reasons. | `nix develop -c cargo test runtime::runner::tests::packet_rejection_audit_fields_include_safe_packet_metadata` |
| NixOS integration | operational | `nix/nixos-module.nix` defines instances, systemd units, runtime directories, firewall ports, TUN kernel module, control socket shutdown, and restart behavior. | `nix flake check` |
| Release packaging | operational | `flake.nix` builds the binary package and reproducible release archive with README, flake files, NixOS module, examples, and this matrix. | `nix build .#releaseArchive && tar -tzf result` |

## Remaining Non-Trivial Gaps

1. Native QUIC datagrams are not operational through the current libp2p
   dependency surface. Closing this requires a lower-level transport integration,
   a dependency upgrade that exposes application datagrams, or a different
   datagram-capable substrate. Owned packet-plane listener configuration,
   listener binding, endpoint capability advertisement, automatic signed packet
   session negotiation over the control plane, X25519 key agreement, per-peer
   packet-plane session state, encrypted datagram frame primitives, socket-level
   UDP send/receive primitives, daemon outbound queue sends over an established
   packet-plane session, and daemon inbound handling for established
   packet-plane sessions exist.
2. MTU handling deliberately rejects oversized packets, writes local
   packet-too-big feedback where possible, and provides Linux route MSS hints,
   but it does not implement overlay fragmentation or active PMTUD.
3. Public libp2p/IPFS infrastructure is supported only as discovery and
   reachability assistance. It is not, and should not become, membership or route
   authority.
4. Privileged namespace tests prove important operational flows on this host, but
   they still require a Linux kernel with user namespaces, network namespaces,
   veth, and `/dev/net/tun`.
