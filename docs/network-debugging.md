# Network Debugging

Use this runbook when a namespace, relay, DCUtR, discovery, or packet-plane run
fails and you need artifacts that can be inspected after the command exits.

## Fast Local Checks

Before running slower namespace or public relay repros, run the reproducible
local feedback loop:

```sh
nix run .#check-fast
```

It uses the flake-provided Rust toolchain and native build inputs to run
formatting, tests, and clippy from a single command.

## Namespace E2E

Check host namespace/TUN prerequisites before running the slower ignored suite:

```sh
nix run .#namespace-preflight
```

Run the whole Linux namespace suite:

```sh
nix run .#tun-e2e -- -- --ignored --nocapture
```

Run the same suite with artifacts preserved by default:

```sh
nix run .#namespace-repro
```

Run a focused case:

```sh
nix run .#tun-e2e -- tun_namespace_relay_overlay_promotes_to_direct_path -- --ignored --exact --nocapture
nix run .#tun-e2e -- tun_namespace_ping_crosses_owned_quic_packet_plane -- --ignored --exact --nocapture
```

Preserve generated configs and node logs after successful runs:

```sh
P2P_VPN_TUN_E2E_KEEP_TEMP=1 nix run .#tun-e2e -- -- --ignored --nocapture
```

`tun-e2e` runs `namespace-preflight` by default. Set
`P2P_VPN_TUN_E2E_SKIP_PREFLIGHT=1` only when debugging the preflight itself or
when a controlled environment has already proved user namespaces, network
namespaces, veth setup, and `/dev/net/tun`.

The preserved directory is printed on stderr. It contains `node-a.log`,
`node-b.log`, and, for relay or bootstrap scenarios, `node-relay.log` or
`node-bootstrap.log`. Each namespace orchestrator also writes
`repro-commands.sh` and `repro-metadata.txt` at startup. The command script
replays the focused test with `P2P_VPN_TUN_E2E_KEEP_TEMP=1` and includes a
direct `unshare` invocation for the already-built test binary. The metadata
records the test name, artifact directory, current test binary, timeout knobs,
Git revision, dirty status, kernel, `unshare`, and `ip` versions.

To triage an existing preserved run, start by listing the directory and reading
the metadata:

```sh
find "$ARTIFACT_DIR" -maxdepth 1 -type f | sort
sed -n '1,220p' "$ARTIFACT_DIR/repro-metadata.txt"
```

Failed tests already include node logs, `ip addr`, and route-table output in
the assertion message.
Namespace peer nodes also expose per-role daemon control sockets in that
directory as `control-a.sock` and `control-b.sock`. The orchestrator uses those
sockets to wait for validated peers, supported paths, and packet-plane sessions
instead of relying on fixed post-start sleeps.
When readiness waits or ping assertions fail, the orchestrator also writes
best-effort daemon snapshots next to the logs: `daemon-status-ROLE.txt`,
`daemon-state-ROLE.txt`, `daemon-peers-ROLE.txt`, `daemon-routes-ROLE.txt`,
`daemon-paths-ROLE.txt`, `daemon-mtu-ROLE.txt`, and
`daemon-capabilities-ROLE.txt`. These files capture stdout, stderr, and command
status for each reachable control socket so path selection, MTU, route, and
capability state can be compared after the failed run exits. For the daemon
view commands, the orchestrator also writes machine-readable
`daemon-state-ROLE.json`, `daemon-peers-ROLE.json`, `daemon-routes-ROLE.json`,
`daemon-paths-ROLE.json`, `daemon-mtu-ROLE.json`, and
`daemon-capabilities-ROLE.json` artifacts with a stable schema version, view
name, and line array.

On slower or heavily loaded hosts, keep the default timings for comparable
results and scale them only when diagnosing infrastructure latency:

