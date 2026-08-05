# p2p-vpn

Rust foundation for a Hyprspace-inspired peer-to-peer mesh VPN.

The v2 design keeps libp2p as the identity, discovery, NAT traversal, and relay
substrate while making the VPN data plane packet-oriented:

- owned authenticated UDP packet-plane sessions as the current preferred
  operational packet transport once peers have a direct libp2p path for
  negotiation
- native libp2p QUIC datagrams as a future promotion path when the libp2p data
  plane exposes a usable datagram handle
- direct QUIC streams as the preferred libp2p stream fallback
- framed libp2p streams as TCP and relay fallback
- bounded per-peer packet queues with intentional drop policy
- separate control, packet, and service protocol surfaces
- explicit route ownership and source-address authorization
- path scoring and promotion from relay to direct paths
- first-class metrics for packet, queue, route, relay, AutoNAT, DCUtR, and path behavior

Discovery is overlay-scoped. Configure bootstrap peers and relay-capable peers
that intentionally participate in the VPN; do not rely on the public IPFS DHT
as the membership or routing authority. When Kademlia is enabled, nodes announce
and query an overlay provider key derived from the configured network name,
`/p2p-vpn/<network>/providers/1`. Provider results are only dialed when the peer
ID is already present in the configured peer list, so DHT discovery is a
reachability hint rather than route or membership authorization. Public libp2p
relays can be useful for experiments only when they support the needed relay
reservations and are acceptable for the deployment's trust and availability
requirements. A relay that proves reservation and relayed-circuit fallback is
still not proof that public-relay-assisted DCUtR will complete, because direct
promotion depends on the relay, transport, and both peers' NAT/path topology.

Runtime connections are role-filtered before they are used for control or packet
forwarding. The local peer and configured VPN peers are overlay members.
Configured bootstrap peers, relay reservation peers, and relays embedded in
relayed peer addresses are reachability infrastructure only: they may remain
connected for discovery, AutoNAT, relay, or DCUtR support, but they do not gain
overlay control, service, packet forwarding, or route authority. Other peers are
disconnected and counted as unauthorized connection drops. Packet source
ownership is checked separately against configured routes.
An optional `network.membership_key` adds overlay-wide membership proof for
configured peers. The base64 key is never sent on the wire; peers exchange a
network-scoped SHA-256 membership tag in the control handshake and reject
configured peers whose tag does not match. During a membership-key rotation,
`network.previous_membership_tags` can list old 32-byte base64 tags that remain
acceptable for inbound control and service-plane validation. Nodes still
advertise only the current tag derived from `network.membership_key`; previous
tags are an acceptance window, not an authority to originate routes or a value
sent as the local identity. Kademlia refreshes query previous-tag rendezvous
keys during that acceptance window so peers can still discover each other while
membership-key rotation is in progress.
Outbound packets are not drained to a peer until that peer has passed the
control-plane capability exchange, including network-name, membership-tag,
protocol, MTU, path, and route-advertisement validation.
Validated capabilities are scoped to the active peer connection set; after a
peer fully disconnects, the next connection must complete a fresh capability
exchange before queued packets can drain again.

Discovery toggles control runtime behaviour construction. Disabling mDNS or
DCUtR prevents the corresponding libp2p behaviour from being installed in the
swarm; disabling AutoNAT prevents libp2p reachability probing; disabling
Kademlia prevents overlay provider advertisement and lookup.

The service plane exposes `/p2p-vpn/service/1` over a bounded reliable
request-response stream. Configured peers exchange lightweight status requests
on connection, scoped by the same network name and optional membership tag as
the control plane. This gives operators and future service discovery features a
separate protocol surface without overloading the packet data path.

The control plane exposes `/p2p-vpn/control/1` over a bounded reliable
request-response stream. Peers exchange capabilities when a configured transport
peer connects, including wire version, packet protocol, effective MTU, preferred
path, overlay network name, advertised route prefixes, whether native libp2p
QUIC datagrams are currently supported, and whether the owned UDP packet plane
or owned QUIC packet-plane backend is available. Owned QUIC packet-plane support
also carries a DER-encoded QUIC server certificate in the signed capability
exchange so peers can pin the trust anchor before opening a direct Quinn
connection. Configs can also declare owned packet-plane UDP bind addresses
under `network.packet_plane.listen`, owned QUIC bind addresses under
`network.packet_plane.quic_listen`, externally reachable UDP packet endpoints
under `network.packet_plane.external_endpoints`, externally reachable owned
QUIC packet endpoints under `network.packet_plane.quic_external_endpoints`, and
the packet session lifetime under `network.packet_plane.session_ttl_seconds`.
`network.packet_plane.max_replay_windows_per_session` bounds the authenticated
datagram replay state retained for each packet-plane peer session. The external
endpoints are advertised as packet endpoint candidates in the capability
exchange. The daemon binds the configured packet-plane UDP listeners during
startup, binds at most one configured owned QUIC packet-plane listener, logs the
bound listener addresses, and exposes UDP and QUIC packet-plane listener state
through daemon status, state, and capability views. Packet-plane negotiation
resolves advertised DNS-style
`host:port` endpoint candidates to concrete socket endpoints, ranks the resolved
endpoints before signing the packet-plane handshake, preferring public-style
addresses over private addresses, private addresses over loopback or link-local
addresses, and concrete addresses over wildcard listener addresses. That keeps
a generated public endpoint from being shadowed by a local bind address when
both are advertised. The current local capability advertises the node's built-in
IPv4 and IPv6 host routes, direct QUIC streams as preferred, and native QUIC
datagrams as unsupported. Owned QUIC packet-plane datagrams are advertised only
when `network.packet_plane.quic_listen` binds successfully, including the
runtime-generated certificate trust anchor and QUIC-specific endpoint
candidates. The owned QUIC runtime primitive already carries the same
authenticated packet-plane datagrams over Quinn QUIC DATAGRAM with explicit
certificate trust, and the control capability schema now validates the
advertised certificate material. Outbound queue draining distinguishes owned UDP
and owned QUIC packet-plane sessions, prefers an established owned QUIC session
when the peer advertised owned QUIC support, and otherwise falls back to an
established UDP packet-plane session or packet streams. The control-plane hello
flow opens or accepts the owned Quinn connection before registering the signed
owned QUIC packet-plane session, and inbound owned QUIC datagrams enter the same
authorization, rate-limit, route, replay, path-probe, and TUN write path as UDP
packet-plane datagrams. Outbound queue draining respects the peer's advertised
effective MTU and drops oversized packets before sending them to the packet
stream fallback.
Capability requests from unconfigured peers are
rejected, and configured peers are only accepted when they advertise the same
overlay network name, compatible wire version, packet protocol, packet header
length, matching membership tag when a key is configured, known preferred path,
coherent datagram support, valid owned-QUIC certificate material when owned QUIC
is advertised, non-zero effective MTU, and no route prefixes outside their
configured ownership. Packet endpoint candidates must parse as socket
addresses or DNS-style `host:port` endpoints; they are candidates for the owned
packet data plane, not membership or route authority. The packet-plane session
primitive uses fixed binary
hello/accept handshakes signed by the node's libp2p identity key and bound to
the overlay network name, session id, nonce, MTU, endpoint, identity public key,
and ephemeral X25519 public key. Verified handshakes can derive directional
ChaCha20-Poly1305 keys and seal the existing packet frame inside an
authenticated datagram envelope keyed by packet session id and sequence. The
packet-plane runtime can send and receive those sealed frames over its bound UDP
listeners and keeps a per-peer session registry with endpoint, MTU, role, and
local/remote packet session ids visible through daemon state and capability
views. Peers that both advertise packet-plane endpoints negotiate those sessions
over the authenticated control plane with a deterministic single initiator,
then use the owned UDP packet plane for outbound queue draining when path
selection allows it. Established owned QUIC sessions can also carry outbound
queued packet frames over Quinn datagrams. The daemon accepts inbound UDP and
owned QUIC frames from established packet-plane sessions, rejects duplicate
authenticated datagrams at the session boundary with bounded per-session replay
state, and then applies the same route authorization, replay protection, rate
limiting, and TUN write path as stream packets.
Packet-plane sessions are intentionally bounded: the daemon expires established
sessions after the configured lifetime, defaulting to 600 seconds, marks the
associated direct datagram path unhealthy, records
`packet_plane_sessions_expired`, and attempts packet-plane negotiation again
when this node is responsible for initiating the hello.

