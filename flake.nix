{
  description = "Rust foundation for a libp2p-native packet-oriented mesh VPN";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
    in
    {
      nixosModules.default = import ./nix/nixos-module.nix { inherit self; };

      templates.nixos-mesh = {
        path = ./examples/nixos-mesh;
        description = "Two-node NixOS deployment skeleton for p2p-vpn";
      };
    }
    // flake-utils.lib.eachSystem supportedSystems (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        lib = nixpkgs.lib;
        rust = pkgs.rustc;
        cargo = pkgs.cargo;
        package = pkgs.rustPlatform.buildRustPackage {
          pname = "p2p-vpn";
          version = "0.1.0";
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [ pkgs.pkg-config ];
        };
        checkFast = pkgs.writeShellApplication {
          name = "p2p-vpn-check-fast";
          runtimeInputs = [
            cargo
            rust
            pkgs.clippy
            pkgs.rustfmt
            pkgs.pkg-config
            pkgs.stdenv.cc
          ];
          text = ''
            if [[ ! -f Cargo.toml || ! -d src ]]; then
              echo "p2p-vpn-check-fast must be run from the p2p-vpn repository root" >&2
              exit 2
            fi

            export RUST_BACKTRACE="''${RUST_BACKTRACE:-1}"
            cargo fmt -- --check
            cargo test
            cargo clippy --all-targets -- -D warnings
          '';
        };
        namespacePreflight = pkgs.writeShellApplication {
          name = "p2p-vpn-namespace-preflight";
          runtimeInputs = [
            pkgs.bash
            pkgs.coreutils
            pkgs.iproute2
            pkgs.util-linux
          ];
          text = ''
            if [[ "$(uname -s)" != Linux ]]; then
              echo "namespace preflight requires Linux" >&2
              exit 2
            fi

            if [[ ! -c /dev/net/tun ]]; then
              echo "missing /dev/net/tun; load the tun module and expose the device" >&2
              exit 2
            fi

            unshare --user --map-root-user --mount --net -- bash -euo pipefail -c '
              ip link add p2p-vpn-pre0 type veth peer name p2p-vpn-pre1
              ip link set p2p-vpn-pre0 up
              ip tuntap add dev p2p-vpn-pre-tun mode tun
              ip link set p2p-vpn-pre-tun up
              ip link delete p2p-vpn-pre0
              ip link delete p2p-vpn-pre-tun
            ' >/dev/null

            echo "namespace preflight ok: user namespace, network namespace, veth, and TUN creation work"
          '';
        };
        tunE2e = pkgs.writeShellApplication {
          name = "p2p-vpn-tun-e2e";
          runtimeInputs = [
            cargo
            rust
            namespacePreflight
            pkgs.iproute2
            pkgs.iputils
            pkgs.pkg-config
            pkgs.procps
            pkgs.stdenv.cc
            pkgs.util-linux
          ];
          text = ''
            if [[ ! -f Cargo.toml || ! -d tests ]]; then
              echo "p2p-vpn-tun-e2e must be run from the p2p-vpn repository root" >&2
              exit 2
            fi

            if [[ "''${P2P_VPN_TUN_E2E_SKIP_PREFLIGHT:-0}" != 1 ]]; then
              p2p-vpn-namespace-preflight
            fi

            export RUST_BACKTRACE=1
            if [[ "$#" -eq 0 ]]; then
              exec cargo test --test tun_namespace -- --ignored --nocapture
            fi

            exec cargo test --test tun_namespace "$@"
          '';
        };
        namespaceRepro = pkgs.writeShellApplication {
          name = "p2p-vpn-namespace-repro";
          runtimeInputs = [ tunE2e ];
          text = ''
            export P2P_VPN_TUN_E2E_KEEP_TEMP="''${P2P_VPN_TUN_E2E_KEEP_TEMP:-1}"
            export RUST_BACKTRACE=1
            exec p2p-vpn-tun-e2e "$@"
          '';
        };
        publicRelayRepro = pkgs.writeShellApplication {
          name = "p2p-vpn-public-relay-repro";
          runtimeInputs = [
            package
            pkgs.coreutils
            pkgs.iproute2
            pkgs.jq
          ];
          text = ''
            artifact_dir="''${P2P_VPN_REPRO_DIR:-}"
            if [[ -z "$artifact_dir" ]]; then
              artifact_dir="$(mktemp -d -t p2p-vpn-public-relay-repro.XXXXXXXX)"
            fi
            mkdir -p "$artifact_dir"

            candidates="$artifact_dir/public-relay-candidates.txt"
            scan_report="$artifact_dir/public-relay-scan-report.json"
            relay_report="$artifact_dir/public-relay-check-report.json"
            relay_config="$artifact_dir/public-relay-config.json"
            dcutr_report="$artifact_dir/public-relay-dcutr-report.json"
            dcutr_listener_descriptor="$artifact_dir/public-dcutr-listener.json"
            dcutr_listen_report="$artifact_dir/public-relay-dcutr-listen-report.json"
            dcutr_dial_report="$artifact_dir/public-dcutr-dial-report.json"
            metadata="$artifact_dir/repro-metadata.txt"
            host_network="$artifact_dir/repro-host-network.txt"
            commands="$artifact_dir/repro-commands.sh"
            dcutr_listen_script="$artifact_dir/repro-dcutr-listen-host-a.sh"
            dcutr_dial_script="$artifact_dir/repro-dcutr-dial-host-b.sh"
            summary="$artifact_dir/repro-summary.txt"
            scan_timeout="''${P2P_VPN_RELAY_SCAN_TIMEOUT_SECONDS:-30}"
            candidate_timeout="''${P2P_VPN_RELAY_CANDIDATE_TIMEOUT_SECONDS:-45}"
            max_candidates="''${P2P_VPN_RELAY_MAX_CANDIDATES:-8}"
            max_validation="''${P2P_VPN_RELAY_MAX_VALIDATION_CANDIDATES:-8}"
            base_config="''${P2P_VPN_REPRO_BASE_CONFIG:-}"
            repro_candidates_file="''${P2P_VPN_REPRO_CANDIDATES_FILE:-}"
            repro_relay_candidate="''${P2P_VPN_REPRO_RELAY_CANDIDATE:-}"
            dcutr_serve_seconds="''${P2P_VPN_REPRO_DCUTR_SERVE_SECONDS:-900}"
            dcutr_dial_timeout="''${P2P_VPN_REPRO_DCUTR_DIAL_TIMEOUT_SECONDS:-90}"
            relay_check_base_args=()
            if [[ -n "$base_config" ]]; then
              relay_check_base_args=(--config "$base_config")
            fi
            status=0
            phase_results=()

            write_metadata() {
              {
                echo "started_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
                echo "working_directory=$(pwd)"
                echo "system=$(uname -a)"
                echo "p2p_vpn_binary=$(command -v p2p-vpn)"
                echo "p2p_vpn_version=$(p2p-vpn --version 2>/dev/null || echo unknown)"
                echo "P2P_VPN_REPRO_DIR=$artifact_dir"
                echo "P2P_VPN_REPRO_BASE_CONFIG=$base_config"
                echo "P2P_VPN_REPRO_CANDIDATES_FILE=$repro_candidates_file"
                echo "P2P_VPN_REPRO_RELAY_CANDIDATE=$repro_relay_candidate"
                echo "P2P_VPN_REPRO_DCUTR_SERVE_SECONDS=$dcutr_serve_seconds"
                echo "P2P_VPN_REPRO_DCUTR_DIAL_TIMEOUT_SECONDS=$dcutr_dial_timeout"
                echo "P2P_VPN_RELAY_SCAN_TIMEOUT_SECONDS=$scan_timeout"
                echo "P2P_VPN_RELAY_CANDIDATE_TIMEOUT_SECONDS=$candidate_timeout"
                echo "P2P_VPN_RELAY_MAX_CANDIDATES=$max_candidates"
                echo "P2P_VPN_RELAY_MAX_VALIDATION_CANDIDATES=$max_validation"
              } > "$metadata"
            }

            route_available() {
              family="$1"
              target="$2"
              if ip "$family" route get "$target" >/dev/null 2>&1; then
                echo yes
              else
                echo no
              fi
            }

            write_host_network() {
              os_pretty_name=unknown
              if [[ -r /etc/os-release ]]; then
                # shellcheck disable=SC1091
                . /etc/os-release
                os_pretty_name="''${PRETTY_NAME:-unknown}"
              fi

              {
                echo "captured_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
                echo "os_pretty_name=$os_pretty_name"
                echo "kernel_name=$(uname -s)"
                echo "kernel_release=$(uname -r)"
                echo "machine=$(uname -m)"
                echo "ipv4_route_to_1_1_1_1=$(route_available -4 1.1.1.1)"
                echo "ipv6_route_to_2606_4700_4700_1111=$(route_available -6 2606:4700:4700::1111)"
                echo
                echo "[ip -br addr]"
                ip -br addr || true
                echo
                echo "[ip -d link show]"
                ip -d link show || true
                echo
                echo "[ip route show]"
                ip route show || true
                echo
                echo "[ip -6 route show]"
                ip -6 route show || true
                echo
                echo "[ss -lunpt]"
                ss -lunpt || true
                echo
                echo "[ps -o pid,ppid,stat,comm,args -C p2p-vpn]"
                ps -o pid,ppid,stat,comm,args -C p2p-vpn || true
              } > "$host_network"
            }

            write_commands() {
              {
                echo "#!/usr/bin/env bash"
                echo "set -euo pipefail"
                printf "export P2P_VPN_REPRO_DIR=%q\n" "$artifact_dir"
                if [[ -n "$base_config" ]]; then
                  printf "export P2P_VPN_REPRO_BASE_CONFIG=%q\n" "$base_config"
                fi
                if [[ -n "$repro_candidates_file" ]]; then
                  printf "export P2P_VPN_REPRO_CANDIDATES_FILE=%q\n" "$repro_candidates_file"
                fi
                if [[ -n "$repro_relay_candidate" ]]; then
                  printf "export P2P_VPN_REPRO_RELAY_CANDIDATE=%q\n" "$repro_relay_candidate"
                fi
                printf "export P2P_VPN_REPRO_DCUTR_SERVE_SECONDS=%q\n" "$dcutr_serve_seconds"
                printf "export P2P_VPN_REPRO_DCUTR_DIAL_TIMEOUT_SECONDS=%q\n" "$dcutr_dial_timeout"
                printf "export P2P_VPN_RELAY_SCAN_TIMEOUT_SECONDS=%q\n" "$scan_timeout"
                printf "export P2P_VPN_RELAY_CANDIDATE_TIMEOUT_SECONDS=%q\n" "$candidate_timeout"
                printf "export P2P_VPN_RELAY_MAX_CANDIDATES=%q\n" "$max_candidates"
                printf "export P2P_VPN_RELAY_MAX_VALIDATION_CANDIDATES=%q\n" "$max_validation"
                echo
                if [[ -n "$repro_candidates_file" ]]; then
                  printf "cp %q %q\n" "$repro_candidates_file" "$candidates"
                  echo
                else
                  echo "p2p-vpn relay-scan \\"
                  echo "  --ipfs-bootstrap-peers \\"
                  printf "  --timeout-seconds %q \\\\\n" "$scan_timeout"
                  printf "  --max-candidates %q \\\\\n" "$max_candidates"
                  printf "  --write-candidates %q \\\\\n" "$candidates"
                  printf "  --write-report %q \\\\\n" "$scan_report"
                  echo "  --force"
                  echo
                fi
                echo "p2p-vpn relay-check \\"
                if [[ -n "$base_config" ]]; then
                  printf "  --config %q \\\\\n" "$base_config"
                fi
                printf "  --relay-candidates-file %q \\\\\n" "$candidates"
                printf "  --timeout-seconds %q \\\\\n" "$candidate_timeout"
                printf "  --max-validation-candidates %q \\\\\n" "$max_validation"
                printf "  --write-report %q \\\\\n" "$relay_report"
                printf "  --write-config %q \\\\\n" "$relay_config"
                echo "  --force"
                echo
                echo "p2p-vpn relay-check \\"
                if [[ -n "$base_config" ]]; then
                  printf "  --config %q \\\\\n" "$base_config"
                fi
                printf "  --relay-candidates-file %q \\\\\n" "$candidates"
                printf "  --timeout-seconds %q \\\\\n" "$candidate_timeout"
                printf "  --max-validation-candidates %q \\\\\n" "$max_validation"
                echo "  --require-dcutr-success \\"
                printf "  --write-report %q \\\\\n" "$dcutr_report"
                echo "  --force"
                echo
                printf "%q\n" "$dcutr_listen_script"
                printf "%q\n" "$dcutr_dial_script"
              } > "$commands"
              chmod +x "$commands"
            }

            selected_public_dcutr_candidate() {
              if [[ -n "$repro_relay_candidate" ]]; then
                printf "%s\n" "$repro_relay_candidate"
                return
              fi

              if [[ -s "$relay_report" ]]; then
                relay_candidate="$(jq -r '[.candidates[]? | select(.succeeded == true) | .address][0] // empty' "$relay_report")"
                if [[ -n "$relay_candidate" ]]; then
                  printf "%s\n" "$relay_candidate"
                  return
                fi
              fi

              if [[ -s "$dcutr_report" ]]; then
                jq -r '[.candidates[]? | select(.failure_stage == "dcutr_success") | .address][0] // empty' "$dcutr_report"
              fi
            }

            write_public_dcutr_handoff_scripts() {
              selected_relay_candidate="$(selected_public_dcutr_candidate)"
              {
                echo "#!/usr/bin/env bash"
                echo "set -euo pipefail"
                if [[ -n "$selected_relay_candidate" ]]; then
                  printf "relay_candidate=%q\n" "$selected_relay_candidate"
                else
                  printf '%s\n' "relay_candidate=\"\$""{P2P_VPN_REPRO_RELAY_CANDIDATE:?set P2P_VPN_REPRO_RELAY_CANDIDATE to a direct /p2p/RELAY multiaddr}\""
                fi
                printf "p2p-vpn relay-dcutr-listen \\\\\n"
                printf "  --relay-candidate \"\$relay_candidate\" \\\\\n"
                printf "  --write-descriptor %q \\\\\n" "$dcutr_listener_descriptor"
                printf "  --write-report %q \\\\\n" "$dcutr_listen_report"
                printf "  --serve-seconds %q \\\\\n" "$dcutr_serve_seconds"
                echo "  --force"
              } > "$dcutr_listen_script"

              {
                echo "#!/usr/bin/env bash"
                echo "set -euo pipefail"
                printf "p2p-vpn relay-dcutr-dial \\\\\n"
                printf "  --descriptor %q \\\\\n" "$dcutr_listener_descriptor"
                printf "  --timeout-seconds %q \\\\\n" "$dcutr_dial_timeout"
                printf "  --write-report %q \\\\\n" "$dcutr_dial_report"
                echo "  --force"
              } > "$dcutr_dial_script"

              chmod +x "$dcutr_listen_script" "$dcutr_dial_script"
            }

            append_report_summary() {
              label="$1"
              report="$2"
              if [[ ! -s "$report" ]]; then
                echo "$label report: missing" >> "$summary"
                return
              fi

              echo "$label report: $report" >> "$summary"
              jq -r '
                "  succeeded=" + ((.succeeded // false) | tostring),
                "  candidates=" + (((.candidates // []) | length) | tostring),
                "  skipped_candidates=" + (((.skipped_candidates // []) | length) | tostring),
                "  host_reachable_candidates=" + (((.host_reachable_candidates // []) | length) | tostring),
                "  discovered_routing_peers=" + ((.discovered_routing_peers // "n/a") | tostring),
                "  dialed_routing_peers=" + ((.dialed_routing_peers // "n/a") | tostring),
                "  closest_peer_results=" + ((.closest_peer_results // "n/a") | tostring),
                "  failure_stages=" + (
                  [(.candidates // [])[].failure_stage | select(. != null)]
                  | if length == 0 then "none"
                    else group_by(.) | map("\(.[0])=\(length)") | join(",")
                    end
                ),
                "  elapsed_millis=" + (
                  [(.candidates // [])[].elapsed_millis | select(. != null)]
                  | if length == 0 then "none"
                    else "min=\(min),max=\(max)"
                    end
                ),
                "  relayed_connection_addresses=" + (
                  [(.candidates // [])[].bootstrap.relayed_connection_addresses[]?]
                  | if length == 0 then "none"
                    else "count=\(length),first=\(.[0])"
                    end
                ),
                "  direct_connection_addresses=" + (
                  [(.candidates // [])[].bootstrap.direct_connection_addresses[]?]
                  | if length == 0 then "none"
                    else "count=\(length),first=\(.[0])"
                    end
                ),
                "  first_error=" + (
                  (
                    [(.candidates // [])[].error, (.peer_results // [])[].last_error]
                    | map(select(. != null and . != ""))
                    | first
                  ) // "none"
                )
              ' "$report" >> "$summary"
            }

            append_handoff_summary() {
              selected_relay_candidate="$(selected_public_dcutr_candidate)"
              {
                echo "two-host dcutr handoff:"
                echo "  selected_relay_candidate=''${selected_relay_candidate:-none}"
                echo "  listen_script=$dcutr_listen_script"
                echo "  dial_script=$dcutr_dial_script"
                echo "  listener_descriptor=$dcutr_listener_descriptor"
                echo "  dial_report=$dcutr_dial_report"
                echo "  serve_seconds=$dcutr_serve_seconds"
                echo "  dial_timeout_seconds=$dcutr_dial_timeout"
              } >> "$summary"
            }

            write_summary() {
              {
                echo "p2p-vpn public relay repro summary"
                echo "artifact_dir=$artifact_dir"
                echo "metadata=$metadata"
                echo "host_network=$host_network"
                echo "commands=$commands"
                echo "dcutr_listen_script=$dcutr_listen_script"
                echo "dcutr_dial_script=$dcutr_dial_script"
                echo "candidate_file=$candidates"
                echo "relay_assisted_config=$relay_config"
                echo
                echo "phase results:"
                if [[ "''${#phase_results[@]}" -eq 0 ]]; then
                  echo "  none"
                else
                  printf "  %s\n" "''${phase_results[@]}"
                fi
                echo
              } > "$summary"
              append_report_summary "scan" "$scan_report"
              append_report_summary "relay-check" "$relay_report"
              append_report_summary "dcutr" "$dcutr_report"
              append_handoff_summary
            }

            run_phase() {
              phase="$1"
              shift
              echo "$phase" >&2
              phase_started="$(date +%s)"
              set +e
              "$@"
              phase_status="$?"
              set -e
              phase_finished="$(date +%s)"
              phase_elapsed="$((phase_finished - phase_started))"
              if [[ "$phase_status" -ne 0 ]]; then
                echo "$phase failed with exit status $phase_status after ''${phase_elapsed}s" >&2
                status=1
              fi
              phase_results+=("$phase status=$phase_status elapsed_seconds=$phase_elapsed")
            }

            echo "writing public relay repro artifacts to $artifact_dir" >&2
            write_metadata
            write_host_network
            write_commands
            if [[ -n "$repro_candidates_file" ]]; then
              if [[ ! -s "$repro_candidates_file" ]]; then
                echo "P2P_VPN_REPRO_CANDIDATES_FILE must point to a nonempty relay candidate file" >&2
                exit 2
              fi
              cp "$repro_candidates_file" "$candidates"
              phase_results+=("using supplied public relay candidate file status=0 elapsed_seconds=0")
            else
              run_phase "scanning IPFS-compatible bootstrap peers for public relay candidates" \
                p2p-vpn relay-scan \
                --ipfs-bootstrap-peers \
                --timeout-seconds "$scan_timeout" \
                --max-candidates "$max_candidates" \
                --write-candidates "$candidates" \
                --write-report "$scan_report" \
                --force
            fi

            if [[ -s "$candidates" ]]; then
              run_phase "probing candidates for relay reservation and relayed-circuit evidence" \
                p2p-vpn relay-check \
                "''${relay_check_base_args[@]}" \
                --relay-candidates-file "$candidates" \
                --timeout-seconds "$candidate_timeout" \
                --max-validation-candidates "$max_validation" \
                --write-report "$relay_report" \
                --write-config "$relay_config" \
                --force

              run_phase "probing candidates for DCUtR success evidence" \
                p2p-vpn relay-check \
                "''${relay_check_base_args[@]}" \
                --relay-candidates-file "$candidates" \
                --timeout-seconds "$candidate_timeout" \
                --max-validation-candidates "$max_validation" \
                --require-dcutr-success \
                --write-report "$dcutr_report" \
                --force
            else
              echo "candidate file is empty; skipping relay-check probes" >&2
              status=1
            fi

            echo "candidate file: $candidates" >&2
            echo "scan report: $scan_report" >&2
            echo "relay-check report: $relay_report" >&2
            echo "relay-assisted config: $relay_config" >&2
            echo "DCUtR report: $dcutr_report" >&2
            write_public_dcutr_handoff_scripts
            write_summary
            echo "metadata: $metadata" >&2
            echo "host network: $host_network" >&2
            echo "replay commands: $commands" >&2
            echo "Host A DCUtR listener script: $dcutr_listen_script" >&2
            echo "Host B DCUtR dial script: $dcutr_dial_script" >&2
            echo "summary: $summary" >&2
            exit "$status"
          '';
        };
        publicVpnRepro = pkgs.writeShellApplication {
          name = "p2p-vpn-public-vpn-repro";
          runtimeInputs = [
            package
            pkgs.coreutils
            pkgs.iproute2
            pkgs.iputils
            pkgs.jq
            pkgs.procps
          ];
          text = ''
            umask 077

            artifact_dir="''${P2P_VPN_VPN_REPRO_DIR:-}"
            if [[ -z "$artifact_dir" ]]; then
              artifact_dir="$(mktemp -d -t p2p-vpn-public-vpn-repro.XXXXXXXX)"
            fi
            mkdir -p "$artifact_dir"

            config="''${P2P_VPN_VPN_REPRO_CONFIG:-}"
            public_relay_dir="''${P2P_VPN_VPN_REPRO_PUBLIC_RELAY_DIR:-}"
            if [[ -z "$config" && -n "$public_relay_dir" && -s "$public_relay_dir/public-relay-config.json" ]]; then
              config="$public_relay_dir/public-relay-config.json"
            fi
            if [[ -z "$config" ]]; then
              config="$artifact_dir/public-relay-config.json"
            fi
            if [[ ! -s "$config" ]]; then
              cat >&2 <<EOF
missing relay-assisted VPN config: $config

Set P2P_VPN_VPN_REPRO_CONFIG to an existing overlay config, or set
P2P_VPN_VPN_REPRO_PUBLIC_RELAY_DIR to a public-relay-repro artifact directory
containing public-relay-config.json.
EOF
              exit 2
            fi

            metadata="$artifact_dir/vpn-repro-metadata.txt"
            host_network="$artifact_dir/vpn-repro-host-network.txt"
            commands="$artifact_dir/vpn-repro-commands.sh"
            host_a_script="$artifact_dir/vpn-repro-host-a.sh"
            host_b_script="$artifact_dir/vpn-repro-host-b.sh"
            collect_script="$artifact_dir/vpn-repro-collect.sh"
            shutdown_script="$artifact_dir/vpn-repro-shutdown.sh"
            summary="$artifact_dir/vpn-repro-summary.txt"
            ping_target="''${P2P_VPN_VPN_REPRO_PING_TARGET:-}"
            ping_count="''${P2P_VPN_VPN_REPRO_PING_COUNT:-3}"
            ping_timeout="''${P2P_VPN_VPN_REPRO_PING_TIMEOUT_SECONDS:-2}"
            health_wait="''${P2P_VPN_VPN_REPRO_HEALTH_WAIT_SECONDS:-60}"
            metrics_interval="''${P2P_VPN_VPN_REPRO_METRICS_INTERVAL_SECONDS:-5}"
            control_socket="''${P2P_VPN_VPN_REPRO_CONTROL_SOCKET:-$artifact_dir/control.sock}"
            pidfile="''${P2P_VPN_VPN_REPRO_PIDFILE:-$artifact_dir/p2p-vpn.pid}"
            daemon_log="''${P2P_VPN_VPN_REPRO_DAEMON_LOG:-$artifact_dir/p2p-vpn-daemon.log}"
            health_log="$artifact_dir/daemon-health.txt"
            state_log="$artifact_dir/daemon-state.txt"
            state_json="$artifact_dir/daemon-state.json"
            peers_log="$artifact_dir/daemon-peers.txt"
            peers_json="$artifact_dir/daemon-peers.json"
            routes_log="$artifact_dir/daemon-routes.txt"
            routes_json="$artifact_dir/daemon-routes.json"
            paths_log="$artifact_dir/daemon-paths.txt"
            paths_json="$artifact_dir/daemon-paths.json"
            mtu_log="$artifact_dir/daemon-mtu.txt"
            mtu_json="$artifact_dir/daemon-mtu.json"
            capabilities_log="$artifact_dir/daemon-capabilities.txt"
            capabilities_json="$artifact_dir/daemon-capabilities.json"
            status_log="$artifact_dir/daemon-status.txt"
            prometheus_log="$artifact_dir/daemon-status-prometheus.txt"
            final_status_log="$artifact_dir/daemon-status-final.txt"
            final_prometheus_log="$artifact_dir/daemon-status-prometheus-final.txt"
            final_state_json="$artifact_dir/daemon-state-final.json"
            final_paths_json="$artifact_dir/daemon-paths-final.json"
            ping_log="$artifact_dir/ping.txt"
            require_packet_session="''${P2P_VPN_VPN_REPRO_REQUIRE_PACKET_SESSION:-1}"
            require_quic_session="''${P2P_VPN_VPN_REPRO_REQUIRE_QUIC_SESSION:-0}"

            route_available() {
              family="$1"
              target="$2"
              if ip "$family" route get "$target" >/dev/null 2>&1; then
                echo yes
              else
                echo no
              fi
            }

            write_host_network() {
              os_pretty_name=unknown
              if [[ -r /etc/os-release ]]; then
                # shellcheck disable=SC1091
                . /etc/os-release
                os_pretty_name="''${PRETTY_NAME:-unknown}"
              fi

              {
                echo "captured_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
                echo "os_pretty_name=$os_pretty_name"
                echo "kernel_name=$(uname -s)"
                echo "kernel_release=$(uname -r)"
                echo "machine=$(uname -m)"
                echo "ipv4_route_to_1_1_1_1=$(route_available -4 1.1.1.1)"
                echo "ipv6_route_to_2606_4700_4700_1111=$(route_available -6 2606:4700:4700::1111)"
                echo
                echo "[ip -br addr]"
                ip -br addr || true
                echo
                echo "[ip -d link show]"
                ip -d link show || true
                echo
                echo "[ip route show]"
                ip route show || true
                echo
                echo "[ip -6 route show]"
                ip -6 route show || true
                echo
                echo "[ss -lunpt]"
                ss -lunpt || true
                echo
                echo "[ps -o pid,ppid,stat,comm,args -C p2p-vpn]"
                ps -o pid,ppid,stat,comm,args -C p2p-vpn || true
              } > "$host_network"
            }

            write_metadata() {
              peer_count="$(jq '(.peers // []) | length' "$config" 2>/dev/null || echo unknown)"
              route_count="$(jq '(.network.routes // []) | length' "$config" 2>/dev/null || echo unknown)"
              interface_name="$(jq -r '.interface.name // "unknown"' "$config" 2>/dev/null || echo unknown)"
              interface_mtu="$(jq -r '.interface.mtu // "unknown"' "$config" 2>/dev/null || echo unknown)"
              {
                echo "started_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
                echo "working_directory=$(pwd)"
                echo "system=$(uname -a)"
                echo "p2p_vpn_binary=$(command -v p2p-vpn)"
                echo "p2p_vpn_version=$(p2p-vpn --version 2>/dev/null || echo unknown)"
                echo "artifact_dir=$artifact_dir"
                echo "config=$config"
                echo "public_relay_dir=$public_relay_dir"
                echo "peer_count=$peer_count"
                echo "route_count=$route_count"
                echo "interface_name=$interface_name"
                echo "interface_mtu=$interface_mtu"
                echo "control_socket=$control_socket"
                echo "pidfile=$pidfile"
                echo "daemon_log=$daemon_log"
                echo "ping_target=$ping_target"
                echo "ping_count=$ping_count"
                echo "ping_timeout_seconds=$ping_timeout"
                echo "health_wait_seconds=$health_wait"
                echo "metrics_interval_seconds=$metrics_interval"
                echo "require_packet_session=$require_packet_session"
                echo "require_quic_session=$require_quic_session"
              } > "$metadata"
            }

            write_runner() {
              script="$1"
              role="$2"
              {
                echo "#!/usr/bin/env bash"
                echo "set -euo pipefail"
                echo "umask 077"
                printf "artifact_dir=%q\n" "$artifact_dir"
                printf "config=%q\n" "$config"
                printf "control_socket=%q\n" "$control_socket"
                printf "pidfile=%q\n" "$pidfile"
                printf "daemon_log=%q\n" "$daemon_log"
                printf "health_log=%q\n" "$health_log"
                printf "state_log=%q\n" "$state_log"
                printf "state_json=%q\n" "$state_json"
                printf "peers_log=%q\n" "$peers_log"
                printf "peers_json=%q\n" "$peers_json"
                printf "routes_log=%q\n" "$routes_log"
                printf "routes_json=%q\n" "$routes_json"
                printf "paths_log=%q\n" "$paths_log"
                printf "paths_json=%q\n" "$paths_json"
                printf "mtu_log=%q\n" "$mtu_log"
                printf "mtu_json=%q\n" "$mtu_json"
                printf "capabilities_log=%q\n" "$capabilities_log"
                printf "capabilities_json=%q\n" "$capabilities_json"
                printf "status_log=%q\n" "$status_log"
                printf "prometheus_log=%q\n" "$prometheus_log"
                printf "final_status_log=%q\n" "$final_status_log"
                printf "final_prometheus_log=%q\n" "$final_prometheus_log"
                printf "final_state_json=%q\n" "$final_state_json"
                printf "final_paths_json=%q\n" "$final_paths_json"
                printf "ping_log=%q\n" "$ping_log"
                printf "ping_target=%q\n" "$ping_target"
                printf "ping_count=%q\n" "$ping_count"
                printf "ping_timeout=%q\n" "$ping_timeout"
                printf "health_wait=%q\n" "$health_wait"
                printf "metrics_interval=%q\n" "$metrics_interval"
                printf "require_packet_session=%q\n" "$require_packet_session"
                printf "require_quic_session=%q\n" "$require_quic_session"
                echo "mkdir -p \"\$artifact_dir\" \"$(dirname "$control_socket")\""
                echo "if [[ -e \"\$pidfile\" ]] && kill -0 \"\$(cat \"\$pidfile\")\" 2>/dev/null; then"
                echo "  echo \"p2p-vpn already running with pid \$(cat \"\$pidfile\")\" >&2"
                echo "else"
                echo "  rm -f \"\$control_socket\""
                echo "  p2p-vpn up --config \"\$config\" --metrics-interval-seconds \"\$metrics_interval\" --control-socket \"\$control_socket\" > \"\$daemon_log\" 2>&1 &"
                echo "  echo \"\$!\" > \"\$pidfile\""
                echo "fi"
                echo "health_args=(--socket \"\$control_socket\" --wait-seconds \"\$health_wait\" --require-validated-peers --require-supported-paths)"
                echo "if [[ \"\$require_packet_session\" == 1 ]]; then"
                echo "  health_args+=(--require-packet-plane-session)"
                echo "fi"
                echo "if [[ \"\$require_quic_session\" == 1 ]]; then"
                echo "  health_args+=(--require-packet-plane-quic-session)"
                echo "fi"
                echo "capture_daemon_views() {"
                echo "  p2p-vpn daemon-state --socket \"\$control_socket\" | tee \"\$state_log\""
                echo "  p2p-vpn daemon-state --socket \"\$control_socket\" --format json > \"\$state_json\""
                echo "  p2p-vpn daemon-peers --socket \"\$control_socket\" | tee \"\$peers_log\""
                echo "  p2p-vpn daemon-peers --socket \"\$control_socket\" --format json > \"\$peers_json\""
                echo "  p2p-vpn daemon-routes --socket \"\$control_socket\" | tee \"\$routes_log\""
                echo "  p2p-vpn daemon-routes --socket \"\$control_socket\" --format json > \"\$routes_json\""
                echo "  p2p-vpn daemon-paths --socket \"\$control_socket\" | tee \"\$paths_log\""
                echo "  p2p-vpn daemon-paths --socket \"\$control_socket\" --format json > \"\$paths_json\""
                echo "  p2p-vpn daemon-mtu --socket \"\$control_socket\" | tee \"\$mtu_log\""
                echo "  p2p-vpn daemon-mtu --socket \"\$control_socket\" --format json > \"\$mtu_json\""
                echo "  p2p-vpn daemon-capabilities --socket \"\$control_socket\" | tee \"\$capabilities_log\""
                echo "  p2p-vpn daemon-capabilities --socket \"\$control_socket\" --format json > \"\$capabilities_json\""
                echo "  p2p-vpn daemon-status --socket \"\$control_socket\" | tee \"\$status_log\""
                echo "  p2p-vpn daemon-status --socket \"\$control_socket\" --format prometheus | tee \"\$prometheus_log\""
                echo "}"
                # shellcheck disable=SC2016
                echo 'p2p-vpn daemon-health "''${health_args[@]}" | tee "$health_log"'
                echo "capture_daemon_views"
                echo "if [[ -n \"\$ping_target\" ]]; then"
                echo "  ping -c \"\$ping_count\" -W \"\$ping_timeout\" \"\$ping_target\" | tee \"\$ping_log\""
                echo "else"
                echo "  echo \"set P2P_VPN_VPN_REPRO_PING_TARGET to the remote tunnel address to prove data forwarding\" | tee \"\$ping_log\""
                echo "fi"
                echo "p2p-vpn daemon-status --socket \"\$control_socket\" | tee \"\$final_status_log\""
                echo "p2p-vpn daemon-status --socket \"\$control_socket\" --format prometheus | tee \"\$final_prometheus_log\""
                echo "p2p-vpn daemon-state --socket \"\$control_socket\" --format json > \"\$final_state_json\""
                echo "p2p-vpn daemon-paths --socket \"\$control_socket\" --format json > \"\$final_paths_json\""
                printf "echo %q\n" "$role complete; artifacts in $artifact_dir"
              } > "$script"
              chmod +x "$script"
            }

            write_collect() {
              {
                echo "#!/usr/bin/env bash"
                echo "set -euo pipefail"
                printf "control_socket=%q\n" "$control_socket"
                printf "health_log=%q\n" "$health_log"
                printf "state_log=%q\n" "$state_log"
                printf "state_json=%q\n" "$state_json"
                printf "peers_log=%q\n" "$peers_log"
                printf "peers_json=%q\n" "$peers_json"
                printf "routes_log=%q\n" "$routes_log"
                printf "routes_json=%q\n" "$routes_json"
                printf "paths_log=%q\n" "$paths_log"
                printf "paths_json=%q\n" "$paths_json"
                printf "mtu_log=%q\n" "$mtu_log"
                printf "mtu_json=%q\n" "$mtu_json"
                printf "capabilities_log=%q\n" "$capabilities_log"
                printf "capabilities_json=%q\n" "$capabilities_json"
                printf "status_log=%q\n" "$status_log"
                printf "prometheus_log=%q\n" "$prometheus_log"
                echo "p2p-vpn daemon-health --socket \"\$control_socket\" | tee \"\$health_log\""
                echo "p2p-vpn daemon-state --socket \"\$control_socket\" | tee \"\$state_log\""
                echo "p2p-vpn daemon-state --socket \"\$control_socket\" --format json > \"\$state_json\""
                echo "p2p-vpn daemon-peers --socket \"\$control_socket\" | tee \"\$peers_log\""
                echo "p2p-vpn daemon-peers --socket \"\$control_socket\" --format json > \"\$peers_json\""
                echo "p2p-vpn daemon-routes --socket \"\$control_socket\" | tee \"\$routes_log\""
                echo "p2p-vpn daemon-routes --socket \"\$control_socket\" --format json > \"\$routes_json\""
                echo "p2p-vpn daemon-paths --socket \"\$control_socket\" | tee \"\$paths_log\""
                echo "p2p-vpn daemon-paths --socket \"\$control_socket\" --format json > \"\$paths_json\""
                echo "p2p-vpn daemon-mtu --socket \"\$control_socket\" | tee \"\$mtu_log\""
                echo "p2p-vpn daemon-mtu --socket \"\$control_socket\" --format json > \"\$mtu_json\""
                echo "p2p-vpn daemon-capabilities --socket \"\$control_socket\" | tee \"\$capabilities_log\""
                echo "p2p-vpn daemon-capabilities --socket \"\$control_socket\" --format json > \"\$capabilities_json\""
                echo "p2p-vpn daemon-status --socket \"\$control_socket\" | tee \"\$status_log\""
                echo "p2p-vpn daemon-status --socket \"\$control_socket\" --format prometheus | tee \"\$prometheus_log\""
              } > "$collect_script"
              chmod +x "$collect_script"
            }

            write_shutdown() {
              {
                echo "#!/usr/bin/env bash"
                echo "set -euo pipefail"
                printf "control_socket=%q\n" "$control_socket"
                printf "pidfile=%q\n" "$pidfile"
                echo "if [[ -S \"\$control_socket\" ]]; then"
                echo "  p2p-vpn daemon-shutdown --socket \"\$control_socket\" || true"
                echo "fi"
                echo "if [[ -e \"\$pidfile\" ]] && kill -0 \"\$(cat \"\$pidfile\")\" 2>/dev/null; then"
                echo "  kill \"\$(cat \"\$pidfile\")\" || true"
                echo "fi"
              } > "$shutdown_script"
              chmod +x "$shutdown_script"
            }

            write_commands() {
              {
                echo "#!/usr/bin/env bash"
                echo "set -euo pipefail"
                printf "export P2P_VPN_VPN_REPRO_DIR=%q\n" "$artifact_dir"
                printf "export P2P_VPN_VPN_REPRO_CONFIG=%q\n" "$config"
                printf "export P2P_VPN_VPN_REPRO_CONTROL_SOCKET=%q\n" "$control_socket"
                printf "export P2P_VPN_VPN_REPRO_PIDFILE=%q\n" "$pidfile"
                printf "export P2P_VPN_VPN_REPRO_DAEMON_LOG=%q\n" "$daemon_log"
                printf "export P2P_VPN_VPN_REPRO_PING_TARGET=%q\n" "$ping_target"
                printf "export P2P_VPN_VPN_REPRO_PING_COUNT=%q\n" "$ping_count"
                printf "export P2P_VPN_VPN_REPRO_PING_TIMEOUT_SECONDS=%q\n" "$ping_timeout"
                printf "export P2P_VPN_VPN_REPRO_HEALTH_WAIT_SECONDS=%q\n" "$health_wait"
                printf "export P2P_VPN_VPN_REPRO_METRICS_INTERVAL_SECONDS=%q\n" "$metrics_interval"
                printf "export P2P_VPN_VPN_REPRO_REQUIRE_PACKET_SESSION=%q\n" "$require_packet_session"
                printf "export P2P_VPN_VPN_REPRO_REQUIRE_QUIC_SESSION=%q\n" "$require_quic_session"
                echo
                printf "nix run .#public-vpn-repro\n"
                printf "%q\n" "$host_a_script"
                printf "%q\n" "$host_b_script"
                printf "%q\n" "$collect_script"
                printf "%q\n" "$shutdown_script"
              } > "$commands"
              chmod +x "$commands"
            }

            write_summary() {
              {
                echo "p2p-vpn public VPN repro summary"
                echo "artifact_dir=$artifact_dir"
                echo "config=$config"
                echo "metadata=$metadata"
                echo "host_network=$host_network"
                echo "commands=$commands"
                echo "host_a_script=$host_a_script"
                echo "host_b_script=$host_b_script"
                echo "collect_script=$collect_script"
                echo "shutdown_script=$shutdown_script"
                echo "daemon_log=$daemon_log"
                echo "health_log=$health_log"
                echo "state_log=$state_log"
                echo "state_json=$state_json"
                echo "peers_log=$peers_log"
                echo "peers_json=$peers_json"
                echo "routes_log=$routes_log"
                echo "routes_json=$routes_json"
                echo "paths_log=$paths_log"
                echo "paths_json=$paths_json"
                echo "mtu_log=$mtu_log"
                echo "mtu_json=$mtu_json"
                echo "capabilities_log=$capabilities_log"
                echo "capabilities_json=$capabilities_json"
                echo "status_log=$status_log"
                echo "prometheus_log=$prometheus_log"
                echo "final_status_log=$final_status_log"
                echo "final_prometheus_log=$final_prometheus_log"
                echo "final_state_json=$final_state_json"
                echo "final_paths_json=$final_paths_json"
                echo "ping_log=$ping_log"
                echo
                echo "workflow:"
                echo "  1. Copy the overlay config to both hosts or point both hosts at equivalent configs."
                echo "  2. Set P2P_VPN_VPN_REPRO_PING_TARGET to the remote tunnel address on each host."
                echo "  3. Run the generated host script with sudo on each host."
                echo "  4. Compare health, routes, paths, MTU, capabilities, status, JSON snapshots, daemon logs, host network, and ping output."
              } > "$summary"
            }

            echo "writing public VPN repro artifacts to $artifact_dir" >&2
            write_metadata
            write_host_network
            write_runner "$host_a_script" "Host A"
            write_runner "$host_b_script" "Host B"
            write_collect
            write_shutdown
            write_commands
            write_summary
            echo "metadata: $metadata" >&2
            echo "host network: $host_network" >&2
            echo "replay commands: $commands" >&2
            echo "Host A VPN script: $host_a_script" >&2
            echo "Host B VPN script: $host_b_script" >&2
            echo "collect script: $collect_script" >&2
            echo "shutdown script: $shutdown_script" >&2
            echo "summary: $summary" >&2
          '';
        };
        moduleEval = lib.nixosSystem {
          inherit system;
          modules = [
            self.nixosModules.default
            (
              { ... }:
              {
                system.stateVersion = "25.11";
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
                services.p2p-vpn.instances.node-b = {
                  enable = true;
                  configFile = "/etc/p2p-vpn/node-b.json";
                  controlSocket = null;
                };
              }
            )
          ];
        };
        nixosSmokeConfig = pkgs.runCommand "p2p-vpn-nixos-smoke-config.json" {
          nativeBuildInputs = [ package ];
        } ''
          p2p-vpn init-config \
            --output "$out" \
            --network nixos-smoke \
            --interface hs-smoke0 \
            --disable-mdns \
            --disable-kademlia \
            --force
        '';
        nixosVmSmoke = pkgs.testers.nixosTest {
          name = "p2p-vpn-nixos-module-smoke";
          nodes.machine =
            { pkgs, ... }:
            {
              imports = [ self.nixosModules.default ];

              system.stateVersion = "25.11";
              environment.systemPackages = [ package ];
              environment.etc."p2p-vpn/smoke.json".source = nixosSmokeConfig;

              services.p2p-vpn.instances.smoke = {
                enable = true;
                configFile = "/etc/p2p-vpn/smoke.json";
                metricsIntervalSeconds = 1;
                controlSocket = "/run/p2p-vpn-smoke/control.sock";
              };
            };

          testScript = ''
            machine.start()
            machine.wait_for_unit("multi-user.target")
            machine.wait_for_unit("p2p-vpn-smoke.service")
            machine.wait_for_file("/run/p2p-vpn-smoke/control.sock")

            machine.succeed(
                "p2p-vpn daemon-status "
                "--socket /run/p2p-vpn-smoke/control.sock "
                "--timeout-seconds 5 | tee /tmp/p2p-vpn-status"
            )
            machine.succeed("grep -q '^tun_read_packets ' /tmp/p2p-vpn-status")
            machine.succeed("grep -q '^packet_plane_session_ttl_seconds ' /tmp/p2p-vpn-status")
            machine.succeed("grep -q '^packet_plane_sessions ' /tmp/p2p-vpn-status")
            machine.succeed(
                "p2p-vpn daemon-status "
                "--socket /run/p2p-vpn-smoke/control.sock "
                "--timeout-seconds 5 "
                "--format prometheus | tee /tmp/p2p-vpn-prometheus"
            )
            machine.succeed("grep -q '^p2p_vpn_tun_read_packets ' /tmp/p2p-vpn-prometheus")
            machine.succeed("grep -q '^p2p_vpn_packet_plane_sessions ' /tmp/p2p-vpn-prometheus")
            machine.succeed(
                "p2p-vpn daemon-health "
                "--socket /run/p2p-vpn-smoke/control.sock "
                "--timeout-seconds 5 "
                "--wait-seconds 5 | tee /tmp/p2p-vpn-health"
            )
            machine.succeed("grep -q '^daemon_health_ready true$' /tmp/p2p-vpn-health")
            machine.succeed("grep -q '^daemon_health_check daemon_running ok ' /tmp/p2p-vpn-health")

            machine.succeed("systemctl stop p2p-vpn-smoke.service")
            machine.wait_until_fails("test -S /run/p2p-vpn-smoke/control.sock")
            machine.succeed("test \"$(systemctl show p2p-vpn-smoke.service -p Result --value)\" = success")
          '';
        };
      in
      {
        packages = {
          default = package;
          check-fast = checkFast;

          releaseArchive = pkgs.runCommand "p2p-vpn-0.1.0-${system}.tar.gz" {
          nativeBuildInputs = [ pkgs.gnutar ];
        } ''
          release_dir="$TMPDIR/p2p-vpn-0.1.0-${system}"
          mkdir -p "$release_dir/bin" "$release_dir/docs" "$release_dir/examples" "$release_dir/nix"
          cp ${package}/bin/p2p-vpn "$release_dir/bin/"
          cp ${./README.md} "$release_dir/README.md"
          cp ${./docs/feature-matrix.md} "$release_dir/docs/feature-matrix.md"
          cp ${./docs/network-debugging.md} "$release_dir/docs/network-debugging.md"
          cp ${./docs/namespace-e2e-smoke.md} "$release_dir/docs/namespace-e2e-smoke.md"
          cp ${./docs/public-bootstrap-smoke.md} "$release_dir/docs/public-bootstrap-smoke.md"
          cp ${./flake.nix} "$release_dir/flake.nix"
          cp ${./flake.lock} "$release_dir/flake.lock"
          cp ${./Cargo.toml} "$release_dir/Cargo.toml"
          cp -R ${./examples/nixos-mesh} "$release_dir/examples/nixos-mesh"
          cp ${./nix/nixos-module.nix} "$release_dir/nix/nixos-module.nix"
          tar --sort=name --mtime="UTC 1970-01-01" \
            --owner=0 --group=0 --numeric-owner \
            -czf "$out" -C "$TMPDIR" "p2p-vpn-0.1.0-${system}"
        '';
        } // lib.optionalAttrs pkgs.stdenv.isLinux {
          namespace-preflight = namespacePreflight;
          namespace-repro = namespaceRepro;
          public-relay-repro = publicRelayRepro;
          public-vpn-repro = publicVpnRepro;
        };

        apps = {
          default = {
            type = "app";
            program = "${self.packages.${system}.default}/bin/p2p-vpn";
            meta = {
              description = "Run the p2p-vpn CLI";
            };
          };
          check-fast = {
            type = "app";
            program = "${checkFast}/bin/p2p-vpn-check-fast";
            meta = {
              description = "Run formatter, tests, and clippy in the Nix tool environment";
            };
          };
        } // lib.optionalAttrs pkgs.stdenv.isLinux {
          namespace-preflight = {
            type = "app";
            program = "${namespacePreflight}/bin/p2p-vpn-namespace-preflight";
            meta = {
              description = "Check host support for privileged namespace E2E tests";
            };
          };
          namespace-repro = {
            type = "app";
            program = "${namespaceRepro}/bin/p2p-vpn-namespace-repro";
            meta = {
              description = "Run namespace E2E tests with repro artifacts preserved";
            };
          };
          public-relay-repro = {
            type = "app";
            program = "${publicRelayRepro}/bin/p2p-vpn-public-relay-repro";
            meta = {
              description = "Run public IPFS relay and DCUtR repro diagnostics";
            };
          };
          public-vpn-repro = {
            type = "app";
            program = "${publicVpnRepro}/bin/p2p-vpn-public-vpn-repro";
            meta = {
              description = "Generate two-host public relay VPN data-plane repro scripts";
            };
          };
          tun-e2e = {
            type = "app";
            program = "${tunE2e}/bin/p2p-vpn-tun-e2e";
            meta = {
              description = "Run privileged Linux TUN namespace end-to-end tests";
            };
          };
        };

        checks = {
          package = package;
          releaseArchive = self.packages.${system}.releaseArchive;
          releaseArchiveSanity = pkgs.runCommand "p2p-vpn-release-archive-sanity" {
            nativeBuildInputs = [
              pkgs.gnutar
              pkgs.gzip
            ];
          } ''
            archive=${self.packages.${system}.releaseArchive}
            root="p2p-vpn-0.1.0-${system}"

            tar -tzf "$archive" > entries
            while IFS= read -r entry; do
              case "$entry" in
                "$root" | "$root/"*) ;;
                *) echo "archive entry outside $root: $entry" >&2; exit 1 ;;
              esac

              case "$entry" in
                /* | ../* | *"/../"* | *"/.." )
                  echo "unsafe archive path: $entry" >&2
                  exit 1
                  ;;
              esac
            done < entries

            for path in \
              "$root/bin/p2p-vpn" \
              "$root/README.md" \
              "$root/flake.nix" \
              "$root/flake.lock" \
              "$root/Cargo.toml" \
              "$root/docs/feature-matrix.md" \
              "$root/docs/network-debugging.md" \
              "$root/docs/namespace-e2e-smoke.md" \
              "$root/docs/public-bootstrap-smoke.md" \
              "$root/examples/nixos-mesh/flake.nix" \
              "$root/nix/nixos-module.nix"
            do
              grep -Fx "$path" entries >/dev/null || {
                echo "release archive missing $path" >&2
                exit 1
              }
            done

            mkdir unpacked
            tar -xzf "$archive" -C unpacked
            test -x "unpacked/$root/bin/p2p-vpn"
            "unpacked/$root/bin/p2p-vpn" --help > help
            grep -q "p2p-vpn" help
            grep -q "Usage:" help
            touch $out
          '';
          clippy = pkgs.rustPlatform.buildRustPackage {
            pname = "p2p-vpn-clippy";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [
              pkgs.clippy
              pkgs.pkg-config
            ];
            buildPhase = ''
              runHook preBuild
              cargo clippy --all-targets -- -D warnings
              runHook postBuild
            '';
            installPhase = ''
              runHook preInstall
              touch $out
              runHook postInstall
            '';
            doCheck = false;
          };
          fmt = pkgs.runCommand "p2p-vpn-fmt" { nativeBuildInputs = [ cargo pkgs.rustfmt ]; } ''
            cd ${self}
            cargo fmt --check
            touch $out
          '';
        } // lib.optionalAttrs pkgs.stdenv.isLinux {
          nixos-module = pkgs.runCommand "p2p-vpn-nixos-module" {
            execStart = moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.ExecStart;
            execStartNoSocket = moduleEval.config.systemd.services.p2p-vpn-node-b.serviceConfig.ExecStart;
            execStop = builtins.toJSON moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.ExecStop;
            execStopNoSocket = builtins.toJSON moduleEval.config.systemd.services.p2p-vpn-node-b.serviceConfig.ExecStop;
            killSignal = moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.KillSignal;
            timeoutStopSec = moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.TimeoutStopSec;
            runtimeDirectory = moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.RuntimeDirectory;
            runtimeDirectoryMode = moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.RuntimeDirectoryMode;
            capabilityBoundingSet = builtins.toJSON moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.CapabilityBoundingSet;
            deviceAllow = builtins.toJSON moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.DeviceAllow;
            devicePolicy = moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.DevicePolicy;
            lockPersonality = builtins.toJSON moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.LockPersonality;
            memoryDenyWriteExecute = builtins.toJSON moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.MemoryDenyWriteExecute;
            noNewPrivileges = builtins.toJSON moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.NoNewPrivileges;
            privateTmp = builtins.toJSON moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.PrivateTmp;
            protectClock = builtins.toJSON moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.ProtectClock;
            protectHome = moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.ProtectHome;
            protectHostname = builtins.toJSON moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.ProtectHostname;
            protectKernelLogs = builtins.toJSON moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.ProtectKernelLogs;
            protectKernelModules = builtins.toJSON moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.ProtectKernelModules;
            protectKernelTunables = builtins.toJSON moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.ProtectKernelTunables;
            protectSystem = moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.ProtectSystem;
            restrictAddressFamilies = builtins.toJSON moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.RestrictAddressFamilies;
            restrictRealtime = builtins.toJSON moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.RestrictRealtime;
            systemCallArchitectures = moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.SystemCallArchitectures;
            umask = moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.UMask;
            tcpPorts = builtins.toJSON moduleEval.config.networking.firewall.allowedTCPPorts;
            udpPorts = builtins.toJSON moduleEval.config.networking.firewall.allowedUDPPorts;
            kernelModules = builtins.toJSON moduleEval.config.boot.kernelModules;
          } ''
            case "$execStart" in
              *"p2p-vpn up --config /etc/p2p-vpn/node-a.json --metrics-interval-seconds 10 --control-socket /run/p2p-vpn-node-a/control.sock"*) ;;
              *) echo "unexpected ExecStart: $execStart" >&2; exit 1 ;;
            esac
            case "$execStartNoSocket" in
              *"--control-socket"*) echo "disabled control socket still in ExecStart: $execStartNoSocket" >&2; exit 1 ;;
              *"p2p-vpn up --config /etc/p2p-vpn/node-b.json"*) ;;
              *) echo "unexpected no-socket ExecStart: $execStartNoSocket" >&2; exit 1 ;;
            esac
            case "$execStop" in
              *"p2p-vpn daemon-shutdown --socket /run/p2p-vpn-node-a/control.sock"*) ;;
              *) echo "unexpected ExecStop: $execStop" >&2; exit 1 ;;
            esac
            test "$execStopNoSocket" = '[]'
            test "$killSignal" = SIGTERM
            test "$timeoutStopSec" = 30s
            test "$runtimeDirectory" = p2p-vpn-node-a
            test "$runtimeDirectoryMode" = 0750
            test "$capabilityBoundingSet" = '["CAP_NET_ADMIN","CAP_NET_RAW"]'
            test "$deviceAllow" = '["/dev/net/tun rw"]'
            test "$devicePolicy" = closed
            test "$lockPersonality" = true
            test "$memoryDenyWriteExecute" = true
            test "$noNewPrivileges" = true
            test "$privateTmp" = true
            test "$protectClock" = true
            test "$protectHome" = read-only
            test "$protectHostname" = true
            test "$protectKernelLogs" = true
            test "$protectKernelModules" = true
            test "$protectKernelTunables" = true
            test "$protectSystem" = strict
            test "$restrictAddressFamilies" = '["AF_INET","AF_INET6","AF_NETLINK","AF_UNIX"]'
            test "$restrictRealtime" = true
            test "$systemCallArchitectures" = native
            test "$umask" = 0077
            test "$tcpPorts" = '[4001]'
            test "$udpPorts" = '[4001,51820,51821]'
            case "$kernelModules" in
              *tun*) ;;
              *) echo "tun kernel module not requested: $kernelModules" >&2; exit 1 ;;
            esac
            touch $out
          '';
          nixos-vm-smoke = nixosVmSmoke;
        };

        devShells.default = pkgs.mkShell {
          packages = [
            cargo
            rust
            pkgs.clippy
            pkgs.rustfmt
            pkgs.rust-analyzer
            pkgs.pkg-config
            pkgs.jujutsu
            pkgs.iproute2
            pkgs.iputils
            pkgs.procps
            pkgs.util-linux
          ];

          RUST_BACKTRACE = "1";
        };
      }
    );
}