```sh
P2P_VPN_TUN_E2E_WAIT_SCALE=2 nix run .#tun-e2e -- tun_namespace_relay_overlay_promotes_to_direct_path -- --ignored --exact --nocapture
P2P_VPN_TUN_E2E_ORCHESTRATOR_TIMEOUT_SECONDS=240 nix run .#tun-e2e -- tun_namespace_relay_overlay_promotes_to_direct_path -- --ignored --exact --nocapture
```

`P2P_VPN_TUN_E2E_WAIT_SCALE` multiplies namespace readiness, control-socket,
snapshot, and helper-command waits. `P2P_VPN_TUN_E2E_ORCHESTRATOR_TIMEOUT_SECONDS`
overrides only the outer `unshare` wrapper timeout.

## What To Inspect

When a daemon is still running with a control socket, capture a durable dump
before restarting it:

```sh
dump_dir="/tmp/p2p-vpn-daemon-dump-$(date -u +%Y%m%dT%H%M%SZ)"
p2p-vpn daemon-dump --socket /run/p2p-vpn/control.sock --output-dir "$dump_dir"
jq . "$dump_dir/summary.json"
```

`daemon-dump` writes `status.txt`, `status.prometheus`, text and JSON files for
state, peers, routes, paths, MTU, and capabilities, plus `summary.json` with
per-view success and artifact paths. It returns non-zero when one or more views
cannot be captured, but keeps any partial files for triage. Use `--force` only
when intentionally reusing a nonempty output directory.

Then check `p2p-vpn daemon-health --socket /run/p2p-vpn/control.sock`. Add
`--require-validated-peers`, `--require-supported-paths`,
`--require-packet-plane-session`, or `--require-packet-plane-quic-session` to
turn a repro into a strict readiness gate for the stage under investigation.
Use `--wait-seconds 30` when the repro should wait for asynchronous discovery,
relay, DCUtR, or packet-plane negotiation instead of sampling only once.

For discovery failures, check node logs for `kademlia query progressed`,
`discovered_address_dial_attempts`, and `control capabilities accepted`.

For relay failures, check relay logs for `CircuitReqAccepted` and peer logs for
relay readiness metrics and `relay_reservations_lost`. The captured
`daemon-state` and `daemon-status` artifacts include `auto_relay_policy_*`,
`auto_relay_current_candidates`, `auto_relay_active_reservations`, and
`auto_relay_pending_retries` lines, which distinguish policy caps from an empty
or retry-delayed candidate set.

For DCUtR or path-promotion failures, check peer logs for `event=dcutr_enabled`,
`event=autonat_enabled`, `event=dcutr_hole_punch_result`, and
`event=path_promoted_to_direct`.

For packet-plane failures, check for `event=packet_plane_session_established`,
`backend=owned_quic` when testing owned QUIC, positive
`path_healthy_direct_quic_datagram_paths`, and positive packet counters such as
`outbound_quic_datagram_packets` or `inbound_accepted_packets`. Use
`p2p-vpn daemon-paths --socket ... --format json` or
`p2p-vpn daemon-state --socket ... --format json` to
compare `observed_rtt_ms` across healthy path candidates after packet-plane
path probes have started flowing. From another configured peer, use
`p2p-vpn paths --config ... --live` to compare the remote daemon's reported
selected path, score, MTU, and RTT without direct access to its control socket.

## Membership Record Repro

Use the local helper when debugging the membership-record CLI flow without
starting daemons or requiring TUN privileges:

```sh
nix run .#membership-record-repro
```

The flake app runs the helper with the packaged `p2p-vpn` binary so the repro
does not rebuild through `cargo run` on every iteration. The helper creates
disposable issuer and member configs, exports the member's public identity,
issues a signed record with a route grant, verifies it against the selected
network, installs it into a derived issuer config, and preserves the generated
JSON, `repro-metadata.txt`, `repro-summary.txt`, `repro-summary.json`, and
replay commands in one artifact directory. Set
`P2P_VPN_MEMBERSHIP_REPRO_DIR` to choose that directory,
`P2P_VPN_MEMBERSHIP_REPRO_NETWORK` to change the network name,
`P2P_VPN_MEMBERSHIP_REPRO_ROUTE_GRANT` to change the granted route, and
`P2P_VPN_MEMBERSHIP_REPRO_EXPIRES_AT_UNIX_SECONDS` to exercise expiry metadata.
Run `scripts/membership-record-repro.sh` directly when you want the script's
default `nix develop -c cargo run --quiet --` path, or set
`P2P_VPN_BIN=/path/to/p2p-vpn` to reproduce with a specific built binary.