The current stream data plane uses a fixed binary header followed by the raw IP
packet payload. The header includes a fresh non-zero packet session id for the
daemon process plus a per-session packet sequence number. It is
intentionally reusable over libp2p streams now and QUIC datagrams later.
Inbound packet acceptance keeps a 64-packet replay window per configured
peer/session pair, so duplicate frames and frames older than the current window
are dropped before they can be written to TUN. Replay windows expire after 15
minutes of inactivity and are capped at 4096 active peer/session windows, which
keeps stale sessions from becoming permanent daemon memory. `daemon-state`
reports the active `replay_windows` count, and the daemon emits a structured
`replay_sessions_expired` log event when periodic maintenance removes stale
replay state.

The libp2p runtime exposes `/p2p-vpn/packet/1` for framed packet exchange over
request-response streams. Inbound packet handling checks the configured peer
allowlist, source-route ownership, and local overlay destination ownership
before writing to TUN. Outbound packets read from the local TUN interface must
also use the local peer's built-in overlay source address before they are queued
for a remote peer. Accepted packet requests receive a compact success response;
rejected requests return a compact rejection reason such as oversized packet,
replay, unauthorized peer, unauthorized source, unauthorized destination,
unexpected payload, or malformed packet instead of relying on request timeout.
The same fixed packet header also carries authorized keepalive and path-probe
frames. Those frames are length-checked, bounded by the effective MTU, and
replay-checked per peer/session, but they are acknowledged without writing any
payload to TUN.

Route ownership is static and exclusive. The local node and each configured peer
own their built-in host routes plus configured route prefixes, but route
compilation rejects overlapping prefixes owned by different peers, including
more-specific prefixes that would hijack another peer's aggregate route. Runtime
route advertisements are treated as claims, not as dynamic routing input:
advertised prefixes must match the local static route table for the
authenticated libp2p peer, and the local node keeps using its configured route
metrics for forwarding decisions.

Outbound packet draining is path-aware. The runtime records direct TCP, direct
QUIC stream, and circuit-relay paths from libp2p connection events, marks paths
unhealthy when their connections close, and only drains a peer's bounded queue
while that peer has a healthy path that the negotiated packet transport can use.
When the selected path changes from relay to direct, or from direct back to
relay, the daemon records promotion/fallback counters and emits a structured
path-selection log event. If an established owned packet-plane datagram path
fails with a send-side transport or session error, the daemon marks that path
unhealthy, records `packet_plane_path_demotions`, emits a
`packet_plane_path_demoted` log event, and lets the next drain pass choose the
best remaining stream or relay path. The same demotion schedules immediate
recovery dials to the peer's configured and discovered addresses, including
when a fallback libp2p connection is still up, and records
`packet_plane_path_recovery_dial_attempts` and
`packet_plane_path_recovery_dial_failures`.
The drain decision is explicit: owned UDP packet-plane datagram, owned QUIC
packet-plane datagram, libp2p stream fallback, or blocked with a reason. The
locked libp2p 0.56 / libp2p-quic 0.13.1 transport disables QUIC datagram
receive buffers internally and exposes only stream muxing through the libp2p
`Swarm`, so the daemon's local native-libp2p datagram capability gate remains
closed: it advertises native libp2p QUIC datagrams as unsupported and cannot
claim a native application datagram sender or receiver. When peers have an
established direct libp2p path, compatible packet-plane capabilities, and an
authenticated owned UDP or owned QUIC packet-plane session, packet frames are
sent over that owned datagram session. Otherwise, the operational fallback is
identity-keyed streams: each
packet frame is sent over libp2p's authenticated request-response channel to the
configured peer ID, and the receiver still applies the overlay allowlist, replay
window, source-route ownership, and local-destination checks before writing to
TUN. The runtime does not hand an unbounded burst of queued packets to
request-response; each peer is limited by the configured packet stream send
window and a small hash of the inner IP flow. Packets from a shard that already
has an in-flight fallback stream stay queued, while other shards for that peer
can continue draining. It also does not report a native libp2p datagram packet
as sent unless a real native-datagram local data plane exists; datagram-only
paths remain blocked instead of silently degrading into fake success.
Configured peers with validated capabilities and a supported path are also sent
periodic path-probe frames, giving operators liveness traffic that does not
depend on user IP packets. When an owned packet-plane datagram session is the
selected path, probes are sent over that UDP session and padded to the selected
path/session MTU so the preferred data plane is exercised directly. Successful
packet-plane probe acknowledgements raise a previously lowered path MTU estimate
one bounded step at a time, capped by the authenticated peer capability and
packet-plane session MTU. Stream and relay paths keep using the authenticated
libp2p packet request-response fallback for probes.
Each active path also carries an MTU estimate. Direct paths start at the local
effective packet MTU, while circuit-relay paths start at a conservative 1200
byte estimate. Outbound queue draining and path probes use the selected path's
estimate as an additional ceiling below the peer-advertised effective MTU, so
an oversized packet is dropped predictably instead of being pushed onto a path
that is known to be smaller. When the original packet is IPv4 or IPv6, the
daemon also writes a local ICMP fragmentation-needed or ICMPv6 packet-too-big
message back to TUN and increments
`outbound_packet_too_big_notifications`. If feedback cannot be emitted, metrics
distinguish no local writer (`outbound_packet_too_big_no_writer`), unparseable
original packets (`outbound_packet_too_big_unparseable`), and write failures
(`outbound_packet_too_big_write_failures`).
Until native datagram sending is implemented, datagram-only paths do not release
packets into the stream fallback; they increment
`outbound_quic_datagram_unavailable_packets` while they remain queued. Packets
for disconnected peers remain bounded by the per-peer queue limits instead of
expanding into unbounded stream requests.
Queued packets also have a configurable age limit; stale packets are dropped on
a runtime expiry tick rather than sent after a long outage. A configured age of
zero is treated as a one millisecond effective age.

Configured bootstrap and peer addresses are redialed periodically when they are
not already connected. This keeps the overlay trying to recover after transient
network loss instead of depending on a one-shot startup dial or a future
discovery event. If outbound packets are queued for a configured peer with no
currently supported path, the runtime also makes a targeted dial attempt for
that blocked peer instead of waiting for the next periodic redial tick.
Addresses learned from mDNS or identify are retained only for configured peers
and are included in the same periodic redial loop. They are refreshed when
rediscovered, removed when mDNS expires them, and aged out after 10 minutes so
stale transient addresses are not retried indefinitely. Redial attempts,
connected-peer skips, failures, and discovered-address expiry are exposed in the
runtime metrics output.
Direct configured peer addresses are dialed during startup. Configured peer
addresses that go through `/p2p-circuit` are deferred during startup, so relay
reservations and relay connections can come up before the node attempts the
relayed peer circuit. Once the daemon observes both relay reservation acceptance
and a relayed listen address for that relay, it immediately dials configured
peers that use the ready relay instead of waiting for the next periodic redial
tick. If the relayed listen address expires, the relay stops being treated as
ready until a replacement address is observed. If the direct connection to the
relay closes with no remaining connection, the daemon clears that relay's
reservation readiness and previous ready-dial attempts so a renewed reservation
can trigger fresh relayed-peer dials.
Relay peers named by configured reservation addresses are also kept as
infrastructure redial targets, so relay fallback can recover even when the
relay is not a packet-routing VPN peer. Generated configs can use
`--relay-peer PEER_ID=MULTIADDR` as a shortcut for shared or public relay
infrastructure: it adds the relay as a Kademlia/bootstrap/AutoNAT reachability
peer and creates the matching `/p2p-circuit` reservation address, but it does
not add the relay to the VPN peer list or grant route authority.
When AutoNAT reports private reachability, the daemon also watches identify
results from connected infrastructure peers. If a connected peer advertises the
libp2p relay hop protocol and a supported TCP or QUIC direct address, the
daemon starts a bounded automatic relay reservation through that peer. This
turns public or shared relay infrastructure into live relay fallback without
requiring every reservation to be written into the config. With Kademlia
enabled, private nodes also query the DHT for nearby peers and provisionally
admit direct TCP or QUIC candidates learned from routing-table updates or
closest-peer results. Those peers remain relay-only infrastructure: Identify
must confirm relay-hop support or the daemon disconnects them, and they do not
gain VPN membership, packet forwarding, control-plane authority, service-plane
authority, or route ownership. Provisional relay-only candidates are also
removed after asynchronous dial failures before connection, so failed public DHT
hints do not stay admitted indefinitely.
Discovered addresses that include an explicit `/p2p/<peer>` target are rejected
when that target does not match the configured peer being learned, including
relayed target addresses after `/p2p-circuit`.
When Kademlia is enabled, the runtime also periodically refreshes the overlay
provider advertisement, reruns the provider lookup, and retries Kademlia
bootstrap. That lets a long-running node find configured peers that join the DHT
after startup rather than relying on the initial one-shot provider query.
Observed external address candidates reported by libp2p identify are passed to
AutoNAT when that behaviour is enabled, and configured bootstrap peers, peer
addresses, and relay-reservation peers are registered as AutoNAT probe servers.
When AutoNAT confirms a public address, the runtime registers it as an
advertised external swarm address, so later identify exchanges and Kademlia
announcements can carry a tested public address alongside any operator-configured
external addresses.
Candidate, scheduled-probe, confirmed, and expired external address events are
exposed in metrics for NAT traversal diagnostics.

Resource limits are part of the overlay config. The packet request-response
fallback has a bounded concurrent stream limit, exposed through `resources`, so
operators can tighten or relax stream pressure without changing code. The
fallback scheduler gates same-shard packets to reduce reordering within an
inner flow while still allowing unrelated shards to use the peer's remaining
stream window. Swarm connection limits are also exposed there so public
bootstrap and relay-capable nodes can bound pending handshakes, established
connections, and connections per
peer. The packet protocol also enforces
`resources.max_inbound_packets_per_peer_per_second` as a per-peer inbound token
bucket before writing any accepted packet to TUN; over-limit frames receive a
compact `rate_limited` packet rejection and are counted separately in metrics.
Runtime validation rejects zero per-peer packet queue capacity, zero concurrent
stream capacity, zero pending handshake capacity, zero inbound packet rate
capacity, and zero total or per-peer established connection capacity, because
those settings would make an otherwise valid node unable to forward VPN traffic.
When `relay.server` is enabled, runtime validation also requires non-zero relay
reservation, circuit, duration, and byte limits so the relay can actually accept
reservations and carry fallback traffic.

The configured interface MTU is treated as the requested packet MTU. The
effective packet MTU is capped by the fixed wire header's `u16` payload length
field and the packet-plane UDP envelope's safe payload ceiling, then used
consistently by the TUN setup and packet forwarder. Runtime validation rejects a
zero interface MTU. The `status` and `up` commands print both configured and
effective MTU values, and `mtu`/`daemon-mtu` report the overlay fragmentation
policy explicitly. The daemon does not fragment overlay packets; it rejects
oversized packets, emits packet-too-big feedback where possible, lowers the
selected path's MTU estimate when an oversized outbound packet proves a smaller
negotiated ceiling, and relies on route MSS hints or the local stack to retry
smaller traffic.

## Development

Enter the reproducible development shell:

```sh
nix develop
```

Generate a libp2p identity:

```sh
cargo run -- keygen
```

Generate a starter node config with a new private key:

```sh
cargo run -- init-config \
  --network lab \
  --output node-a.json \
  --membership-key <base64-32-byte-or-longer-secret> \
  --listen-address /ip4/0.0.0.0/tcp/0 \
  --listen-address /ip4/0.0.0.0/udp/0/quic-v1 \
  --local-route 10.41.0.0/24,100 \
  --queue-max-packets-per-peer 256 \
  --queue-max-bytes-per-peer 524288 \
  --queue-max-packet-age-millis 1000 \
  --max-concurrent-control-streams 64 \
  --max-concurrent-packet-streams 256 \
  --external-address /dns4/node-a.example.net/udp/4001/quic-v1
```

Generate another node config that knows how to dial node A:

```sh
cargo run -- init-config \
  --network lab \
  --output node-b.json \
  --listen-address /ip4/0.0.0.0/tcp/0 \
  --listen-address /ip4/0.0.0.0/udp/0/quic-v1 \
  --peer <node-a-peer-id>=/dns4/<node-a-hostname>/udp/4001/quic-v1 \
  --peer-route <node-a-peer-id>=10.42.0.0/24,100 \
  --bootstrap-peer <node-a-peer-id>=/dns4/<node-a-hostname>/udp/4001/quic-v1
```

Or export a signed invite from node A and import it for node B:

```sh
cargo run -- invite-export \
  --config node-a.json \
  --output node-a.invite.json \
  --expires-at-unix-seconds 1893456000 \
  --membership-epoch 1

cargo run -- invite-import \
  --invite node-a.invite.json \
  --output node-b.json \
  --local-route 10.42.0.0/24,100 \
  --peer-name node-a
```

The invite is signed by node A's existing libp2p identity and binds the inviter
public key to the inviter peer ID. Import verifies the signature, expiration,
protocol constants, wire version, membership tag, peer-address bindings,
discovery settings, and route syntax before writing a config. The invite
carries the current membership key so the imported node can join the same
private overlay; treat invite files as sensitive. `--membership-epoch` and
repeatable `--previous-membership-tag` metadata let operators label membership
key rotations and distribute compatibility hints. Imported configs preserve
those previous tags, and the daemon and remote status query path accept peers
using them while continuing to advertise the current invite key's tag. Invites
also preserve bootstrap peers and relay reservation addresses, and importer-side
validation accepts relayed inviter addresses shaped as
`/p2p/<relay>/p2p-circuit/p2p/<inviter>` while rejecting malformed relay
reservation payloads.

Use `--private-key` to regenerate a config for an existing identity, `--force`
to overwrite an existing file, and `--output -` to print the generated JSON to
stdout. Use the same `--membership-key` value on every node that should join
the private overlay; it must decode to at least 32 bytes. During a staged key
rotation, repeat `--previous-membership-tag BASE64_32_BYTE_TAG` on
`init-config` or `invite-export` for old tags that should remain accepted until
all nodes have moved to the current key. Repeat
`--local-route CIDR[,METRIC]` for prefixes this node is allowed to originate
and advertise. Repeat `--peer PEER_ID=MULTIADDR` for additional peer addresses,
and repeat `--peer-route PEER_ID=CIDR[,METRIC]` for prefixes that peer is
allowed to originate. Peers may omit `MULTIADDR` only when mDNS or Kademlia is
enabled, because relay, DCUtR, and AutoNAT can improve reachability but do not
discover an otherwise unknown peer address. The default generated route metric
is `100`, preserving the built-in host routes at metric `0`. Repeat `--bootstrap-peer
PEER_ID=MULTIADDR` for Kademlia bootstrap nodes. By default the generated
config uses the private
`/p2p-vpn/kad/1` Kademlia protocol; pass `--ipfs-kademlia` as shorthand for
`--kademlia-protocol /ipfs/kad/1.0.0` only when you intentionally want
IPFS/public-DHT protocol compatibility with your bootstrap peers. Add
`--ipfs-bootstrap-peers` with `--ipfs-kademlia` to include the well-known public
IPFS bootstrap multiaddrs in the generated config. Public bootstrap peers are
reachability infrastructure only: they can help route Kademlia queries, AutoNAT
probes, and relay/DCUtR setup, but configured peer IDs and route ownership still
define the VPN overlay. DNS multiaddrs, including `/dns4`, `/dns6`, `/dns`, and
`/dnsaddr`, are resolved by the libp2p transport for startup dials and redials.
Use `bootstrap-check` before starting the TUN daemon when you need a rootless
smoke test that the configured bootstrap peers are reachable through libp2p; the
binary enables libp2p RSA peer identity decoding so the legacy public IPFS
bootstrap peer IDs can be dialed.
Repeat `--external-address MULTIADDR` for stable public or DNS addresses that
libp2p should advertise to peers in addition to observed addresses learned
through identify and confirmed through AutoNAT. Use
`--relay-peer PEER_ID=MULTIADDR` for a shared or public circuit-relay node that
should be used as reachability infrastructure; the address should be the
relay's direct address, without `/p2p-circuit`. The generated config also adds
that relay as a bootstrap/AutoNAT probe peer so reservation setup can recover
after restart or transient network loss. Repeat `--relay-reservation MULTIADDR`
when you already have the exact relay reservation listen address.
Use
`--disable-mdns`, `--disable-kademlia`, `--disable-dcutr`, or
`--disable-autonat` to omit optional discovery and NAT traversal behaviours from
the generated config. `--disable-kademlia` also disables provider
advertisement; hand-written configs that set
`kademlia_provider_advertisement=true` while `kademlia=false` are rejected as
not runtime-ready. Use `--disable-kademlia-provider-advertisement` on
bootstrap-only nodes that should route Kademlia queries without advertising
themselves as VPN packet providers. Use the `--queue-*`, `--max-concurrent-*`,
`--max-inbound-packets-per-peer-per-second`, and `--max-*-connections*` flags to
tune per-peer packet buffering, packet rate pressure, stream pressure, and swarm
connection limits in generated configs instead of hand-editing JSON.
Relay-capable nodes can also set `--relay-max-reservations`,
`--relay-max-reservations-per-peer`, `--relay-reservation-duration-secs`,
`--relay-max-circuits`, `--relay-max-circuits-per-peer`,
`--relay-max-circuit-duration-secs`, and `--relay-max-circuit-bytes`.