Use `p2p-vpn membership-record-list --config CONFIG` on preserved configs to
audit configured trust roots, active grants, revocation tombstones, effective
overlay members, and record-derived route grants without hand-reading
`network.member_records` JSON.

When a config has `network.member_records` and Kademlia enabled, use
`bootstrap-check` to collect rootless DHT propagation evidence before starting
the TUN daemon:

```sh
p2p-vpn bootstrap-check \
  --config CONFIG \
  --timeout-seconds 60 \
  --require-membership-records \
  --write-report membership-dht-bootstrap-check.json \
  --force
```

The text and JSON report include configured record count, DHT publication
start/success/failure state, lookup start state, found bundle count, verified
record count, accepted record count, invalid bundle count, and the last lookup
or validation error.
A returned bundle still has to pass local network-name, membership-scope,
signature, and trusted-issuer validation; public Kademlia peers only carry the
value and do not become membership authority.

## Public Relay Smoke

Use the packaged repro when the goal is to compare public relay, DCUtR, and
bootstrap behaviour across hosts or NATs. It keeps every phase in one artifact
directory, writes the exact replay commands, and exits nonzero when a later
phase fails without deleting earlier evidence.

### Live Public Relay/DCUtR Checklist

1. Pick a stable artifact directory per host and run the full public repro:

   ```sh
   export P2P_VPN_REPRO_DIR="/tmp/p2p-vpn-public-relay-$(hostname)-$(date -u +%Y%m%dT%H%M%SZ)"
   export P2P_VPN_RELAY_SCAN_TIMEOUT_SECONDS=30
   export P2P_VPN_RELAY_CANDIDATE_TIMEOUT_SECONDS=45
   export P2P_VPN_RELAY_MAX_CANDIDATES=8
   export P2P_VPN_RELAY_MAX_VALIDATION_CANDIDATES=8
   nix run .#public-relay-repro
   ```

   Add `P2P_VPN_REPRO_BASE_CONFIG=/path/to/p2p-vpn.json` when the generated
   relay-assisted config must preserve an existing overlay identity,
   membership, routes, peers, and packet-plane settings.
   The repro also runs a membership-record DHT bootstrap check by default and
   writes `public-membership-dht-bootstrap-check.json`; set
   `P2P_VPN_REPRO_MEMBERSHIP_DHT=0` when replaying relay-only failures.

2. Start triage from the preserved summary:

   ```sh
   sed -n '1,220p' "$P2P_VPN_REPRO_DIR/repro-summary.txt"
   jq . "$P2P_VPN_REPRO_DIR/repro-summary.json"
   sed -n '1,160p' "$P2P_VPN_REPRO_DIR/repro-host-network.txt"
   ```

   Compare `phase results`, `failure_stages`, `direct_connection_addresses`,
   `relayed_connection_addresses`, and `first_error` between hosts. A useful
   relay fallback proof has a successful relay-check phase and relayed
   connection addresses. A public DCUtR proof also needs a successful DCUtR
   phase with a direct non-relayed connection address. `repro-summary.json`
   provides a stable top-level index for comparing phase status, candidate
   counts, failure stages, elapsed ranges, route availability, relay
   reservation and relayed-circuit diagnostic counts, and handoff scripts across
   machines. When DCUtR does not succeed, compare each relay-check/DCUtR
   candidate's `bootstrap.peer_results`, `bootstrap.relay_results`, and
   `bootstrap.relayed_peer_results` arrays before changing relay candidates or
   timeout budgets. Compare
   `reports.membership_dht.membership_records.publish_succeeded`,
   `found_records`, `verified_records`, `accepted_records`, `invalid_records`,
   and `last_error` to separate public Kademlia reachability problems from
   signature or trust-root validation problems.
   For wall-clock comparisons, inspect `repro-phases.tsv`; it records each
   phase status with UTC start/end times and elapsed seconds, which is easier
   to diff across repeated public relay or two-host NAT runs than console logs.

3. Replay the same candidate set without public discovery when iterating:

   ```sh
   export P2P_VPN_REPRO_DIR="/tmp/p2p-vpn-public-relay-replay-$(hostname)-$(date -u +%Y%m%dT%H%M%SZ)"
   export P2P_VPN_REPRO_CANDIDATES_FILE=/tmp/previous-public-relay-candidates.txt
   export P2P_VPN_RELAY_CANDIDATE_TIMEOUT_SECONDS=60
   export P2P_VPN_RELAY_MAX_VALIDATION_CANDIDATES=8
   nix run .#public-relay-repro
   ```

   The previous candidate file can be
   `public-relay-candidates.txt` from another repro directory. This keeps
   repeated NAT/DCUtR runs comparable because only the local topology and
   timeout budget changed.
   Every run also writes `repro-retry-env.sh`; source it before the next
   `nix run .#public-relay-repro` to reuse the preserved candidate file and
   timeout knobs. Override `P2P_VPN_REPRO_DIR` after sourcing when the retry
   should write into a fresh artifact directory.
   For the fastest single-relay retry, set
   `P2P_VPN_REPRO_RELAY_CANDIDATE` to a direct `/p2p/RELAY` multiaddr; the
   repro writes that one candidate into the artifact directory and skips public
   discovery.

4. For a two-host public hole-punch proof, run the generated Host A listener
   while Host B dials the descriptor:

   ```sh
   "$P2P_VPN_REPRO_DIR/repro-dcutr-listen-host-a.sh"
   sed -n '1,220p' "$P2P_VPN_REPRO_DIR/public-relay-dcutr-listen-report.json"
   ```

   Copy `public-dcutr-listener.json` from Host A to the same path inside Host
   B's repro directory, then run:

   ```sh
   "$P2P_VPN_REPRO_DIR/repro-dcutr-dial-host-b.sh"
   sed -n '1,220p' "$P2P_VPN_REPRO_DIR/public-relay-dcutr-dial-report.json"
   ```

   If the generated listener script does not contain a selected relay, set
   `P2P_VPN_REPRO_RELAY_CANDIDATE` to a direct `/p2p/RELAY` multiaddr and
   rerun `nix run .#public-relay-repro` or the generated listener script.
   Compare Host A's `connected_to_relay`, `reservation_accepted`, and
   `relayed_listen_address_observed` fields with Host B's relayed-circuit and
   DCUtR fields before increasing timeouts; mismatches usually identify which
   side failed before the actual hole punch.

5. When a relay-assisted config is produced, inspect a live daemon through the
   control socket instead of scraping logs:

   ```sh
   p2p-vpn daemon-health \
     --socket /run/p2p-vpn/control.sock \
     --wait-seconds 30 \
     --require-validated-peers \
     --require-supported-paths

   p2p-vpn daemon-state --socket /run/p2p-vpn/control.sock
   p2p-vpn daemon-paths --socket /run/p2p-vpn/control.sock
   ```

   For owned packet-plane diagnostics, add
   `--require-packet-plane-session` or
   `--require-packet-plane-quic-session` to `daemon-health`.

### Two-Host Public VPN Data-Plane Repro

After `public-relay-repro` writes `public-relay-config.json`, generate the
two-host daemon runbook:

```sh
export P2P_VPN_VPN_REPRO_DIR="/tmp/p2p-vpn-public-vpn-$(hostname)-$(date -u +%Y%m%dT%H%M%SZ)"
export P2P_VPN_VPN_REPRO_PUBLIC_RELAY_DIR="$P2P_VPN_REPRO_DIR"
export P2P_VPN_VPN_REPRO_PING_TARGET=10.42.0.2
nix run .#public-vpn-repro
```