Run local checks:

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
nix flake check
nix build .#default
```

`nix flake check` builds and tests the package, checks formatting, runs clippy
with warnings denied, evaluates the NixOS module, and on Linux runs a NixOS VM
smoke check that starts the packaged systemd service and queries its daemon
control socket.

Build or run the packaged CLI with Nix:

```sh
nix build .#default
nix run . -- status --config p2p-vpn.json
```

The flake exports packages, apps, development shells, and checks for
`x86_64-linux`, `aarch64-linux`, and `aarch64-darwin`. `x86_64-darwin` is not
exported because the pinned nixpkgs branch no longer supports that platform.

Install the CLI into a user profile from a checked-out tree or a Git URL:

```sh
nix profile install .#default
nix profile install github:hermetic-foundation/p2p-vpn#default
```

Build a reproducible release archive for the current system:

```sh
nix build .#releaseArchive
system=$(nix eval --raw --impure --expr builtins.currentSystem)
nix build ".#checks.${system}.releaseArchiveSanity"
```

The archive contains the packaged `p2p-vpn` binary, README, flake lock, NixOS
module, the feature matrix, and the `nixos-mesh` deployment template. Release
builds should also run `nix flake check` so the binary package, release archive,
release-archive sanity check, formatter, clippy check, and NixOS module
evaluation are all verified before publishing. The release sanity check unpacks
the archive, verifies the expected operational docs, NixOS module, and template
are present, rejects unsafe archive paths, and confirms the archived CLI can
print its help text.

The current feature-completeness audit lives in
[`docs/feature-matrix.md`](docs/feature-matrix.md). It links each major
Hyprspace-style requirement to implementation evidence, verification commands,
and known remaining gaps.

Run the Linux TUN end-to-end smoke test on hosts that allow unprivileged user
and network namespaces:

```sh
nix run .#namespace-preflight
cargo test --test tun_namespace -- --ignored --nocapture
nix run .#tun-e2e
```

`namespace-preflight` checks the host can create a rootless user namespace,
network namespace, veth pair, and TUN device before you wait for the ignored
test binary to build. `tun-e2e` runs the same preflight by default; set
`P2P_VPN_TUN_E2E_SKIP_PREFLIGHT=1` only when you need to bypass that host check.

The namespace suite covers direct static peer addresses, mDNS peer discovery,
Kademlia/bootstrap peer discovery, circuit-relay fallback, signed-invite
onboarding over a relayed inviter address, and relay-to-direct promotion with
DCUtR and AutoNAT enabled. A focused namespace smoke also covers direct owned
QUIC packet-plane forwarding. To run only the NAT traversal and path promotion
case:

```sh
nix run .#tun-e2e -- tun_namespace_relay_overlay_promotes_to_direct_path -- --ignored --exact --nocapture
```

To run only the owned QUIC packet-plane case:

```sh
nix run .#tun-e2e -- tun_namespace_ping_crosses_owned_quic_packet_plane -- --ignored --exact --nocapture
```

Set `P2P_VPN_TUN_E2E_KEEP_TEMP=1` to preserve generated configs and per-node
logs after successful namespace runs. See
[`docs/network-debugging.md`](docs/network-debugging.md) for focused repro and
artifact-inspection commands.

For a preserved-artifact namespace repro, use:

```sh
nix run .#namespace-repro
```

Recorded namespace E2E smoke evidence is kept in
`docs/namespace-e2e-smoke.md`.

These tests intentionally stay outside `nix flake check` because they need a
host kernel that permits user namespaces, network namespaces, veth setup, and
`/dev/net/tun`.

## Example Config

```json
{
  "network": {
    "name": "lab",
    "local_peer": "12D3KooW...",
    "private_key": "CAES...",
    "membership_key": "base64-encoded-32-byte-or-longer-secret",
    "routes": [
      {
        "prefix": "10.41.0.0/24",
        "metric": 100
      }
    ],
    "listen_addresses": [
      "/ip4/0.0.0.0/tcp/0",
      "/ip4/0.0.0.0/udp/0/quic-v1"
    ],
    "external_addresses": [
      "/dns4/node-a.example.net/udp/4001/quic-v1"
    ],
    "bootstrap_peers": [
      {
        "id": "12D3KooW...",
        "address": "/dns4/bootstrap.example.net/tcp/4001"
      }
    ],
    "discovery": {
      "mdns": true,
      "kademlia": true,
      "kademlia_provider_advertisement": true,
      "kademlia_protocol": "/p2p-vpn/kad/1",
      "dcutr": true,
      "autonat": true
    },
    "relay": {
      "server": false,
      "reservations": [
        "/dns4/relay.example.net/tcp/4001/p2p/12D3KooWRelay.../p2p-circuit"
      ],
      "resources": {
        "max_reservations": 128,
        "max_reservations_per_peer": 4,
        "reservation_duration_secs": 3600,
        "max_circuits": 16,
        "max_circuits_per_peer": 4,
        "max_circuit_duration_secs": 120,
        "max_circuit_bytes": 131072
      }
    }
  },
  "interface": {
    "name": "hs0",
    "mtu": 1280
  },
  "queue": {
    "max_packets_per_peer": 256,
    "max_bytes_per_peer": 524288,
    "max_packet_age_millis": 1000
  },
  "resources": {
    "max_concurrent_control_streams": 64,
    "max_concurrent_packet_streams": 256,
    "max_inbound_packets_per_peer_per_second": 4096,
    "max_pending_incoming_connections": 64,
    "max_pending_outgoing_connections": 64,
    "max_established_incoming_connections": 256,
    "max_established_outgoing_connections": 256,
    "max_established_connections_per_peer": 8,
    "max_established_connections": 512
  },
  "peers": [
    {
      "id": "12D3KooW...",
      "name": "node-a",
      "addresses": [
        "/ip4/192.0.2.10/tcp/4001",
        "/ip4/192.0.2.10/udp/4001/quic-v1"
      ],
      "routes": [
        {
          "prefix": "10.42.0.0/24",
          "metric": 10
        }
      ]
    }
  ]
}
```

`bootstrap_peers` are dialed and added to the configured Kademlia routing table
when Kademlia discovery is enabled. TCP and QUIC startup dials support DNS
multiaddrs, including `/dns4`, `/dns6`, `/dns`, and `/dnsaddr`. Kademlia nodes
query a provider key for other configured peers. Overlays without
`network.membership_key` use a network-name provider key for compatibility.
When `network.membership_key` is configured, the provider key is scoped by the
derived membership tag so public/IPFS-compatible DHT nodes can assist discovery
without exposing the overlay under a network-name-only rendezvous key. Nodes
with `discovery.kademlia_provider_advertisement` enabled also advertise
themselves under that key; disable it on public/bootstrap infrastructure that
should help route discovery without claiming to be an overlay VPN endpoint.
During membership-key rotation, runtime refreshes query the current provider key
and any `network.previous_membership_tags`, but provider advertisement remains
limited to the current key.
`network.routes` are local route-ownership claims. The node advertises them in
the control handshake and accepts outbound TUN packets sourced from those
prefixes; other peers must configure matching `peer.routes` entries for this
node before they will accept those packets.
`discovery.kademlia_protocol` defaults to the private `/p2p-vpn/kad/1`
protocol. Set it to `/ipfs/kad/1.0.0` only for deployments that explicitly use
IPFS-compatible public bootstrap peers; `init-config --ipfs-kademlia
--ipfs-bootstrap-peers` writes the default public bootstrap multiaddrs into the
config. Those peers help discovery and NAT traversal, but do not grant overlay
membership or route authority.
`external_addresses` are registered with the libp2p swarm as explicit
advertised addresses; use them for stable public socket, DNS, or port-forwarded
addresses that peers should prefer over wildcard listen addresses. Confirmed
AutoNAT public addresses are registered the same way at runtime. Bootstrap,
configured peer, and relay-reservation addresses are also registered as AutoNAT
probe servers when AutoNAT is enabled, so public libp2p/IPFS infrastructure can
assist reachability checks without becoming VPN membership or routing authority.
`relay.reservations` are full libp2p relay listen addresses; listening on
one asks that relay for a circuit relay v2 reservation. Peer `addresses` may
also contain full relayed target addresses such as
`/dns4/relay.example.net/tcp/4001/p2p/<relay>/p2p-circuit/p2p/<peer>`. Set
`relay.server` to `true` on nodes that should accept relay reservations and
relay circuits for the overlay. `relay.resources` maps to circuit relay v2
server limits; its defaults match libp2p's relay defaults while retaining the
library's default rate limiters. Nodes with `relay.server` enabled reject zero
relay reservation, circuit, duration, or byte limits during config validation.

Inspect the compiled local view:

```sh
cargo run -- status --config p2p-vpn.json
```

`status` validates the config before printing the compiled view. It checks that
the private key matches `network.local_peer`, the optional membership key is
valid base64 key material, configured routes compile, all listen and external
multiaddrs parse, bootstrap and peer multiaddrs either omit an explicit peer id
or match the configured peer, and relay reservation multiaddrs contain
`/p2p/<relay>/p2p-circuit`. It also checks that packet queues, established
connection capacity, enabled relay-server limits, and the configured interface
MTU are non-zero. The output includes the local overlay IPv4/IPv6
addresses plus built-in and configured route ownership lines, which are the
claims peers must mirror in their `peer.routes` entries before accepting routed
traffic from this node.

Inspect the compiled route table or resolve a destination address:

```sh
cargo run -- routes --config p2p-vpn.json
cargo run -- routes --config p2p-vpn.json --resolve 10.42.0.9
```

`routes` prints every built-in and configured route after validation, including
the owning local or peer identity, peer name when configured, metric, and
whether the route came from explicit config or generated host-route ownership.
With `--resolve`, it also shows the exact route owner selected by the current
longest-prefix and metric ordering for the destination.

Inspect packet MTU, route MSS hints, and path MTU estimates:

```sh
cargo run -- mtu --config p2p-vpn.json
cargo run -- mtu --config p2p-vpn.json --live --timeout-seconds 10
```

`mtu` reports the configured interface MTU, the effective packet MTU after wire
and packet-plane payload capping, the packet header length, the wire max packet
payload length, packet-plane datagram overhead, packet-plane max payload length,
every compiled route's Linux `advmss` hint, and configured direct/relay path MTU
estimates. Live mode queries configured peers and reports each peer's advertised
effective MTU, the local negotiated ceiling, preferred path, estimated path MTU,
and remote packet-plane payload limits when the peer reports them.

List configured peers and their route/address inventory:

```sh
cargo run -- peers --config p2p-vpn.json
```

Add `--live` to query every configured peer's validated control and service
status:

```sh
cargo run -- peers --config p2p-vpn.json --live --timeout-seconds 10
```

`peers` prints each configured peer id, optional name, dial addresses, built-in
host routes, and configured route claims. In live mode it also reports whether
each peer was reachable, the validated remote network, membership-key match,
effective MTU, QUIC datagram support, preferred packet path, and advertised
routes. Unreachable peers are reported per peer instead of aborting the whole
inspection run.

Inspect configured path candidates and live remote path capability:

```sh
cargo run -- paths --config p2p-vpn.json
cargo run -- paths --config p2p-vpn.json --live --timeout-seconds 10
```

`paths` classifies configured peer dial addresses as direct QUIC stream, direct
TCP stream, or circuit relay paths, shows their default promotion scores, and
prints the initial MTU estimate for each configured address. Addressless peers
are marked as depending on mDNS or Kademlia discovery. Live mode queries each
configured peer's validated control and service status and reports its
preferred packet path, effective MTU, selected path MTU estimate, QUIC datagram
support, and whether the current capability set can carry path probes. Active
daemon connection counts, selected path MTU, and path-probe counters are exposed
through `daemon-state`.

Inspect the local capability contract or validate configured peers' remote
capabilities:

```sh
cargo run -- capabilities --config p2p-vpn.json
cargo run -- capabilities --config p2p-vpn.json --live --timeout-seconds 10
```

`capabilities` prints the control-plane capability values this node advertises:
network name, membership-key match state, wire version, packet protocol, packet
header length, effective MTU, preferred packet path, QUIC datagram support, and
advertised routes. Live mode queries every configured peer and prints the same
validated remote fields, reporting unreachable peers without aborting the whole
inspection run.

Check configured bootstrap reachability without opening a TUN interface:

```sh
cargo run -- bootstrap-check --config p2p-vpn.json --timeout-seconds 30
cargo run -- bootstrap-check --config p2p-vpn.json --require-all --timeout-seconds 30
cargo run -- bootstrap-check --config p2p-vpn.json --require-relay-reservations --timeout-seconds 30
cargo run -- bootstrap-check --config p2p-vpn.json --require-autonat-status --timeout-seconds 30
cargo run -- bootstrap-check --config p2p-vpn.json --require-dcutr-ready --timeout-seconds 30
cargo run -- bootstrap-check --config p2p-vpn.json --require-dcutr-success --timeout-seconds 30
cargo run -- bootstrap-check --config p2p-vpn.json --require-relayed-peer-circuits --timeout-seconds 30
cargo run -- bootstrap-check --config p2p-vpn.json --write-report bootstrap-check.json --force
```

`bootstrap-check` builds the same libp2p host used by the daemon, starts the
configured bootstrap dials, Kademlia bootstrap, rendezvous lookup/advertise, and
AutoNAT probe-server registration, then waits for bootstrap peer connections.
By default the check succeeds when any configured bootstrap peer connects, which
fits public IPFS bootstrap sets where individual public nodes can be
temporarily unavailable. Use `--require-all` for private infrastructure where
every configured bootstrap peer is expected to be reachable. The output reports
the Kademlia protocol, whether it is IPFS-compatible, the success threshold,
DCUtR enablement/readiness, Kademlia startup state, AutoNAT probe-server count,
observed AutoNAT status, per-peer connection state, and dial-failure counts.
Pass `--write-report PATH` to persist the same bootstrap/DCUtR/AutoNAT/relay
state as JSON for later comparison; existing files require `--force`.
Use `--require-autonat-status` to make the command fail unless AutoNAT has
registered at least one probe server and observed a non-unknown `public` or
`private` status before the timeout. When the config contains relay reservation
listen addresses, the same command also reports configured relay reservations,
accepted relay reservation events, relayed listen address readiness, and
per-relay acceptance/listen-address state. Use `--require-relay-reservations` to
make the command fail unless every configured relay reservation is accepted and
that same relay has a corresponding relayed listen address. Use
`--require-dcutr-ready` to fail unless DCUtR is enabled and the relay
reservation prerequisites for DCUtR are usable on each configured relay;
this is a rootless readiness check, not proof that a remote peer has completed a
hole punch. Use `--require-dcutr-success` to fail unless the check observes a
successful libp2p DCUtR hole-punch event before the timeout. Use
`--require-relayed-peer-circuits` when the VPN peer list contains full relayed
target addresses such as
`/dns4/relay.example.net/tcp/4001/p2p/RELAY/p2p-circuit/p2p/PEER`; the command
dials each configured relayed peer target, reports per-peer circuit status, and
fails unless every configured relayed peer circuit connects before the timeout.
It does not add bootstrap or relay peers to VPN membership or grant route
authority.

Run the ignored live public relay smokes when you want to validate a specific
shared relay, or a small candidate set of relays, outside the deterministic
local test suite:

```sh
cargo run -- relay-scan --ipfs-bootstrap-peers --timeout-seconds 30