Use `P2P_VPN_VPN_REPRO_CONFIG=/path/to/p2p-vpn.json` instead of
`P2P_VPN_VPN_REPRO_PUBLIC_RELAY_DIR` when both hosts already have their final
overlay configs. Set `P2P_VPN_VPN_REPRO_REQUIRE_PACKET_SESSION=0` when the run
is intentionally proving stream fallback over a relayed circuit. Set
`P2P_VPN_VPN_REPRO_REQUIRE_QUIC_SESSION=1` when the expected result is an owned
QUIC packet-plane session.

The app writes `vpn-repro-host-a.sh`, `vpn-repro-host-b.sh`,
`vpn-repro-collect.sh`, `vpn-repro-shutdown.sh`, `vpn-repro-metadata.txt`,
`vpn-repro-host-network.txt`, `vpn-repro-summary.txt`, and
`vpn-repro-evidence.json`. Run the generated Host A and Host B scripts with the
privileges needed to create the TUN device.
The metadata records source revision and dirty status when the app is run from
a Git checkout, so artifact bundles can be compared against the exact code that
produced them.
Each host script records before/after host-network snapshots, preserves a
daemon log tail, records command exit statuses, starts `p2p-vpn up`, waits on
`daemon-health`, captures
`daemon-state`, `daemon-peers`, `daemon-routes`, `daemon-paths`, `daemon-mtu`,
`daemon-capabilities`, line-oriented metrics, Prometheus metrics, matching JSON
view envelopes, and final post-ping status/path snapshots even when health or
ping fails. It then pings `P2P_VPN_VPN_REPRO_PING_TARGET` when it is set. The
evidence JSON condenses health readiness, ping status, direct and relay path
lines, packet-plane counts, QUIC packet-plane counts, DCUtR successes, and
direct promotion counters so failed two-host runs can be compared without
hand-parsing every log. The artifact names are stable so two hosts can exchange
directories and compare the selected path,
packet-plane session state, drops, queue state, route ownership, MTU ceilings,
capability negotiation, host topology, and ping result directly.

Scan public IPFS-compatible bootstrap peers and write candidates:

```sh
p2p-vpn relay-scan \
  --ipfs-bootstrap-peers \
  --check-candidates \
  --write-candidates /tmp/p2p-vpn-relays.txt \
  --write-report /tmp/p2p-vpn-relay-scan-report.json
```

Run the packaged repro bundle:

```sh
nix run .#public-relay-repro
```

The app writes artifacts to a temporary directory and prints that path. Set
`P2P_VPN_REPRO_DIR` to choose the directory. Tune long-running network probes
with `P2P_VPN_RELAY_SCAN_TIMEOUT_SECONDS`,
`P2P_VPN_RELAY_CANDIDATE_TIMEOUT_SECONDS`, `P2P_VPN_RELAY_MAX_CANDIDATES`, and
`P2P_VPN_RELAY_MAX_VALIDATION_CANDIDATES`. Set
`P2P_VPN_REPRO_BASE_CONFIG` to an existing overlay config when a successful
relay-circuit phase should preserve that identity, membership, routes, peers,
and packet-plane settings in the generated config. Set
`P2P_VPN_REPRO_CANDIDATES_FILE` to a nonempty newline-separated candidate file
from an earlier `relay-scan --write-candidates` or `public-relay-repro` run to
skip public discovery and replay relay-circuit/DCUtR validation immediately
against the same candidates. The repro runs discovery, relay-circuit
validation, and DCUtR validation as separate phases and preserves
`public-relay-scan-report.json`, `public-relay-check-report.json`, and
`public-relay-dcutr-report.json` when those phases can run. If relay-circuit
validation succeeds, it also writes `public-relay-config.json`, a runnable
relay-assisted config generated from the validated public relay. Without
`P2P_VPN_REPRO_BASE_CONFIG`, that config uses the public IPFS profile plus the
validated relay shortcut; with a base config, it preserves the supplied overlay
and adds only relay infrastructure. It exits nonzero if any phase fails, but
earlier artifacts remain in the repro directory.
Probe reports use schema version 3 and include per-candidate
`elapsed_millis`, plus bootstrap-level `direct_connection_addresses` and
`relayed_connection_addresses` when a candidate reaches the bootstrap-check
phase. These fields help distinguish fast setup failures from full-budget
reservation, relayed-circuit, or DCUtR timeouts, and confirm which concrete path
type was observed.

Every repro directory also contains `repro-metadata.txt`,
`repro-host-network.txt`, `repro-commands.sh`,
`repro-dcutr-listen-host-a.sh`, `repro-dcutr-dial-host-b.sh`, and
`repro-summary.txt`. The metadata records source revision and dirty status when
the app is run from a Git checkout. The Host A/Host B scripts are generated
from the first successful relay-check candidate, or from
`P2P_VPN_REPRO_RELAY_CANDIDATE` when that override is set. Use
`P2P_VPN_REPRO_DCUTR_SERVE_SECONDS` and
`P2P_VPN_REPRO_DCUTR_DIAL_TIMEOUT_SECONDS` to tune the generated listener and
dialer durations. Start
with the summary when triaging a failure: it records each phase status,
phase duration, candidate counts, skipped candidates, routing-peer counts,
failure-stage counts, candidate elapsed-time ranges, and the first candidate
error from each report. For relay-check reports, it also summarizes observed
relayed and direct connection addresses so DCUtR runs show whether they reached
relay fallback only or completed direct promotion. Use the metadata file to
compare the exact timeout/candidate-limit environment between runs. Use the
host-network file to compare OS/kernel, IPv4/IPv6 route availability, interface
addresses, and route tables across machines. Use the commands file to replay
the same scan, relay-circuit, and DCUtR probes against the preserved candidate
file.

Probe candidates and write a machine-readable report:

```sh
p2p-vpn relay-check \
  --config p2p-vpn.json \
  --relay-candidates-file /tmp/p2p-vpn-relays.txt \
  --write-report /tmp/p2p-vpn-relay-report.json \
  --write-config /tmp/p2p-vpn-public-relay-config.json
```

When debugging hole punching, require DCUtR evidence so the command fails at
the relevant stage:

```sh
p2p-vpn relay-check \
  --relay-candidates-file /tmp/p2p-vpn-relays.txt \
  --require-dcutr-success \
  --write-report /tmp/p2p-vpn-dcutr-report.json
```

For a real public hole-punch proof, split the probe across two hosts instead of
running both temporary nodes on one machine. On Host A, reserve the public relay
and keep the listener running:

```sh
p2p-vpn relay-dcutr-listen \
  --relay-candidate /dns4/relay.example.net/tcp/4001/p2p/RELAY \
  --write-descriptor /tmp/p2p-vpn-dcutr-listener.json \
  --serve-seconds 900 \
  --force
```

Move `/tmp/p2p-vpn-dcutr-listener.json` to Host B while Host A is still
serving, then dial it from Host B:

```sh
p2p-vpn relay-dcutr-dial \
  --descriptor /tmp/p2p-vpn-dcutr-listener.json \
  --timeout-seconds 90 \
  --write-report /tmp/p2p-vpn-dcutr-dial-report.json \
  --force
```

The listener descriptor is a small JSON handoff containing the selected relay,
relay peer ID, listener peer ID, relayed target address, and listener bind
addresses. The dial report includes the descriptor plus the same bootstrap
report fields as `relay-check --require-dcutr-success`, including direct and
relayed connection address lists.

Inspect `repro-summary.txt` first, then the scan report peer counts, routing
peer counts, candidate addresses, candidate peer results, `failure_stage`,
bootstrap details, relay readiness, DCUtR counters, direct and relayed
connection address lists, and `last_error` fields.