cargo run -- relay-scan \
  --ipfs-bootstrap-peers \
  --write-candidates public-relay-candidates.txt \
  --write-report public-relay-scan.json \
  --timeout-seconds 30

cargo run -- relay-check \
  --config p2p-vpn.json \
  --relay-candidates-file public-relay-candidates.txt \
  --write-report public-relay-check.json \
  --write-config p2p-vpn-public-relay.json \
  --max-validation-candidates 6 \
  --timeout-seconds 45

cargo run -- relay-scan \
  --ipfs-bootstrap-peers \
  --check-candidates \
  --write-config p2p-vpn-public-relay.json \
  --max-validation-candidates 6 \
  --candidate-timeout-seconds 45 \
  --timeout-seconds 30

cargo run -- relay-check \
  --relay-candidate /dns4/relay.example.net/tcp/4001/p2p/RELAY \
  --timeout-seconds 45

cargo run -- relay-check \
  --relay-candidate /dns4/relay.example.net/tcp/4001/p2p/RELAY \
  --require-dcutr-success \
  --max-validation-candidates 4 \
  --write-report public-relay-dcutr.json \
  --timeout-seconds 45

cargo run -- relay-check \
  --relay-candidate /dns4/relay.example.net/tcp/4001/p2p/RELAY \
  --write-config p2p-vpn-public-relay.json \
  --timeout-seconds 45
```

To prove public-relay-assisted DCUtR across a real two-host topology, keep a
listener alive on Host A and dial its descriptor from Host B. First choose a
validated relay candidate from `relay-scan --write-candidates` or
`relay-check`.

On Host A:

```sh
cargo run -- relay-dcutr-listen \
  --relay-candidate /dns4/relay.example.net/tcp/4001/p2p/RELAY \
  --write-descriptor public-dcutr-listener.json \
  --serve-seconds 900 \
  --force
```

Copy `public-dcutr-listener.json` to Host B while Host A is still serving, then
run:

```sh
cargo run -- relay-dcutr-dial \
  --descriptor public-dcutr-listener.json \
  --timeout-seconds 90 \
  --write-report public-dcutr-dial-report.json \
  --force
```

The descriptor contains the relay candidate, relay peer ID, listener peer ID,
and relayed listener address. The dial report wraps the usual bootstrap/DCUtR
report with that descriptor so two-machine results can be compared without
hand-copying logs.

`relay-scan` is rootless. It dials configured or supplied bootstrap peers,
waits for libp2p Identify responses, and prints direct relay candidate
TCP/QUIC multiaddrs for peers that advertise the circuit-relay v2 hop protocol.
Use `--config p2p-vpn.json` to scan the config's bootstrap peers, repeat
`--bootstrap-peer PEER_ID=MULTIADDR` to scan explicit peers, or pass
`--ipfs-bootstrap-peers` to scan the bundled public IPFS bootstrap set and
actively sample additional peers through the public `/ipfs/kad/1.0.0` routing
table. Scan output reports both configured bootstrap peers and total scanned
peers, whether the active closest-peer lookup started and finished, how many
peer records it returned, and how many routing-table peers were discovered.
The bounded candidate set prefers distinct relay peers when it can replace
duplicate addresses from an already represented relay. A discovered candidate
is only a hint that the peer advertises relay-hop support and has a direct
TCP/QUIC address this binary can dial; run `relay-check` against the printed
candidate, or pass `--check-candidates` to validate scanned candidates
immediately, before using it for a VPN reservation or DCUtR path. Validation
tries scanned candidates round-robin by relay peer, so a candidate set with
many addresses for one relay still gives other relays an early chance before
cycling through alternate addresses. Within each relay peer, validation tries
QUIC-capable addresses before TCP addresses so bounded public DCUtR searches
spend early attempts on the transports most likely to support hole punching.
Pass `--write-candidates PATH` to write the ordered direct relay candidates as
newline-separated multiaddrs for repeatable `relay-check --relay-candidates-file
PATH` runs. Pass `--write-report PATH` to preserve scan metadata, routing-peer
counts, identified relay-hop peers, candidate addresses, and peer-level dial
failures before validation begins.
Validation skips relay candidates that require IPv4 or IPv6 when the local host
has no usable route for that address family and prints each skip as
`public relay scan validation skipped: ... reason ipv4_unreachable` or
`reason ipv6_unreachable`. Use
`--max-validation-candidates N` to bound a validation pass after host
reachability filtering; this is useful for public DCUtR searches where each
candidate has a single end-to-end reservation plus circuit/DCUtR timeout
budget. Add `--require-dcutr-success` with `--check-candidates` when the scan
should only pass after a successful public-relay-assisted hole punch. Pass
`--write-config PATH` with
`--check-candidates` to write the same default relay-assisted config that
`relay-check --write-config` writes from the first validated scanned candidate.
When `relay-scan` also receives `--config p2p-vpn.json`, the written config
preserves that overlay's identity, membership, peers, routes, queue limits, and
packet-plane settings while adding only the validated relay bootstrap and
reservation infrastructure.

`relay-check` is rootless. In its default mode it creates a temporary listener,
reserves a circuit on the supplied relay, and dials that listener from a second
temporary node through the relay. With `--require-dcutr-success`, it also gives
both temporary nodes TCP and QUIC direct listen sockets and fails unless libp2p
reports a successful DCUtR hole-punch event and the probe observes a direct
non-relayed connection to the target peer. Repeat `--relay-candidate` to try a small
candidate set; the command tries candidates round-robin by relay peer, with
QUIC-capable addresses before TCP alternates for the same relay, then stops
after the first usable relay and prints candidate-level failures when none
work. Use `--relay-candidates-file PATH` to consume the newline-separated
candidate file produced by `relay-scan --write-candidates`; file candidates can
be combined with repeated `--relay-candidate` flags and use the same
round-robin ordering before validation. Before probing, it skips relay
candidates that require IPv4 or IPv6 when
the local host has no usable route for that address family and prints each skip
as `public relay check skipped: ... reason ipv4_unreachable` or
`reason ipv6_unreachable`. Use `--max-validation-candidates N` to bound the
manual check after host reachability filtering; this is useful for public DCUtR
searches where each candidate has a single end-to-end reservation plus
circuit/DCUtR timeout budget. When that validation cap is set, `relay-check`
accepts a larger bounded scan artifact and applies ordering, host reachability
filtering, and truncation before it opens public relay probes. Each candidate
line includes a stable `failure_stage` value: `candidate_setup`,
`relay_reservation`, `relayed_peer_circuit`, `dcutr_success`, or `none` for a
usable candidate.
The report also prints a `public relay candidate failure stages:` summary with
per-stage counts across the attempted candidate set, which is useful for
comparing bounded public scans.
Failed candidates report whether the probe connected directly to the relay,
whether relay reservation acceptance or relayed listen-address publication
timed out, and the last direct relay dial error when one was observed. When the
probe reaches the bootstrap-check phase, the failed candidate also includes the
same reservation, relayed-circuit, AutoNAT, and DCUtR detail lines as
`bootstrap-check`, including `dcutr last_error` for the most recent libp2p
hole-punch failure. Pass `--write-report PATH` to save a pretty-printed JSON
artifact with schema version, probe mode, timeout, validation cap,
host-reachable candidates, skipped candidates with reasons, per-candidate
success, failure stage, elapsed milliseconds, error, bootstrap/DCUtR summary,
and observed relayed/direct connection addresses for each candidate that reached
the bootstrap-check phase.
Successful candidates also print a
`public relay candidate config:` line containing the exact
`--relay-peer PEER=MULTIADDR` shortcut value and matching full
`--relay-reservation .../p2p-circuit` address for `init-config`. Pass
`--write-config PATH` to write a runtime-valid config from the first validated
relay candidate; add `--config p2p-vpn.json` when that output should preserve
an existing overlay's identity, membership, peers, routes, queue limits, and
packet-plane settings. The relay is added as bootstrap/AutoNAT/relay
infrastructure only and is not added as a VPN peer or route owner. Use
`--force` to overwrite an existing output file. Each supplied relay multiaddr
must be the relay's direct address with its `/p2p/RELAY` peer ID and without
`/p2p-circuit`.

The packaged public relay repro app is the preferred way to collect a
repeatable failure bundle:

```sh
P2P_VPN_REPRO_DIR=public-relay-repro \
P2P_VPN_REPRO_BASE_CONFIG=p2p-vpn.json \
nix run .#public-relay-repro
```

In addition to the scan/check/DCUtR JSON reports and candidate file, it writes
`repro-metadata.txt`, `repro-host-network.txt`, `repro-commands.sh`,
`repro-dcutr-listen-host-a.sh`, `repro-dcutr-dial-host-b.sh`, and
`repro-summary.txt`. The two host scripts use the first successful relay-check
candidate, or `P2P_VPN_REPRO_RELAY_CANDIDATE` when supplied, to turn the manual
Host A/Host B DCUtR proof into a repeatable handoff. Tune those scripts with
`P2P_VPN_REPRO_DCUTR_SERVE_SECONDS` and
`P2P_VPN_REPRO_DCUTR_DIAL_TIMEOUT_SECONDS`. The summary captures phase exit statuses, candidate
durations, candidate counts, skipped-candidate counts, routing-peer counts,
failure-stage counts, candidate elapsed-time ranges, and the first candidate
error from each report. For relay-check reports, it also summarizes observed
relayed and direct connection addresses. The host-network file captures
OS/kernel, IPv4/IPv6 route availability, interface addresses, and route tables
so public relay and DCUtR failures can be compared across machines. The
commands file records the exact probe commands and environment limits used for
the run so the same candidate set can be replayed without starting over from
memory. Set
`P2P_VPN_REPRO_CANDIDATES_FILE` to a nonempty candidate file from an earlier
run to skip the public discovery phase and immediately replay relay-circuit and
DCUtR validation against those relay candidates.

Once `public-relay-repro` writes `public-relay-config.json`, generate a
two-host VPN data-plane runbook with:

```sh
P2P_VPN_VPN_REPRO_PUBLIC_RELAY_DIR="$P2P_VPN_REPRO_DIR" \
P2P_VPN_VPN_REPRO_PING_TARGET=10.42.0.2 \
nix run .#public-vpn-repro
```

The generated Host A/Host B scripts start `p2p-vpn up`, wait on daemon health
requirements, capture daemon state/path/status artifacts, run a ping proof when
`P2P_VPN_VPN_REPRO_PING_TARGET` is set, and write stable logs for comparing the
same public relay-assisted overlay across two machines.

The ignored tests run the same kind of probe from the test harness:

```sh
P2P_VPN_LIVE_RELAY_MULTIADDR=/dns4/relay.example.net/tcp/4001/p2p/RELAY \
  cargo test runtime::bootstrap_check::tests::bootstrap_check_can_probe_live_public_relayed_peer_circuit \
  -- --ignored --exact --nocapture

P2P_VPN_LIVE_RELAY_MULTIADDR=/dns4/relay.example.net/tcp/4001/p2p/RELAY \
  cargo test runtime::bootstrap_check::tests::bootstrap_check_can_probe_live_public_dcutr_success \
  -- --ignored --exact --nocapture
```

Use `P2P_VPN_LIVE_RELAY_MULTIADDRS` for up to eight comma, semicolon, or
newline-separated candidate relay addresses. The single
`P2P_VPN_LIVE_RELAY_MULTIADDR` variable is still accepted for one relay. Each
supplied relay multiaddr must be the relay's direct address with its
`/p2p/RELAY` peer ID and without `/p2p-circuit`. Set
`P2P_VPN_LIVE_RELAY_TIMEOUT_SECONDS` to widen or shrink the per-candidate probe
budget; it defaults to 45 seconds. The tests try candidates until one succeeds
and report all candidate failures if none work. The relayed-peer
smoke creates a temporary listener, reserves a circuit on that relay, then uses
`bootstrap-check` against a second temporary node to prove the relayed target can
be dialed. Use `--require-dcutr-success` only when the evidence target is
relay-assisted direct promotion; the DCUtR smoke gives both temporary nodes TCP
and QUIC direct listen sockets and fails unless `bootstrap-check` observes a
successful DCUtR hole-punch event plus a direct non-relayed connection to the
target peer.

Recorded public bootstrap smoke evidence is kept in
`docs/public-bootstrap-smoke.md`. Recorded 2026-08-04 runs prove public
IPFS-compatible bootstrap connectivity, AutoNAT observation, public
Kademlia-assisted relay discovery, public relay reservation, relayed circuit
dialing, and relay-assisted config generation. Public-relay-assisted DCUtR
proof still requires a suitable relay plus NAT topology that completes a hole
punch for the test.

Inspect the runtime metric names and startup snapshot:

```sh
cargo run -- metrics --config p2p-vpn.json
cargo run -- metrics --config p2p-vpn.json --format prometheus
```

The metrics output includes control-plane exchange counters, capability
acceptance and rejection counters by reason, packet counters, queue occupancy,
oldest queued packet age in milliseconds, total queue drops, queue expiry drops,
inbound accepted IP packets, accepted keepalive and path-probe frames, outbound
path-probe send/ACK/failure counters, path-MTU update and probe-confirmation
counters, packet-too-big notification/skipped/failed counters, packet-plane
session expiry counters, inbound and outbound packet drop reasons including
packet-plane receive rejection counters by stable reason
(`unknown_endpoint`, `decrypt`, `replayed_datagram`, `payload_too_large`, and
the other packet-plane datagram parser/session failures), rate-limited inbound
frames, expired outbound queue packets, stream fallback sends, attempted native
QUIC datagram sends, datagram-unavailable queue stalls, direct versus
relayed connection counts, selected-path promotions to direct, selected-path
fallbacks to relay, packet-plane path demotions, relay reservation/circuit
counts including client-side `relay_reservations_lost`, auto-relay
candidate/reservation attempt/failure counts,
auto-relay infrastructure candidate/dial/failure counts, packet-plane
recovery dial attempts/failures, relay-server
accept/deny/close/timeout counts, DCUtR success/failure counts, observed
external address candidate/scheduled-probe/confirmed/expired counts, AutoNAT
current public/private/unknown reachability gauges and status-change counters,
service-plane request/response/status/rejection/failure counters, Kademlia
provider lookup/result/ignored-provider/configured-provider-dial/advertisement
and bootstrap refresh counts, unauthorized connection drops, configured peer
redial counters, accepted/dialed/rejected/expired discovered-address counters,
asynchronous outgoing connection error counts, healthy path counts by transport
kind, configured peers with and without a currently supported packet path, and
queue-drain stalls caused by peers having no currently supported packet path or a
full packet stream send window.
Use `--format prometheus` on `metrics` or `daemon-status` to emit only
Prometheus-compatible numeric samples with the `p2p_vpn_` prefix, suitable for
textfile collection or ad hoc scrape validation.
Those counters are intended to show whether a deployment is
exchanging capabilities, checking service status, probing and advertising
observed public addresses, refreshing DHT discovery, rejecting bad discovered
addresses, using direct paths, relay fallback, hole-punching, enforcing
membership, recovering connections, and waiting on path negotiation as expected.
Outbound drop-reason counters also include packet requests rejected by the
remote peer, so peer-side MTU, authorization, replay, and payload validation
failures remain visible on the sender.

Query a configured peer's live control and service status without opening a
TUN interface:

```sh
cargo run -- peer-status <peer-id> --config p2p-vpn.json --timeout-seconds 10
```

`peer-status` builds the local libp2p host from the config, dials the target
when a configured address is available, and exchanges the same bounded
control-plane capabilities and service-plane status messages used by the
runtime. The target peer must already be present in `peers`; the response is
accepted only when the remote advertises the expected network, membership tag,
wire protocol, MTU shape, preferred path, and route ownership. The output shows
the validated packet protocol, effective MTU, QUIC datagram support flag,
packet-plane session TTL when the peer reports it, preferred path, and
advertised routes. Older peers that do not report the TTL are shown as
`unknown`.

Run as a NixOS service:

```nix
{
  inputs.p2p-vpn.url = "github:hermetic-foundation/p2p-vpn";

  outputs = { nixpkgs, p2p-vpn, ... }: {
    nixosConfigurations.node-a = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        p2p-vpn.nixosModules.default
        {
          services.p2p-vpn.instances.node-a = {
            enable = true;
            configFile = "/etc/p2p-vpn/node-a.json";
            metricsIntervalSeconds = 10;
            openFirewall = true;
            tcpPorts = [ 4001 ];
            udpPorts = [ 4001 ];
            packetPlaneUdpPorts = [ 51820 ];
            packetPlaneQuicPorts = [ 51821 ];
          };
        }
      ];
    };
  };
}
```

The NixOS module exports named systemd units such as `p2p-vpn-node-a.service`,
loads the `tun` kernel module, runs `p2p-vpn up --config ...`, adds `iproute2`
to the unit path for interface setup, grants `CAP_NET_ADMIN` and `CAP_NET_RAW`,
restarts on failure, and can open declared TCP/UDP listen ports in the NixOS
firewall. Use `tcpPorts` and `udpPorts` for libp2p transports, then use
`packetPlaneUdpPorts` and `packetPlaneQuicPorts` for owned packet-plane
listeners declared in `network.packet_plane.listen` and
`network.packet_plane.quic_listen`. The Linux flake checks include
`checks.x86_64-linux.nixos-vm-smoke`,
which boots a NixOS VM, starts a module-managed instance, queries
`daemon-status` and `daemon-health` over its control socket, stops the unit, and
confirms orderly socket cleanup. Keep JSON configs that contain
`network.private_key` or
`network.membership_key` outside the Nix store, for example under
`/etc/p2p-vpn`, with permissions managed by your deployment system. Those JSON
configs can tune packet-plane expiry with
`network.packet_plane.session_ttl_seconds`, defaulting to 600 seconds, and
packet-plane replay-state memory with
`network.packet_plane.max_replay_windows_per_session`, defaulting to 1024.
Both values must be nonzero. A two-node deployment skeleton is available as the
`nixos-mesh` flake template in
`examples/nixos-mesh`.

The daemon handles Ctrl-C and systemd's SIGTERM as orderly shutdown requests.
When a NixOS instance has `controlSocket` enabled, the systemd unit first runs
`p2p-vpn daemon-shutdown --socket ...` as `ExecStop`; SIGTERM and
`TimeoutStopSec` remain as the fallback if the local control socket is disabled
or unavailable. On shutdown it stops the libp2p runtime loop, prints a final
metrics snapshot, and exits successfully. Runtime lifecycle and high-value
network events are written to stderr as key-value lines such as
`level=info event=connection_established peer=<peer-id> relayed=false`, which
keeps journald output grep-friendly without requiring a separate logging
collector. Rejected packet, control, and service requests emit warn-level audit
events (`packet_rejected`, `packet_response_rejected`,
`control_capabilities_rejected`, and `service_status_rejected`) with the peer,
reason, and safe packet metadata such as payload type, session, sequence,
payload length, and parsed IP endpoints when present. Packet payload bytes and
secrets are not logged.

Inspect the Linux interface setup plan without requiring root:

```sh
cargo run -- up --config p2p-vpn.json --dry-run
```

Remote route commands include the effective overlay MTU and, where the MTU is
large enough, a Linux `advmss` hint derived from the IP family. This avoids
silent oversized TCP segments on routed overlay prefixes without adding
fragmentation inside the p2p-vpn packet protocol. The same invariant is exposed
as `overlay fragmentation: disabled` in configured, live-peer, and daemon MTU
output.

Attempt to create the TUN device and install routes:

```sh
sudo target/debug/p2p-vpn up --config p2p-vpn.json
```

Print live forwarding counters while the runtime is up:

```sh
sudo target/debug/p2p-vpn up --config p2p-vpn.json --metrics-interval-seconds 10
```

Expose the daemon's live status over a local Unix socket:

```sh
sudo target/debug/p2p-vpn up \
  --config p2p-vpn.json \
  --control-socket /run/p2p-vpn/control.sock

cargo run -- daemon-status --socket /run/p2p-vpn/control.sock
cargo run -- daemon-status --socket /run/p2p-vpn/control.sock --format prometheus
cargo run -- daemon-state --socket /run/p2p-vpn/control.sock
cargo run -- daemon-peers --socket /run/p2p-vpn/control.sock
cargo run -- daemon-routes --socket /run/p2p-vpn/control.sock
cargo run -- daemon-paths --socket /run/p2p-vpn/control.sock
cargo run -- daemon-mtu --socket /run/p2p-vpn/control.sock
cargo run -- daemon-capabilities --socket /run/p2p-vpn/control.sock
cargo run -- daemon-health --socket /run/p2p-vpn/control.sock
cargo run -- daemon-health --socket /run/p2p-vpn/control.sock --wait-seconds 30 --require-validated-peers --require-supported-paths
cargo run -- daemon-shutdown --socket /run/p2p-vpn/control.sock
```

The control socket serves bounded `status`, `state`, `peers`, `routes`,
`paths`, `mtu`, `capabilities`, and `shutdown` requests. `daemon-health` queries
the state view and prints line-oriented readiness checks, returning nonzero when
any required check fails. By default it only requires a responsive running
daemon; add `--require-peers`, `--require-validated-peers`,
`--require-supported-paths`, `--require-packet-plane-listener`,
`--require-packet-plane-session`, `--require-packet-plane-quic-listener`, or
`--require-packet-plane-quic-session` to turn repro scripts into stricter stage
gates. Add `--wait-seconds N` to poll until those checks pass or the wait
deadline expires. `daemon-status` uses the same line-oriented metric names as
`metrics`, but comes from the running daemon's current queue and path state
instead of a startup snapshot.
`daemon-state` reports the running daemon's configured peers, validated
capability state, selected path, healthy direct and relay path counts, effective
MTU, selected path MTU, per-candidate path MTU estimates, configured
packet-plane session TTL and replay-window limit, and path-probe, DCUtR,
AutoNAT, relay-only infrastructure peers admitted from public or shared
reachability discovery, and stream-fallback in-flight shard counters. The
narrower `daemon-peers`, `daemon-routes`, `daemon-paths`, `daemon-mtu`, and
`daemon-capabilities` commands expose those live daemon views directly for
scripts and operators that do not want to parse the full state dump.
`daemon-shutdown` asks the daemon to acknowledge the request, print the final
metrics snapshot, remove the control socket, and exit through the same orderly
shutdown path used for Ctrl-C and systemd SIGTERM.
NixOS instances enable this by default at
`/run/p2p-vpn-<instance>/control.sock` through a `0750` runtime directory; set
`services.p2p-vpn.instances.<name>.controlSocket = null` to disable it.
