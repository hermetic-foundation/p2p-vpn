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
            cargo clippy --all-targets -- -D clippy::correctness -D clippy::suspicious -D clippy::perf
          '';
        };
        checkOperational = pkgs.writeShellApplication {
          name = "p2p-vpn-check-operational";
          runtimeInputs = [
            pkgs.coreutils
            pkgs.nix
          ];
          text = ''
            usage() {
              cat <<'USAGE'
Usage: p2p-vpn-check-operational [--list] [--skip-vms]

Runs the local operational release gate for p2p-vpn.

Options:
  --list      Print the checks without running them.
  --skip-vms  Omit NixOS VM checks.
USAGE
            }

            list_only=0
            skip_vms=0
            while [[ "$#" -gt 0 ]]; do
              case "$1" in
                --list)
                  list_only=1
                  ;;
                --skip-vms)
                  skip_vms=1
                  ;;
                -h|--help)
                  usage
                  exit 0
                  ;;
                *)
                  echo "unknown argument: $1" >&2
                  usage >&2
                  exit 2
                  ;;
              esac
              shift
            done

            if [[ ! -f flake.nix || ! -f Cargo.toml ]]; then
              echo "p2p-vpn-check-operational must be run from the p2p-vpn repository root" >&2
              exit 2
            fi

            system="${system}"
            is_linux="${if pkgs.stdenv.isLinux then "1" else "0"}"
            checks=(
              ".#checks.$system.package"
              ".#checks.$system.fmt"
              ".#checks.$system.clippy"
              ".#checks.$system.releaseArchiveSanity"
            )

            linux_checks=(
              ".#checks.$system.nixos-module"
              ".#checks.$system.public-relay-repro-structure"
              ".#checks.$system.public-vpn-repro-structure"
              ".#checks.$system.public-vpn-repro-evidence-structure"
              ".#checks.$system.public-vpn-evidence-check"
            )

            vm_checks=(
              ".#checks.$system.nixos-vm-minimal-lan"
              ".#checks.$system.nixos-vm-forced-relay"
              ".#checks.$system.nixos-vm-network-move"
            )

            if [[ "$is_linux" -eq 1 ]]; then
              checks+=("''${linux_checks[@]}")
              if [[ "$skip_vms" -eq 0 ]]; then
                checks+=("''${vm_checks[@]}")
              fi
            fi

            if [[ "$list_only" -eq 1 ]]; then
              printf '%s\n' "operational check targets:"
              printf '  %s\n' "''${checks[@]}"
              printf '%s\n' ""
              if [[ "$is_linux" -eq 1 ]]; then
                printf '%s\n' "external proof still required:"
                printf '%s\n' "  nix run .#public-vpn-evidence-check -- --host-a HOST_A/vpn-repro-evidence.json --host-b HOST_B/vpn-repro-evidence.json --require-relay --require-direct --require-dcutr --require-quic-session"
              else
                printf '%s\n' "NixOS, VM, and public two-host gates are Linux-only."
              fi
              exit 0
            fi

            nix build --no-write-lock-file -L "''${checks[@]}"
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
        membershipRecordRepro = pkgs.writeShellApplication {
          name = "p2p-vpn-membership-record-repro";
          runtimeInputs = [
            pkgs.bash
            pkgs.jq
          ];
          text = ''
            export P2P_VPN_BIN="${package}/bin/p2p-vpn"
            exec bash ${./scripts/membership-record-repro.sh} "$@"
          '';
        };
        debugBundle = pkgs.writeShellApplication {
          name = "p2p-vpn-debug-bundle";
          runtimeInputs = [
            package
            cargo
            rust
            checkFast
            pkgs.bash
            pkgs.coreutils
            pkgs.git
            pkgs.jq
            pkgs.jujutsu
            pkgs.nix
          ] ++ lib.optionals pkgs.stdenv.isLinux [
            pkgs.iproute2
            pkgs.procps
            pkgs.util-linux
          ];
          text = ''
            exec bash ${./scripts/debug-bundle.sh} "$@"
          '';
        };
        publicRelayRepro = pkgs.writeShellApplication {
          name = "p2p-vpn-public-relay-repro";
          runtimeInputs = [
            package
            pkgs.coreutils
            pkgs.git
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
            vpn_host_a_config="$artifact_dir/public-vpn-host-a.json"
            vpn_host_b_config="$artifact_dir/public-vpn-host-b.json"
            vpn_host_a_relay_reservation_report="$artifact_dir/public-vpn-host-a-relay-reservation-check.json"
            vpn_host_b_relay_reservation_report="$artifact_dir/public-vpn-host-b-relay-reservation-check.json"
            dcutr_report="$artifact_dir/public-relay-dcutr-report.json"
            membership_config="$artifact_dir/public-membership-dht-config.json"
            membership_root_record="$artifact_dir/public-membership-root.record.json"
            membership_installed_config="$artifact_dir/public-membership-dht-config.with-record.json"
            membership_dht_report="$artifact_dir/public-membership-dht-bootstrap-check.json"
            dcutr_listener_descriptor="$artifact_dir/public-dcutr-listener.json"
            dcutr_listen_report="$artifact_dir/public-relay-dcutr-listen-report.json"
            dcutr_dial_report="$artifact_dir/public-dcutr-dial-report.json"
            metadata="$artifact_dir/repro-metadata.txt"
            host_network="$artifact_dir/repro-host-network.txt"
            commands="$artifact_dir/repro-commands.sh"
            retry_env="$artifact_dir/repro-retry-env.sh"
            phase_log="$artifact_dir/repro-phases.tsv"
            phase_logs_dir="$artifact_dir/phase-logs"
            phase_logs_manifest="$artifact_dir/repro-phase-logs.tsv"
            dcutr_listen_script="$artifact_dir/repro-dcutr-listen-host-a.sh"
            dcutr_dial_script="$artifact_dir/repro-dcutr-dial-host-b.sh"
            summary="$artifact_dir/repro-summary.txt"
            summary_json="$artifact_dir/repro-summary.json"
            scan_timeout="''${P2P_VPN_RELAY_SCAN_TIMEOUT_SECONDS:-30}"
            candidate_timeout="''${P2P_VPN_RELAY_CANDIDATE_TIMEOUT_SECONDS:-45}"
            max_candidates="''${P2P_VPN_RELAY_MAX_CANDIDATES:-8}"
            max_validation="''${P2P_VPN_RELAY_MAX_VALIDATION_CANDIDATES:-8}"
            phase_timeout="''${P2P_VPN_REPRO_PHASE_TIMEOUT_SECONDS:-}"
            base_config="''${P2P_VPN_REPRO_BASE_CONFIG:-}"
            repro_candidates_file="''${P2P_VPN_REPRO_CANDIDATES_FILE:-}"
            repro_relay_candidate="''${P2P_VPN_REPRO_RELAY_CANDIDATE:-}"
            dcutr_serve_seconds="''${P2P_VPN_REPRO_DCUTR_SERVE_SECONDS:-900}"
            dcutr_dial_timeout="''${P2P_VPN_REPRO_DCUTR_DIAL_TIMEOUT_SECONDS:-90}"
            membership_dht="''${P2P_VPN_REPRO_MEMBERSHIP_DHT:-0}"
            membership_network="''${P2P_VPN_REPRO_MEMBERSHIP_NETWORK:-public-membership-repro}"
            membership_dht_timeout="''${P2P_VPN_REPRO_MEMBERSHIP_DHT_TIMEOUT_SECONDS:-45}"
            vpn_host_a_route="''${P2P_VPN_REPRO_VPN_HOST_A_ROUTE:-10.42.0.1/32}"
            vpn_host_b_route="''${P2P_VPN_REPRO_VPN_HOST_B_ROUTE:-10.42.0.2/32}"
            vpn_network="''${P2P_VPN_REPRO_VPN_NETWORK:-public-vpn-repro}"
            require_vpn_relay_reservations="''${P2P_VPN_REPRO_REQUIRE_VPN_RELAY_RESERVATIONS:-0}"
            require_dcutr="''${P2P_VPN_REPRO_REQUIRE_DCUTR:-0}"
            relay_check_base_args=()
            if [[ -n "$base_config" ]]; then
              relay_check_base_args=(--config "$base_config")
            fi
            if [[ -z "$phase_timeout" ]]; then
              phase_timeout="$((candidate_timeout * max_validation + 60))"
            fi
            status=0
            phase_index=0
            phase_results=()
            mkdir -p "$phase_logs_dir"

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
                echo "P2P_VPN_REPRO_MEMBERSHIP_DHT=$membership_dht"
                echo "P2P_VPN_REPRO_MEMBERSHIP_NETWORK=$membership_network"
                echo "P2P_VPN_REPRO_MEMBERSHIP_DHT_TIMEOUT_SECONDS=$membership_dht_timeout"
                echo "P2P_VPN_REPRO_VPN_HOST_A_ROUTE=$vpn_host_a_route"
                echo "P2P_VPN_REPRO_VPN_HOST_B_ROUTE=$vpn_host_b_route"
                echo "P2P_VPN_REPRO_VPN_NETWORK=$vpn_network"
                echo "P2P_VPN_REPRO_REQUIRE_VPN_RELAY_RESERVATIONS=$require_vpn_relay_reservations"
                echo "P2P_VPN_REPRO_REQUIRE_DCUTR=$require_dcutr"
                echo "P2P_VPN_RELAY_SCAN_TIMEOUT_SECONDS=$scan_timeout"
                echo "P2P_VPN_RELAY_CANDIDATE_TIMEOUT_SECONDS=$candidate_timeout"
                echo "P2P_VPN_RELAY_MAX_CANDIDATES=$max_candidates"
                echo "P2P_VPN_RELAY_MAX_VALIDATION_CANDIDATES=$max_validation"
                echo "P2P_VPN_REPRO_PHASE_TIMEOUT_SECONDS=$phase_timeout"
                echo
                echo "[git rev-parse HEAD]"
                git rev-parse HEAD 2>&1 || true
                echo
                echo "[git status --short]"
                git status --short 2>&1 || true
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
                printf "export P2P_VPN_REPRO_MEMBERSHIP_DHT=%q\n" "$membership_dht"
                printf "export P2P_VPN_REPRO_MEMBERSHIP_NETWORK=%q\n" "$membership_network"
                printf "export P2P_VPN_REPRO_MEMBERSHIP_DHT_TIMEOUT_SECONDS=%q\n" "$membership_dht_timeout"
                printf "export P2P_VPN_REPRO_VPN_HOST_A_ROUTE=%q\n" "$vpn_host_a_route"
                printf "export P2P_VPN_REPRO_VPN_HOST_B_ROUTE=%q\n" "$vpn_host_b_route"
                printf "export P2P_VPN_REPRO_VPN_NETWORK=%q\n" "$vpn_network"
                printf "export P2P_VPN_REPRO_REQUIRE_VPN_RELAY_RESERVATIONS=%q\n" "$require_vpn_relay_reservations"
                printf "export P2P_VPN_REPRO_REQUIRE_DCUTR=%q\n" "$require_dcutr"
                printf "export P2P_VPN_RELAY_SCAN_TIMEOUT_SECONDS=%q\n" "$scan_timeout"
                printf "export P2P_VPN_RELAY_CANDIDATE_TIMEOUT_SECONDS=%q\n" "$candidate_timeout"
                printf "export P2P_VPN_RELAY_MAX_CANDIDATES=%q\n" "$max_candidates"
                printf "export P2P_VPN_RELAY_MAX_VALIDATION_CANDIDATES=%q\n" "$max_validation"
                printf "export P2P_VPN_REPRO_PHASE_TIMEOUT_SECONDS=%q\n" "$phase_timeout"
                echo
                if [[ -n "$repro_candidates_file" ]]; then
                  printf "cp %q %q\n" "$repro_candidates_file" "$candidates"
                  echo
                elif [[ -n "$repro_relay_candidate" ]]; then
                  printf "printf '%%s\\n' %q > %q\n" "$repro_relay_candidate" "$candidates"
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
                printf "  --write-host-a-config %q \\\\\n" "$vpn_host_a_config"
                printf "  --write-host-b-config %q \\\\\n" "$vpn_host_b_config"
                printf "  --two-host-network %q \\\\\n" "$vpn_network"
                printf "  --host-a-route %q \\\\\n" "$vpn_host_a_route"
                printf "  --host-b-route %q \\\\\n" "$vpn_host_b_route"
                echo "  --force"
                echo
                echo "p2p-vpn bootstrap-check \\"
                printf "  --config %q \\\\\n" "$vpn_host_a_config"
                printf "  --timeout-seconds %q \\\\\n" "$candidate_timeout"
                echo "  --require-relay-reservations \\"
                printf "  --write-report %q \\\\\n" "$vpn_host_a_relay_reservation_report"
                echo "  --force"
                echo
                echo "p2p-vpn bootstrap-check \\"
                printf "  --config %q \\\\\n" "$vpn_host_b_config"
                printf "  --timeout-seconds %q \\\\\n" "$candidate_timeout"
                echo "  --require-relay-reservations \\"
                printf "  --write-report %q \\\\\n" "$vpn_host_b_relay_reservation_report"
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
                if [[ "$membership_dht" == 1 ]]; then
                  echo "p2p-vpn init-config \\"
                  printf "  --output %q \\\\\n" "$membership_config"
                  printf "  --network %q \\\\\n" "$membership_network"
                  echo "  --public-ipfs-profile \\"
                  echo "  --disable-dcutr \\"
                  echo "  --force"
                  echo
                  echo "p2p-vpn membership-record-issue \\"
                  printf "  --issuer-config %q \\\\\n" "$membership_config"
                  echo "  --issuer-as-member \\"
                  printf "  --output %q \\\\\n" "$membership_root_record"
                  echo "  --force"
                  echo
                  echo "p2p-vpn membership-record-install \\"
                  printf "  --config %q \\\\\n" "$membership_config"
                  printf "  --record %q \\\\\n" "$membership_root_record"
                  printf "  --output %q \\\\\n" "$membership_installed_config"
                  echo "  --force"
                  echo
                  echo "p2p-vpn bootstrap-check \\"
                  printf "  --config %q \\\\\n" "$membership_installed_config"
                  printf "  --timeout-seconds %q \\\\\n" "$membership_dht_timeout"
                  echo "  --require-membership-records \\"
                  printf "  --write-report %q \\\\\n" "$membership_dht_report"
                  echo "  --force"
                  echo
                fi
                printf "%q\n" "$dcutr_listen_script"
                printf "%q\n" "$dcutr_dial_script"
                printf "source %q\n" "$retry_env"
                printf "jq . %q\n" "$summary_json"
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

            write_retry_env() {
              selected_relay_candidate="$(selected_public_dcutr_candidate)"
              {
                echo "#!/usr/bin/env bash"
                echo "# Source this file from the repository root before retrying a public relay repro."
                printf "export P2P_VPN_REPRO_CANDIDATES_FILE=%q\n" "$candidates"
                if [[ -n "$base_config" ]]; then
                  printf "export P2P_VPN_REPRO_BASE_CONFIG=%q\n" "$base_config"
                fi
                if [[ -n "$selected_relay_candidate" ]]; then
                  printf "export P2P_VPN_REPRO_RELAY_CANDIDATE=%q\n" "$selected_relay_candidate"
                fi
                printf "export P2P_VPN_REPRO_DCUTR_SERVE_SECONDS=%q\n" "$dcutr_serve_seconds"
                printf "export P2P_VPN_REPRO_DCUTR_DIAL_TIMEOUT_SECONDS=%q\n" "$dcutr_dial_timeout"
                printf "export P2P_VPN_REPRO_MEMBERSHIP_DHT=%q\n" "$membership_dht"
                printf "export P2P_VPN_REPRO_MEMBERSHIP_NETWORK=%q\n" "$membership_network"
                printf "export P2P_VPN_REPRO_MEMBERSHIP_DHT_TIMEOUT_SECONDS=%q\n" "$membership_dht_timeout"
                printf "export P2P_VPN_REPRO_REQUIRE_VPN_RELAY_RESERVATIONS=%q\n" "$require_vpn_relay_reservations"
                printf "export P2P_VPN_RELAY_SCAN_TIMEOUT_SECONDS=%q\n" "$scan_timeout"
                printf "export P2P_VPN_RELAY_CANDIDATE_TIMEOUT_SECONDS=%q\n" "$candidate_timeout"
                printf "export P2P_VPN_RELAY_MAX_CANDIDATES=%q\n" "$max_candidates"
                printf "export P2P_VPN_RELAY_MAX_VALIDATION_CANDIDATES=%q\n" "$max_validation"
                echo
                echo "# Examples:"
                echo "#   P2P_VPN_REPRO_DIR=/tmp/p2p-vpn-public-relay-retry nix run .#public-relay-repro"
                echo "#   P2P_VPN_REPRO_MEMBERSHIP_DHT=0 P2P_VPN_RELAY_CANDIDATE_TIMEOUT_SECONDS=90 nix run .#public-relay-repro"
              } > "$retry_env"
              chmod +x "$retry_env"
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
                "  diagnoses=" + (
                  [(.candidates // [])[].diagnosis | select(. != null)]
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
                "  accepted_relay_reservations=" + (
                  [(.candidates // [])[].bootstrap.relay_results[]? | select(.accepted == true)]
                  | length
                  | tostring
                ),
                "  relayed_peer_outbound_circuits=" + (
                  [(.candidates // [])[].bootstrap.relayed_peer_results[]? | select(.outbound_circuit == true)]
                  | length
                  | tostring
                ),
                "  first_error=" + (
                  (
                    [
                      (.candidates // [])[].error,
                      (.peer_results // [])[].last_error,
                      (.candidates // [])[].bootstrap.peer_results[]?.last_error,
                      (.candidates // [])[].bootstrap.relayed_peer_results[]?.last_error
                    ]
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

            # shellcheck disable=SC2329
            check_two_host_vpn_relay_reservations() {
              if [[ ! -s "$vpn_host_a_config" || ! -s "$vpn_host_b_config" ]]; then
                echo "generated Host A/B VPN configs are missing; cannot check relay reservations" >&2
                return 2
              fi

              vpn_host_a_check_config="$artifact_dir/public-vpn-host-a.relay-check.json"
              vpn_host_b_check_config="$artifact_dir/public-vpn-host-b.relay-check.json"
              jq '.network.listen_addresses = ["/ip4/0.0.0.0/tcp/0"]' \
                "$vpn_host_a_config" > "$vpn_host_a_check_config"
              jq '.network.listen_addresses = ["/ip4/0.0.0.0/tcp/0"]' \
                "$vpn_host_b_config" > "$vpn_host_b_check_config"

              p2p-vpn bootstrap-check \
                --config "$vpn_host_a_check_config" \
                --timeout-seconds "$candidate_timeout" \
                --require-relay-reservations \
                --write-report "$vpn_host_a_relay_reservation_report" \
                --force

              p2p-vpn bootstrap-check \
                --config "$vpn_host_b_check_config" \
                --timeout-seconds "$candidate_timeout" \
                --require-relay-reservations \
                --write-report "$vpn_host_b_relay_reservation_report" \
                --force
            }

            append_membership_dht_summary() {
              if [[ "$membership_dht" != 1 ]]; then
                echo "membership-dht report: disabled" >> "$summary"
                return
              fi

              if [[ ! -s "$membership_dht_report" ]]; then
                echo "membership-dht report: missing" >> "$summary"
                return
              fi

              echo "membership-dht report: $membership_dht_report" >> "$summary"
              jq -r '
                "  succeeded=" + ((.succeeded // false) | tostring),
                "  timeout_seconds=" + ((.timeout_seconds // "n/a") | tostring),
                "  configured_bootstrap_peers=" + ((.bootstrap.configured_bootstrap_peers // "n/a") | tostring),
                "  connected_bootstrap_peers=" + ((.bootstrap.connected_bootstrap_peers // "n/a") | tostring),
                "  membership_records_configured=" + ((.bootstrap.membership_records.configured_records // "n/a") | tostring),
                "  membership_records_publish_succeeded=" + ((.bootstrap.membership_records.publish_succeeded // false) | tostring),
                "  membership_records_found=" + ((.bootstrap.membership_records.found_records // 0) | tostring),
                "  membership_records_verified=" + ((.bootstrap.membership_records.verified_records // 0) | tostring),
                "  membership_records_accepted=" + ((.bootstrap.membership_records.accepted_records // 0) | tostring),
                "  membership_records_invalid=" + ((.bootstrap.membership_records.invalid_records // 0) | tostring),
                "  membership_records_last_error=" + ((.bootstrap.membership_records.last_error // "none") | tostring)
              ' "$membership_dht_report" >> "$summary"
            }

            # shellcheck disable=SC2329
            setup_membership_dht_config() {
              p2p-vpn init-config \
                --output "$membership_config" \
                --network "$membership_network" \
                --listen-address /ip4/0.0.0.0/tcp/0 \
                --public-ipfs-profile \
                --disable-dcutr \
                --force

              p2p-vpn membership-record-issue \
                --issuer-config "$membership_config" \
                --issuer-as-member \
                --output "$membership_root_record" \
                --force

              p2p-vpn membership-record-install \
                --config "$membership_config" \
                --record "$membership_root_record" \
                --output "$membership_installed_config" \
                --force
            }

            # shellcheck disable=SC2329
            check_membership_dht() {
              p2p-vpn bootstrap-check \
                --config "$membership_installed_config" \
                --timeout-seconds "$membership_dht_timeout" \
                --require-membership-records \
                --write-report "$membership_dht_report" \
                --force
            }

            write_report_summary_json() {
              label="$1"
              report="$2"
              output="$3"
              if [[ ! -s "$report" ]]; then
                jq -n \
                  --arg label "$label" \
                  --arg path "$report" \
                  '{label: $label, path: $path, present: false}' > "$output"
                return
              fi

              jq \
                --arg label "$label" \
                --arg path "$report" '
                  def candidate_values($field):
                    [(.candidates // [])[].bootstrap[$field][]?];
                  def elapsed_values:
                    [(.candidates // [])[].elapsed_millis | select(. != null)];
                  {
                    label: $label,
                    path: $path,
                    present: true,
                    succeeded: (.succeeded // false),
                    candidates: ((.candidates // []) | length),
                    skipped_candidates: ((.skipped_candidates // []) | length),
                    host_reachable_candidates: ((.host_reachable_candidates // []) | length),
                    discovered_routing_peers: (.discovered_routing_peers // null),
                    dialed_routing_peers: (.dialed_routing_peers // null),
                    closest_peer_results: (.closest_peer_results // null),
                    failure_stages: (
                      reduce [(.candidates // [])[].failure_stage | select(. != null)][] as $stage
                        ({}; .[$stage] = ((.[$stage] // 0) + 1))
                    ),
                    diagnoses: (
                      reduce [(.candidates // [])[].diagnosis | select(. != null)][] as $diagnosis
                        ({}; .[$diagnosis] = ((.[$diagnosis] // 0) + 1))
                    ),
                    elapsed_millis: (
                      elapsed_values as $values
                      | {
                          count: ($values | length),
                          min: (if ($values | length) == 0 then null else ($values | min) end),
                          max: (if ($values | length) == 0 then null else ($values | max) end)
                        }
                    ),
                    relayed_connection_addresses: (
                      candidate_values("relayed_connection_addresses") as $values
                      | {
                          count: ($values | length),
                          first: (if ($values | length) == 0 then null else $values[0] end)
                        }
                    ),
                    direct_connection_addresses: (
                      candidate_values("direct_connection_addresses") as $values
                      | {
                          count: ($values | length),
                          first: (if ($values | length) == 0 then null else $values[0] end)
                        }
                    ),
                    relay_diagnostics: {
                      accepted_reservations: (
                        [(.candidates // [])[].bootstrap.relay_results[]? | select(.accepted == true)]
                        | length
                      ),
                      relayed_listen_addresses: (
                        [(.candidates // [])[].bootstrap.relay_results[]? | select(.relayed_listen_address == true)]
                        | length
                      ),
                      outbound_circuits: (
                        [(.candidates // [])[].bootstrap.relayed_peer_results[]? | select(.outbound_circuit == true)]
                        | length
                      ),
                      connected_relayed_peers: (
                        [(.candidates // [])[].bootstrap.relayed_peer_results[]? | select(.connected == true)]
                        | length
                      )
                    },
                    first_error: (
                      [
                        (.candidates // [])[].error,
                        (.peer_results // [])[].last_error,
                        (.candidates // [])[].bootstrap.peer_results[]?.last_error,
                        (.candidates // [])[].bootstrap.relayed_peer_results[]?.last_error
                      ]
                      | map(select(. != null and . != ""))
                      | first // null
                    )
                  }
                ' "$report" > "$output"
            }

            write_machine_summary() {
              selected_relay_candidate="$(selected_public_dcutr_candidate)"
              scan_summary="$artifact_dir/.repro-scan-summary.json"
              relay_summary="$artifact_dir/.repro-relay-check-summary.json"
              dcutr_summary="$artifact_dir/.repro-dcutr-summary.json"
              vpn_host_a_relay_reservation_summary="$artifact_dir/.repro-vpn-host-a-relay-reservation-summary.json"
              vpn_host_b_relay_reservation_summary="$artifact_dir/.repro-vpn-host-b-relay-reservation-summary.json"
              membership_dht_summary="$artifact_dir/.repro-membership-dht-summary.json"
              phase_summary="$artifact_dir/.repro-phase-results.json"

              write_report_summary_json "scan" "$scan_report" "$scan_summary"
              write_report_summary_json "relay-check" "$relay_report" "$relay_summary"
              write_report_summary_json "dcutr" "$dcutr_report" "$dcutr_summary"
              write_bootstrap_summary_json "vpn-host-a-relay-reservation" "$vpn_host_a_relay_reservation_report" "$vpn_host_a_relay_reservation_summary"
              write_bootstrap_summary_json "vpn-host-b-relay-reservation" "$vpn_host_b_relay_reservation_report" "$vpn_host_b_relay_reservation_summary"
              if [[ -s "$membership_dht_report" ]]; then
                jq \
                  --arg path "$membership_dht_report" '
                    {
                      label: "membership-dht",
                      path: $path,
                      present: true,
                      succeeded: (.succeeded // false),
                      timeout_seconds: (.timeout_seconds // null),
                      configured_bootstrap_peers: (.bootstrap.configured_bootstrap_peers // null),
                      connected_bootstrap_peers: (.bootstrap.connected_bootstrap_peers // null),
                      membership_records: (.bootstrap.membership_records // null),
                      kademlia: (.bootstrap.kademlia // null),
                      autonat_status: (.bootstrap.autonat_status // null)
                    }
                  ' "$membership_dht_report" > "$membership_dht_summary"
              else
                jq -n \
                  --arg path "$membership_dht_report" \
                  '{label: "membership-dht", path: $path, present: false}' > "$membership_dht_summary"
              fi
              if [[ "''${#phase_results[@]}" -eq 0 ]]; then
                jq -n '[]' > "$phase_summary"
              else
                printf "%s\n" "''${phase_results[@]}" | jq -R . | jq -s . > "$phase_summary"
              fi

              jq -n \
                --arg artifact_dir "$artifact_dir" \
                --arg metadata "$metadata" \
                --arg host_network "$host_network" \
                --arg commands "$commands" \
                --arg retry_env "$retry_env" \
                --arg phase_log "$phase_log" \
                --arg phase_logs_dir "$phase_logs_dir" \
                --arg phase_logs_manifest "$phase_logs_manifest" \
                --arg candidate_file "$candidates" \
                --arg relay_assisted_config "$relay_config" \
                --arg vpn_host_a_config "$vpn_host_a_config" \
                --arg vpn_host_b_config "$vpn_host_b_config" \
                --arg vpn_host_a_relay_reservation_report "$vpn_host_a_relay_reservation_report" \
                --arg vpn_host_b_relay_reservation_report "$vpn_host_b_relay_reservation_report" \
                --arg membership_config "$membership_config" \
                --arg membership_installed_config "$membership_installed_config" \
                --arg membership_root_record "$membership_root_record" \
                --arg membership_dht_report "$membership_dht_report" \
                --arg dcutr_listen_script "$dcutr_listen_script" \
                --arg dcutr_dial_script "$dcutr_dial_script" \
                --arg listener_descriptor "$dcutr_listener_descriptor" \
                --arg dcutr_dial_report "$dcutr_dial_report" \
                --arg selected_relay_candidate "$selected_relay_candidate" \
                --arg dcutr_serve_seconds "$dcutr_serve_seconds" \
                --arg dcutr_dial_timeout_seconds "$dcutr_dial_timeout" \
                --arg ipv4_route_to_1_1_1_1 "$(route_available -4 1.1.1.1)" \
                --arg ipv6_route_to_2606_4700_4700_1111 "$(route_available -6 2606:4700:4700::1111)" \
                --slurpfile phases "$phase_summary" \
                --slurpfile scan "$scan_summary" \
                --slurpfile relay "$relay_summary" \
                --slurpfile dcutr "$dcutr_summary" \
                --slurpfile vpn_host_a_relay_reservation "$vpn_host_a_relay_reservation_summary" \
                --slurpfile vpn_host_b_relay_reservation "$vpn_host_b_relay_reservation_summary" \
                --slurpfile membership_dht "$membership_dht_summary" \
                '{
                  schema_version: 1,
                  artifact_dir: $artifact_dir,
                  artifacts: {
                    metadata: $metadata,
                    host_network: $host_network,
                    commands: $commands,
                    retry_env: $retry_env,
                    phase_log: $phase_log,
                    phase_logs_dir: $phase_logs_dir,
                    phase_logs_manifest: $phase_logs_manifest,
                    candidate_file: $candidate_file,
                    relay_assisted_config: $relay_assisted_config,
                    vpn_host_a_config: $vpn_host_a_config,
                    vpn_host_b_config: $vpn_host_b_config,
                    vpn_host_a_relay_reservation_report: $vpn_host_a_relay_reservation_report,
                    vpn_host_b_relay_reservation_report: $vpn_host_b_relay_reservation_report,
                    membership_config: $membership_config,
                    membership_installed_config: $membership_installed_config,
                    membership_root_record: $membership_root_record,
                    membership_dht_report: $membership_dht_report
                  },
                  host: {
                    ipv4_route_to_1_1_1_1: $ipv4_route_to_1_1_1_1,
                    ipv6_route_to_2606_4700_4700_1111: $ipv6_route_to_2606_4700_4700_1111
                  },
                  phase_results: $phases[0],
                  reports: {
                    scan: $scan[0],
                    relay_check: $relay[0],
                    dcutr: $dcutr[0],
                    vpn_host_a_relay_reservation: $vpn_host_a_relay_reservation[0],
                    vpn_host_b_relay_reservation: $vpn_host_b_relay_reservation[0],
                    membership_dht: $membership_dht[0]
                  },
                  two_host_dcutr_handoff: {
                    selected_relay_candidate: (if $selected_relay_candidate == "" then null else $selected_relay_candidate end),
                    listen_script: $dcutr_listen_script,
                    dial_script: $dcutr_dial_script,
                    listener_descriptor: $listener_descriptor,
                    dial_report: $dcutr_dial_report,
                    serve_seconds: ($dcutr_serve_seconds | tonumber),
                    dial_timeout_seconds: ($dcutr_dial_timeout_seconds | tonumber)
                  }
                }' > "$summary_json"

              rm -f \
                "$scan_summary" \
                "$relay_summary" \
                "$dcutr_summary" \
                "$vpn_host_a_relay_reservation_summary" \
                "$vpn_host_b_relay_reservation_summary" \
                "$membership_dht_summary" \
                "$phase_summary"
            }

            write_bootstrap_summary_json() {
              label="$1"
              report="$2"
              output="$3"
              if [[ ! -s "$report" ]]; then
                jq -n \
                  --arg label "$label" \
                  --arg path "$report" \
                  '{label: $label, path: $path, present: false}' > "$output"
                return
              fi

              jq \
                --arg label "$label" \
                --arg path "$report" '
                  {
                    label: $label,
                    path: $path,
                    present: true,
                    succeeded: (.succeeded // false),
                    configured_relay_reservations: (.bootstrap.configured_relay_reservations // null),
                    accepted_relay_reservations: (.bootstrap.accepted_relay_reservations // null),
                    relayed_listen_addresses: (.bootstrap.relayed_listen_addresses // null),
                    relay_results: (.bootstrap.relay_results // []),
                    first_error: (
                      [
                        (.bootstrap.peer_results[]?.last_error),
                        (.bootstrap.relayed_peer_results[]?.last_error)
                      ]
                      | map(select(. != null and . != ""))
                      | first // null
                    )
                  }
                ' "$report" > "$output"
            }

            write_summary() {
              {
                echo "p2p-vpn public relay repro summary"
                echo "artifact_dir=$artifact_dir"
                echo "metadata=$metadata"
                echo "host_network=$host_network"
                echo "commands=$commands"
                echo "retry_env=$retry_env"
                echo "phase_log=$phase_log"
                echo "phase_logs_dir=$phase_logs_dir"
                echo "phase_logs_manifest=$phase_logs_manifest"
                echo "summary_json=$summary_json"
                echo "dcutr_listen_script=$dcutr_listen_script"
                echo "dcutr_dial_script=$dcutr_dial_script"
                echo "candidate_file=$candidates"
                echo "relay_assisted_config=$relay_config"
                echo "vpn_host_a_config=$vpn_host_a_config"
                echo "vpn_host_b_config=$vpn_host_b_config"
                echo "membership_dht_report=$membership_dht_report"
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
              append_membership_dht_summary
              append_handoff_summary
              write_machine_summary
            }

            record_phase_result() {
              phase="$1"
              phase_status="$2"
              phase_started_utc="$3"
              phase_finished_utc="$4"
              phase_elapsed="$5"
              phase_stdout="$6"
              phase_stderr="$7"
              phase_results+=("$phase status=$phase_status elapsed_seconds=$phase_elapsed stdout=$phase_stdout stderr=$phase_stderr")
              printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\n" \
                "$phase" \
                "$phase_status" \
                "$phase_started_utc" \
                "$phase_finished_utc" \
                "$phase_elapsed" \
                "$phase_stdout" \
                "$phase_stderr" >> "$phase_log"
              printf "%s\t%s\t%s\n" \
                "$phase" \
                "$phase_stdout" \
                "$phase_stderr" >> "$phase_logs_manifest"
            }

            run_phase() {
              phase="$1"
              shift
              phase_index="$((phase_index + 1))"
              phase_slug="$(printf "%s" "$phase" | tr '[:upper:] ' '[:lower:]-' | tr -cd '[:alnum:]._-')"
              if [[ -z "$phase_slug" ]]; then
                phase_slug="phase"
              fi
              phase_id="$(printf "%02d-%s" "$phase_index" "$phase_slug")"
              phase_stdout="$phase_logs_dir/$phase_id.stdout"
              phase_stderr="$phase_logs_dir/$phase_id.stderr"
              echo "$phase" >&2
              phase_started_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
              phase_started="$(date +%s)"
              set +e
              if declare -F "$1" >/dev/null; then
                "$@" > >(tee "$phase_stdout") 2> >(tee "$phase_stderr" >&2)
              else
                timeout --kill-after=5s "''${phase_timeout}s" "$@" > >(tee "$phase_stdout") 2> >(tee "$phase_stderr" >&2)
              fi
              phase_status="$?"
              set -e
              phase_finished="$(date +%s)"
              phase_finished_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
              phase_elapsed="$((phase_finished - phase_started))"
              if [[ "$phase_status" -ne 0 ]]; then
                echo "$phase failed with exit status $phase_status after ''${phase_elapsed}s" >&2
                status=1
              fi
              record_phase_result "$phase" "$phase_status" "$phase_started_utc" "$phase_finished_utc" "$phase_elapsed" "$phase_stdout" "$phase_stderr"
            }

            echo "writing public relay repro artifacts to $artifact_dir" >&2
            printf "phase\tstatus\tstarted_utc\tfinished_utc\telapsed_seconds\tstdout_log\tstderr_log\n" > "$phase_log"
            printf "phase\tstdout_log\tstderr_log\n" > "$phase_logs_manifest"
            write_metadata
            write_host_network
            write_commands
            if [[ "$membership_dht" == 1 ]]; then
              run_phase "creating signed membership-record DHT repro config" \
                setup_membership_dht_config
              if [[ -s "$membership_installed_config" ]]; then
                run_phase "checking public Kademlia membership-record propagation" \
                  check_membership_dht
              else
                echo "membership DHT config was not created; skipping membership-record bootstrap-check" >&2
                status=1
              fi
            else
              phase_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
              record_phase_result "membership-record DHT repro disabled" 0 "$phase_utc" "$phase_utc" 0 "" ""
            fi
            if [[ -n "$repro_candidates_file" ]]; then
              if [[ ! -s "$repro_candidates_file" ]]; then
                echo "P2P_VPN_REPRO_CANDIDATES_FILE must point to a nonempty relay candidate file" >&2
                exit 2
              fi
              cp "$repro_candidates_file" "$candidates"
              phase_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
              record_phase_result "using supplied public relay candidate file" 0 "$phase_utc" "$phase_utc" 0 "" ""
            elif [[ -n "$repro_relay_candidate" ]]; then
              printf "%s\n" "$repro_relay_candidate" > "$candidates"
              phase_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
              record_phase_result "using supplied public relay candidate" 0 "$phase_utc" "$phase_utc" 0 "" ""
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
                --write-host-a-config "$vpn_host_a_config" \
                --write-host-b-config "$vpn_host_b_config" \
                --two-host-network "$vpn_network" \
                --host-a-route "$vpn_host_a_route" \
                --host-b-route "$vpn_host_b_route" \
                --force

              if [[ "$require_vpn_relay_reservations" == 1 ]]; then
                run_phase "checking generated two-host VPN relay reservations" \
                  check_two_host_vpn_relay_reservations
              else
                phase_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
                record_phase_result "generated two-host VPN relay reservation check disabled" 0 "$phase_utc" "$phase_utc" 0 "" ""
              fi

              if [[ "$require_dcutr" == 1 ]]; then
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
                phase_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
                record_phase_result "DCUtR success evidence check disabled" 0 "$phase_utc" "$phase_utc" 0 "" ""
              fi
            else
              echo "candidate file is empty; skipping relay-check probes" >&2
              status=1
            fi

            echo "candidate file: $candidates" >&2
            echo "scan report: $scan_report" >&2
            echo "relay-check report: $relay_report" >&2
            echo "relay-assisted config: $relay_config" >&2
            echo "Host A VPN config: $vpn_host_a_config" >&2
            echo "Host B VPN config: $vpn_host_b_config" >&2
            echo "DCUtR report: $dcutr_report" >&2
            echo "membership DHT report: $membership_dht_report" >&2
            write_public_dcutr_handoff_scripts
            write_retry_env
            write_summary
            echo "metadata: $metadata" >&2
            echo "host network: $host_network" >&2
            echo "replay commands: $commands" >&2
            echo "retry environment: $retry_env" >&2
            echo "phase log: $phase_log" >&2
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
            pkgs.git
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
            host_a_config="''${P2P_VPN_VPN_REPRO_HOST_A_CONFIG:-}"
            host_b_config="''${P2P_VPN_VPN_REPRO_HOST_B_CONFIG:-}"
            public_relay_dir="''${P2P_VPN_VPN_REPRO_PUBLIC_RELAY_DIR:-}"
            if [[ -z "$host_a_config" && -n "$public_relay_dir" && -s "$public_relay_dir/public-vpn-host-a.json" ]]; then
              host_a_config="$public_relay_dir/public-vpn-host-a.json"
            fi
            if [[ -z "$host_b_config" && -n "$public_relay_dir" && -s "$public_relay_dir/public-vpn-host-b.json" ]]; then
              host_b_config="$public_relay_dir/public-vpn-host-b.json"
            fi
            if [[ -z "$config" && -z "$host_a_config" && -z "$host_b_config" && -n "$public_relay_dir" && -s "$public_relay_dir/public-relay-config.json" ]]; then
              config="$public_relay_dir/public-relay-config.json"
            fi
            if [[ -z "$config" && -z "$host_a_config" && -z "$host_b_config" ]]; then
              config="$artifact_dir/public-relay-config.json"
            fi
            if [[ -n "$config" ]]; then
              if [[ -z "$host_a_config" ]]; then
                host_a_config="$config"
              fi
              if [[ -z "$host_b_config" ]]; then
                host_b_config="$config"
              fi
            fi
            if [[ ! -s "$host_a_config" || ! -s "$host_b_config" ]]; then
              cat >&2 <<EOF
missing relay-assisted VPN configs:
  Host A: $host_a_config
  Host B: $host_b_config

Set P2P_VPN_VPN_REPRO_HOST_A_CONFIG and P2P_VPN_VPN_REPRO_HOST_B_CONFIG to
matched overlay configs, set P2P_VPN_VPN_REPRO_CONFIG to one shared existing
overlay config, or set
P2P_VPN_VPN_REPRO_PUBLIC_RELAY_DIR to a public-relay-repro artifact directory
containing public-vpn-host-a.json and public-vpn-host-b.json.
EOF
              exit 2
            fi
            if [[ -z "$config" ]]; then
              config="$host_a_config"
            fi

            route_ping_target_from_config() {
              config_path="$1"
              jq -r '
                [(.network.routes // [])[].prefix // ""]
                | map(select(length > 0))
                | .[0] // ""
                | sub("/.*$"; "")
              ' "$config_path" 2>/dev/null || true
            }

            metadata="$artifact_dir/vpn-repro-metadata.txt"
            host_network="$artifact_dir/vpn-repro-host-network.txt"
            host_network_before="$artifact_dir/vpn-repro-host-network-before.txt"
            host_network_after="$artifact_dir/vpn-repro-host-network-after.txt"
            commands="$artifact_dir/vpn-repro-commands.sh"
            host_a_script="$artifact_dir/vpn-repro-host-a.sh"
            host_b_script="$artifact_dir/vpn-repro-host-b.sh"
            collect_script="$artifact_dir/vpn-repro-collect.sh"
            shutdown_script="$artifact_dir/vpn-repro-shutdown.sh"
            summary="$artifact_dir/vpn-repro-summary.txt"
            evidence_json="$artifact_dir/vpn-repro-evidence.json"
            result_log="$artifact_dir/vpn-repro-result.txt"
            ping_target="''${P2P_VPN_VPN_REPRO_PING_TARGET:-}"
            host_a_ping_target="''${P2P_VPN_VPN_REPRO_HOST_A_PING_TARGET:-}"
            host_b_ping_target="''${P2P_VPN_VPN_REPRO_HOST_B_PING_TARGET:-}"
            if [[ -z "$host_a_ping_target" && -n "$ping_target" ]]; then
              host_a_ping_target="$ping_target"
            fi
            if [[ -z "$host_b_ping_target" && -n "$ping_target" ]]; then
              host_b_ping_target="$ping_target"
            fi
            if [[ -z "$host_a_ping_target" ]]; then
              host_a_ping_target="$(route_ping_target_from_config "$host_b_config")"
            fi
            if [[ -z "$host_b_ping_target" ]]; then
              host_b_ping_target="$(route_ping_target_from_config "$host_a_config")"
            fi
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
            daemon_log_tail="$artifact_dir/p2p-vpn-daemon-tail.txt"
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
              peer_count="$(jq '(.peers // []) | length' "$host_a_config" 2>/dev/null || echo unknown)"
              route_count="$(jq '(.network.routes // []) | length' "$host_a_config" 2>/dev/null || echo unknown)"
              interface_name="$(jq -r '.interface.name // "unknown"' "$host_a_config" 2>/dev/null || echo unknown)"
              interface_mtu="$(jq -r '.interface.mtu // "unknown"' "$host_a_config" 2>/dev/null || echo unknown)"
              {
                echo "started_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
                echo "working_directory=$(pwd)"
                echo "system=$(uname -a)"
                echo "p2p_vpn_binary=$(command -v p2p-vpn)"
                echo "p2p_vpn_version=$(p2p-vpn --version 2>/dev/null || echo unknown)"
                echo "artifact_dir=$artifact_dir"
                echo "config=$config"
                echo "host_a_config=$host_a_config"
                echo "host_b_config=$host_b_config"
                echo "public_relay_dir=$public_relay_dir"
                echo "peer_count=$peer_count"
                echo "route_count=$route_count"
                echo "interface_name=$interface_name"
                echo "interface_mtu=$interface_mtu"
                echo "control_socket=$control_socket"
                echo "pidfile=$pidfile"
                echo "daemon_log=$daemon_log"
                echo "daemon_log_tail=$daemon_log_tail"
                echo "host_network_before=$host_network_before"
                echo "host_network_after=$host_network_after"
                echo "result_log=$result_log"
                echo "ping_target=$ping_target"
                echo "host_a_ping_target=$host_a_ping_target"
                echo "host_b_ping_target=$host_b_ping_target"
                echo "ping_count=$ping_count"
                echo "ping_timeout_seconds=$ping_timeout"
                echo "health_wait_seconds=$health_wait"
                echo "metrics_interval_seconds=$metrics_interval"
                echo "require_packet_session=$require_packet_session"
                echo "require_quic_session=$require_quic_session"
                echo
                echo "[git rev-parse HEAD]"
                git rev-parse HEAD 2>&1 || true
                echo
                echo "[git status --short]"
                git status --short 2>&1 || true
              } > "$metadata"
            }

            write_runner() {
              script="$1"
              role="$2"
              runner_config="$3"
              runner_ping_target="$4"
              {
                echo "#!/usr/bin/env bash"
                echo "set -euo pipefail"
                echo "umask 077"
                printf "artifact_dir=%q\n" "$artifact_dir"
                printf "config=%q\n" "$runner_config"
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
                printf "evidence_json=%q\n" "$evidence_json"
                printf "host_network_before=%q\n" "$host_network_before"
                printf "host_network_after=%q\n" "$host_network_after"
                printf "daemon_log_tail=%q\n" "$daemon_log_tail"
                printf "result_log=%q\n" "$result_log"
                printf "ping_log=%q\n" "$ping_log"
                printf "ping_target=%q\n" "$runner_ping_target"
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
                echo "record_status() {"
                echo "  label=\"\$1\""
                echo "  status=\"\$2\""
                printf "%s\n" "  printf '%s %s exit=%s\\n' \"\$(date -u +%Y-%m-%dT%H:%M:%SZ)\" \"\$label\" \"\$status\" >> \"\$result_log\""
                echo "}"
                echo "capture_host_network() {"
                echo "  target=\"\$1\""
                echo "  {"
                echo "    echo \"captured_utc=\$(date -u +%Y-%m-%dT%H:%M:%SZ)\""
                echo "    echo \"system=\$(uname -a)\""
                echo "    echo"
                echo "    echo \"[ip -br addr]\""
                echo "    ip -br addr || true"
                echo "    echo"
                echo "    echo \"[ip -d addr]\""
                echo "    ip -d addr || true"
                echo "    echo"
                echo "    echo \"[ip -s link]\""
                echo "    ip -s link || true"
                echo "    echo"
                echo "    echo \"[ip route show]\""
                echo "    ip route show || true"
                echo "    echo"
                echo "    echo \"[ip -6 route show]\""
                echo "    ip -6 route show || true"
                echo "    if [[ -n \"\$ping_target\" ]]; then"
                echo "      echo"
                echo "      echo \"[ip route get \$ping_target]\""
                echo "      ip route get \"\$ping_target\" || true"
                echo "    fi"
                echo "    echo"
                echo "    echo \"[ss -lunp]\""
                echo "    ss -lunp || true"
                echo "    echo"
                echo "    echo \"[ps -o pid,ppid,stat,comm,args -C p2p-vpn]\""
                echo "    ps -o pid,ppid,stat,comm,args -C p2p-vpn || true"
                echo "  } > \"\$target\" 2>&1"
                echo "}"
                echo "capture_final_artifacts() {"
                echo "  if [[ -r \"\$daemon_log\" ]]; then"
                echo "    tail -n 200 \"\$daemon_log\" > \"\$daemon_log_tail\" 2>/dev/null || true"
                echo "  fi"
                echo "  if [[ ! -S \"\$control_socket\" ]]; then"
                echo "    record_status final_artifacts 2"
                echo "    write_evidence_summary"
                echo "    return"
                echo "  fi"
                echo "  p2p-vpn daemon-status --socket \"\$control_socket\" > \"\$final_status_log\" 2> \"\$final_status_log.stderr\"; record_status final_status \"\$?\""
                echo "  p2p-vpn daemon-status --socket \"\$control_socket\" --format prometheus > \"\$final_prometheus_log\" 2> \"\$final_prometheus_log.stderr\"; record_status final_prometheus \"\$?\""
                echo "  p2p-vpn daemon-state --socket \"\$control_socket\" --format json > \"\$final_state_json\" 2> \"\$final_state_json.stderr\"; record_status final_state_json \"\$?\""
                echo "  p2p-vpn daemon-paths --socket \"\$control_socket\" --format json > \"\$final_paths_json\" 2> \"\$final_paths_json.stderr\"; record_status final_paths_json \"\$?\""
                echo "  write_evidence_summary"
                echo "}"
                echo "metric_value() {"
                echo "  metric=\"\$1\""
                echo "  if [[ -s \"\$final_prometheus_log\" ]]; then"
                echo "    awk -v metric=\"\$metric\" '\$1 == metric { value = \$2 } END { if (value == \"\") value = 0; print value }' \"\$final_prometheus_log\""
                echo "  else"
                printf "%s\n" "    printf '0\\n'"
                echo "  fi"
                echo "}"
                echo "result_status() {"
                echo "  label=\"\$1\""
                echo "  if [[ -s \"\$result_log\" ]]; then"
                echo "    awk -v label=\"\$label\" '\$2 == label { value = \$3 } END { sub(/^exit=/, \"\", value); if (value == \"\") value = \"null\"; print value }' \"\$result_log\""
                echo "  else"
                printf "%s\n" "    printf 'null\\n'"
                echo "  fi"
                echo "}"
                echo "json_lines() {"
                echo "  file=\"\$1\""
                echo "  if [[ -s \"\$file\" ]]; then"
                printf "%s\n" "    jq -R -s 'split(\"\\n\")[:-1]' \"\$file\""
                echo "  else"
                printf "%s\n" "    printf '[]\\n'"
                echo "  fi"
                echo "}"
                echo "json_view_lines() {"
                echo "  file=\"\$1\""
                echo "  if [[ -s \"\$file\" ]]; then"
                echo "    jq '.lines // []' \"\$file\""
                echo "  else"
                printf "%s\n" "    printf '[]\\n'"
                echo "  fi"
                echo "}"
                echo "write_evidence_summary() {"
                echo "  health_ready=false"
                echo "  if [[ -s \"\$health_log\" ]] && grep -q '^daemon_health_ready true$' \"\$health_log\"; then"
                echo "    health_ready=true"
                echo "  fi"
                echo "  ping_exit=\"\$(result_status ping)\""
                echo "  if [[ \"\$ping_exit\" == null ]]; then"
                echo "    ping_succeeded=false"
                echo "  elif [[ \"\$ping_exit\" == 0 ]]; then"
                echo "    ping_succeeded=true"
                echo "  else"
                echo "    ping_succeeded=false"
                echo "  fi"
                echo "  path_lines_json=\"\$(json_view_lines \"\$final_paths_json\")\""
                echo "  state_lines_json=\"\$(json_view_lines \"\$final_state_json\")\""
                echo "  health_lines_json=\"\$(json_lines \"\$health_log\")\""
                echo "  result_lines_json=\"\$(json_lines \"\$result_log\")\""
                echo "  jq -n \\"
                echo "    --arg generated_utc \"\$(date -u +%Y-%m-%dT%H:%M:%SZ)\" \\"
                echo "    --arg artifact_dir \"\$artifact_dir\" \\"
                echo "    --arg config \"\$config\" \\"
                echo "    --arg ping_target \"\$ping_target\" \\"
                echo "    --argjson health_ready \"\$health_ready\" \\"
                echo "    --argjson ping_succeeded \"\$ping_succeeded\" \\"
                echo "    --argjson ping_exit \"\$ping_exit\" \\"
                echo "    --argjson health_lines \"\$health_lines_json\" \\"
                echo "    --argjson result_lines \"\$result_lines_json\" \\"
                echo "    --argjson path_lines \"\$path_lines_json\" \\"
                echo "    --argjson state_lines \"\$state_lines_json\" \\"
                echo "    --arg path_promotions_to_direct \"\$(metric_value p2p_vpn_path_promotions_to_direct)\" \\"
                echo "    --arg dcutr_successes \"\$(metric_value p2p_vpn_dcutr_successes)\" \\"
                echo "    --arg direct_connections \"\$(metric_value p2p_vpn_direct_connections_established)\" \\"
                echo "    --arg relayed_connections \"\$(metric_value p2p_vpn_relayed_connections_established)\" \\"
                echo "    --arg peers_with_supported_path \"\$(metric_value p2p_vpn_path_peers_with_supported_path)\" \\"
                echo "    --arg packet_plane_sessions \"\$(metric_value p2p_vpn_packet_plane_sessions)\" \\"
                echo "    --arg packet_plane_quic_sessions \"\$(metric_value p2p_vpn_packet_plane_quic_sessions)\" \\"
                echo "    --arg healthy_direct_quic_datagram_paths \"\$(metric_value p2p_vpn_path_healthy_direct_quic_datagram_paths)\" \\"
                echo "    --arg healthy_direct_quic_stream_paths \"\$(metric_value p2p_vpn_path_healthy_direct_quic_stream_paths)\" \\"
                echo "    --arg healthy_direct_tcp_stream_paths \"\$(metric_value p2p_vpn_path_healthy_direct_tcp_stream_paths)\" \\"
                echo "    --arg healthy_relay_paths \"\$(metric_value p2p_vpn_path_healthy_relay_paths)\" \\"
                echo "    '{"
                echo "      schema_version: 1,"
                echo "      generated_utc: \$generated_utc,"
                echo "      artifact_dir: \$artifact_dir,"
                echo "      config: \$config,"
                echo "      ping_target: \$ping_target,"
                echo "      health_ready: \$health_ready,"
                echo "      ping_succeeded: \$ping_succeeded,"
                echo "      ping_exit: \$ping_exit,"
                echo "      metrics: {"
                echo "        path_promotions_to_direct: (\$path_promotions_to_direct | tonumber),"
                echo "        dcutr_successes: (\$dcutr_successes | tonumber),"
                echo "        direct_connections_established: (\$direct_connections | tonumber),"
                echo "        relayed_connections_established: (\$relayed_connections | tonumber),"
                echo "        peers_with_supported_path: (\$peers_with_supported_path | tonumber),"
                echo "        packet_plane_sessions: (\$packet_plane_sessions | tonumber),"
                echo "        packet_plane_quic_sessions: (\$packet_plane_quic_sessions | tonumber),"
                echo "        healthy_direct_quic_datagram_paths: (\$healthy_direct_quic_datagram_paths | tonumber),"
                echo "        healthy_direct_quic_stream_paths: (\$healthy_direct_quic_stream_paths | tonumber),"
                echo "        healthy_direct_tcp_stream_paths: (\$healthy_direct_tcp_stream_paths | tonumber),"
                echo "        healthy_relay_paths: (\$healthy_relay_paths | tonumber)"
                echo "      },"
                echo "      path_evidence: {"
                echo "        direct_lines: [\$path_lines[] | select(test(\"direct \"))],"
                echo "        relay_lines: [\$path_lines[] | select(test(\"circuit relay|relay true\"))]"
                echo "      },"
                echo "      health_lines: \$health_lines,"
                echo "      result_lines: \$result_lines,"
                echo "      final_state_lines: \$state_lines,"
                echo "      final_path_lines: \$path_lines"
                echo "    }' > \"\$evidence_json\""
                echo "}"
                echo "if [[ \"\''${P2P_VPN_VPN_REPRO_WRITE_EVIDENCE_ONLY:-0}\" == 1 ]]; then"
                echo "  write_evidence_summary"
                echo "  exit 0"
                echo "fi"
                echo "on_exit() {"
                echo "  status=\"\$?\""
                echo "  set +e"
                echo "  record_status script_exit \"\$status\""
                echo "  capture_host_network \"\$host_network_after\""
                echo "  capture_final_artifacts"
                echo "  exit \"\$status\""
                echo "}"
                echo "trap on_exit EXIT"
                echo "capture_host_network \"\$host_network_before\""
                echo "set +e"
                echo "p2p-vpn daemon-health \"\''${health_args[@]}\" | tee \"\$health_log\""
                echo "health_status=\"\''${PIPESTATUS[0]}\""
                echo "set -e"
                echo "record_status daemon_health \"\$health_status\""
                echo "if [[ \"\$health_status\" -ne 0 ]]; then"
                echo "  exit \"\$health_status\""
                echo "fi"
                echo "capture_daemon_views"
                echo "if [[ -n \"\$ping_target\" ]]; then"
                echo "  set +e"
                echo "  ping -c \"\$ping_count\" -W \"\$ping_timeout\" \"\$ping_target\" | tee \"\$ping_log\""
                echo "  ping_status=\"\''${PIPESTATUS[0]}\""
                echo "  set -e"
                echo "  record_status ping \"\$ping_status\""
                echo "  if [[ \"\$ping_status\" -ne 0 ]]; then"
                echo "    exit \"\$ping_status\""
                echo "  fi"
                echo "else"
                echo "  echo \"set the role-specific ping target to the remote tunnel address to prove data forwarding\" | tee \"\$ping_log\""
                echo "  record_status ping 2"
                echo "fi"
                printf "echo %q\n" "$role complete; artifacts in $artifact_dir"
              } > "$script"
              chmod +x "$script"
            }

            write_collect() {
              {
                echo "#!/usr/bin/env bash"
                echo "set -euo pipefail"
                printf "control_socket=%q\n" "$control_socket"
                printf "host_network_after=%q\n" "$host_network_after"
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
                echo "{"
                echo "  echo \"captured_utc=\$(date -u +%Y-%m-%dT%H:%M:%SZ)\""
                echo "  echo \"[ip -br addr]\""
                echo "  ip -br addr || true"
                echo "  echo \"[ip -d addr]\""
                echo "  ip -d addr || true"
                echo "  echo \"[ip -s link]\""
                echo "  ip -s link || true"
                echo "  echo \"[ip route show]\""
                echo "  ip route show || true"
                echo "  echo \"[ip -6 route show]\""
                echo "  ip -6 route show || true"
                echo "  echo \"[ss -lunp]\""
                echo "  ss -lunp || true"
                echo "} > \"\$host_network_after\" 2>&1"
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
                printf "export P2P_VPN_VPN_REPRO_HOST_A_CONFIG=%q\n" "$host_a_config"
                printf "export P2P_VPN_VPN_REPRO_HOST_B_CONFIG=%q\n" "$host_b_config"
                printf "export P2P_VPN_VPN_REPRO_HOST_A_PING_TARGET=%q\n" "$host_a_ping_target"
                printf "export P2P_VPN_VPN_REPRO_HOST_B_PING_TARGET=%q\n" "$host_b_ping_target"
                printf "export P2P_VPN_VPN_REPRO_CONTROL_SOCKET=%q\n" "$control_socket"
                printf "export P2P_VPN_VPN_REPRO_PIDFILE=%q\n" "$pidfile"
                printf "export P2P_VPN_VPN_REPRO_DAEMON_LOG=%q\n" "$daemon_log"
                if [[ -n "$ping_target" ]]; then
                  printf "export P2P_VPN_VPN_REPRO_PING_TARGET=%q\n" "$ping_target"
                fi
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
                printf "jq . %q\n" "$evidence_json"
              } > "$commands"
              chmod +x "$commands"
            }

            write_summary() {
              {
                echo "p2p-vpn public VPN repro summary"
                echo "artifact_dir=$artifact_dir"
                echo "config=$config"
                echo "host_a_config=$host_a_config"
                echo "host_b_config=$host_b_config"
                echo "host_a_ping_target=$host_a_ping_target"
                echo "host_b_ping_target=$host_b_ping_target"
                echo "metadata=$metadata"
                echo "host_network=$host_network"
                echo "host_network_before=$host_network_before"
                echo "host_network_after=$host_network_after"
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
                echo "evidence_json=$evidence_json"
                echo "daemon_log_tail=$daemon_log_tail"
                echo "result_log=$result_log"
                echo "ping_log=$ping_log"
                echo
                echo "workflow:"
                echo "  1. Copy the overlay config to both hosts or point both hosts at equivalent configs."
                echo "  2. Confirm each generated script's ping target is the remote tunnel address for that host."
                echo "  3. Run the generated host script with sudo on each host."
                echo "  4. Compare evidence_json first, then health, routes, paths, MTU, capabilities, status, JSON snapshots, daemon logs, host network, and ping output."
              } > "$summary"
            }

            echo "writing public VPN repro artifacts to $artifact_dir" >&2
            write_metadata
            write_host_network
            write_runner "$host_a_script" "Host A" "$host_a_config" "$host_a_ping_target"
            write_runner "$host_b_script" "Host B" "$host_b_config" "$host_b_ping_target"
            write_collect
            write_shutdown
            write_commands
            write_summary
            if [[ "''${P2P_VPN_VPN_REPRO_EVIDENCE_ONLY:-0}" == 1 ]]; then
              P2P_VPN_VPN_REPRO_WRITE_EVIDENCE_ONLY=1 bash "$host_a_script"
            fi
            echo "metadata: $metadata" >&2
            echo "host network: $host_network" >&2
            echo "replay commands: $commands" >&2
            echo "Host A VPN script: $host_a_script" >&2
            echo "Host B VPN script: $host_b_script" >&2
            echo "collect script: $collect_script" >&2
            echo "shutdown script: $shutdown_script" >&2
            echo "evidence summary: $evidence_json" >&2
            echo "summary: $summary" >&2
          '';
        };
        publicVpnEvidenceCheck = pkgs.writeShellApplication {
          name = "p2p-vpn-public-vpn-evidence-check";
          runtimeInputs = [
            pkgs.coreutils
            pkgs.jq
          ];
          text = ''
            usage() {
              cat >&2 <<'EOF'
Usage:
  p2p-vpn-public-vpn-evidence-check \
    --host-a HOST_A_EVIDENCE.json \
    --host-b HOST_B_EVIDENCE.json \
    [--write-report REPORT.json] \
    [--require-direct] \
    [--require-relay] \
    [--require-dcutr] \
    [--require-quic-session] \
    [--min-packet-sessions N]

Checks two public-vpn-repro evidence files for operational two-host proof.
EOF
            }

            host_a=""
            host_b=""
            report=""
            require_direct=0
            require_relay=0
            require_dcutr=0
            require_quic=0
            min_packet_sessions=1

            while [[ "$#" -gt 0 ]]; do
              case "$1" in
                --host-a)
                  host_a="''${2:-}"
                  shift 2
                  ;;
                --host-b)
                  host_b="''${2:-}"
                  shift 2
                  ;;
                --write-report)
                  report="''${2:-}"
                  shift 2
                  ;;
                --require-direct)
                  require_direct=1
                  shift
                  ;;
                --require-relay)
                  require_relay=1
                  shift
                  ;;
                --require-dcutr)
                  require_dcutr=1
                  shift
                  ;;
                --require-quic-session)
                  require_quic=1
                  shift
                  ;;
                --min-packet-sessions)
                  min_packet_sessions="''${2:-}"
                  shift 2
                  ;;
                -h|--help)
                  usage
                  exit 0
                  ;;
                *)
                  echo "unknown argument: $1" >&2
                  usage
                  exit 2
                  ;;
              esac
            done

            if [[ -z "$host_a" || -z "$host_b" ]]; then
              usage
              exit 2
            fi
            if [[ ! "$min_packet_sessions" =~ ^[0-9]+$ ]]; then
              echo "--min-packet-sessions must be a non-negative integer" >&2
              exit 2
            fi
            for evidence in "$host_a" "$host_b"; do
              if [[ ! -s "$evidence" ]]; then
                echo "missing evidence file: $evidence" >&2
                exit 2
              fi
              jq -e 'type == "object" and .schema_version == 1' "$evidence" >/dev/null
            done

            report_tmp="$(mktemp)"
            jq -n \
              --slurpfile host_a "$host_a" \
              --slurpfile host_b "$host_b" \
              --argjson require_direct "$require_direct" \
              --argjson require_relay "$require_relay" \
              --argjson require_dcutr "$require_dcutr" \
              --argjson require_quic "$require_quic" \
              --argjson min_packet_sessions "$min_packet_sessions" \
              '
              def metric($e; $name): ($e.metrics[$name] // 0);
              def direct_evidence($e):
                ((metric($e; "direct_connections_established") > 0)
                or (metric($e; "healthy_direct_quic_datagram_paths") > 0)
                or (metric($e; "healthy_direct_quic_stream_paths") > 0)
                or (metric($e; "healthy_direct_tcp_stream_paths") > 0)
                or (($e.path_evidence.direct_lines // []) | length > 0));
              def relay_evidence($e):
                ((metric($e; "relayed_connections_established") > 0)
                or (metric($e; "healthy_relay_paths") > 0)
                or (($e.path_evidence.relay_lines // []) | length > 0));
              def checks($name; $e): [
                {
                  name: ($name + ".health_ready"),
                  ok: ($e.health_ready == true),
                  detail: ($e.health_ready // null)
                },
                {
                  name: ($name + ".ping_succeeded"),
                  ok: ($e.ping_succeeded == true and ($e.ping_exit // 1) == 0),
                  detail: { ping_succeeded: ($e.ping_succeeded // null), ping_exit: ($e.ping_exit // null) }
                },
                {
                  name: ($name + ".supported_path"),
                  ok: (metric($e; "peers_with_supported_path") >= 1),
                  detail: metric($e; "peers_with_supported_path")
                },
                {
                  name: ($name + ".packet_sessions"),
                  ok: (metric($e; "packet_plane_sessions") >= $min_packet_sessions),
                  detail: metric($e; "packet_plane_sessions")
                },
                {
                  name: ($name + ".quic_session"),
                  ok: (($require_quic == 0) or (metric($e; "packet_plane_quic_sessions") >= 1)),
                  detail: metric($e; "packet_plane_quic_sessions")
                },
                {
                  name: ($name + ".direct_evidence"),
                  ok: (($require_direct == 0) or direct_evidence($e)),
                  detail: {
                    direct_connections_established: metric($e; "direct_connections_established"),
                    healthy_direct_quic_datagram_paths: metric($e; "healthy_direct_quic_datagram_paths"),
                    healthy_direct_quic_stream_paths: metric($e; "healthy_direct_quic_stream_paths"),
                    healthy_direct_tcp_stream_paths: metric($e; "healthy_direct_tcp_stream_paths"),
                    direct_lines: (($e.path_evidence.direct_lines // []) | length)
                  }
                },
                {
                  name: ($name + ".relay_evidence"),
                  ok: (($require_relay == 0) or relay_evidence($e)),
                  detail: {
                    relayed_connections_established: metric($e; "relayed_connections_established"),
                    healthy_relay_paths: metric($e; "healthy_relay_paths"),
                    relay_lines: (($e.path_evidence.relay_lines // []) | length)
                  }
                },
                {
                  name: ($name + ".dcutr_evidence"),
                  ok: (($require_dcutr == 0) or (metric($e; "dcutr_successes") >= 1)),
                  detail: metric($e; "dcutr_successes")
                }
              ];
              ($host_a[0]) as $a |
              ($host_b[0]) as $b |
              (checks("host_a"; $a) + checks("host_b"; $b)) as $checks |
              {
                schema_version: 1,
                generated_utc: (now | todateiso8601),
                host_a: $a.artifact_dir,
                host_b: $b.artifact_dir,
                requirements: {
                  min_packet_sessions: $min_packet_sessions,
                  require_quic_session: ($require_quic == 1),
                  require_direct: ($require_direct == 1),
                  require_relay: ($require_relay == 1),
                  require_dcutr: ($require_dcutr == 1)
                },
                checks: $checks,
                ok: (all($checks[]; .ok == true))
              }
            ' > "$report_tmp"

            if [[ -n "$report" ]]; then
              mkdir -p "$(dirname "$report")"
              cp "$report_tmp" "$report"
            fi

            jq -r '
              "public VPN evidence check: " + (if .ok then "ok" else "failed" end),
              ("host_a=" + (.host_a // "unknown")),
              ("host_b=" + (.host_b // "unknown")),
              (.checks[] | "check " + .name + " " + (if .ok then "ok" else "failed" end))
            ' "$report_tmp"

            if ! jq -e '.ok == true' "$report_tmp" >/dev/null; then
              jq -r '.checks[] | select(.ok != true) | "failed: " + .name + " detail=" + (.detail | @json)' "$report_tmp" >&2
              exit 1
            fi
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
                services.p2p-vpn.instances.node-c = {
                  enable = true;
                  networkName = "nixos-module";
                  localPeer = "0000000000000000000000000000000000000000000000000000000000000000";
                  privateKey = "BASE64_PRIVATE_KEY";
                  vpnIp = "10.44.0.1";
                  peers."1111111111111111111111111111111111111111111111111111111111111111" = {
                    ip = "192.168.0.203";
                    vpnIp = "10.44.0.2";
                  };
                  autoRelay = {
                    maxCandidates = 12;
                    maxReservations = 3;
                    retryIntervalSeconds = 45;
                  };
                };
                services.p2p-vpn.instances.node-d = {
                  enable = true;
                  networkName = "nixos-module";
                  localPeer = "2222222222222222222222222222222222222222222222222222222222222222";
                  privateKeyFile = "/run/secrets/p2p-vpn/node-d.key";
                  vpnIp = "fd00::1";
                  peers."3333333333333333333333333333333333333333333333333333333333333333".vpnIp = "fd00::2";
                };
                services.p2p-vpn.instances.node-e = {
                  enable = true;
                  networkName = "nixos-module-minimal";
                  privateKey = "BASE64_PRIVATE_KEY";
                  peers."5555555555555555555555555555555555555555555555555555555555555555" = { };
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
        nixosVmMesh = import ./tests/nixos/mesh.nix {
          inherit self pkgs package;
        };
        nixosVmForcedRelay = import ./tests/nixos/forced-relay.nix {
          inherit self pkgs package;
        };
        nixosVmNetworkMove = import ./tests/nixos/network-move.nix {
          inherit self pkgs package;
        };
        namespaceSmokePreflighted = pkgs.rustPlatform.buildRustPackage {
          pname = "p2p-vpn-namespace-smoke-preflighted";
          version = "0.1.0";
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [
            pkgs.pkg-config
            namespacePreflight
            pkgs.iproute2
            pkgs.iputils
            pkgs.procps
            pkgs.util-linux
          ];
          checkPhase = ''
            runHook preCheck

            mkdir -p "$TMPDIR/namespace-smoke"
            preflight_stdout="$TMPDIR/namespace-smoke/preflight.stdout"
            preflight_stderr="$TMPDIR/namespace-smoke/preflight.stderr"
            smoke_log="$TMPDIR/namespace-smoke/smoke.log"
            status_file="$TMPDIR/namespace-smoke/status.txt"

            set +e
            p2p-vpn-namespace-preflight >"$preflight_stdout" 2>"$preflight_stderr"
            preflight_status="$?"
            set -e

            if [[ "$preflight_status" -ne 0 ]]; then
              {
                echo "status=skipped"
                echo "reason=namespace preflight failed"
                echo "preflight_status=$preflight_status"
              } > "$status_file"
              cat "$preflight_stderr" >&2
            else
              {
                echo "status=running"
                echo "test=tun_namespace_ping_crosses_two_node_overlay"
              } > "$status_file"
              set -o pipefail
              cargo test --test tun_namespace tun_namespace_ping_crosses_two_node_overlay -- --ignored --exact --nocapture 2>&1 | tee "$smoke_log"
              {
                echo "status=passed"
                echo "test=tun_namespace_ping_crosses_two_node_overlay"
              } > "$status_file"
            fi

            runHook postCheck
          '';
          installPhase = ''
            runHook preInstall

            mkdir -p "$out"
            cp -R "$TMPDIR/namespace-smoke/." "$out/"

            runHook postInstall
          '';
        };
      in
      {
        packages = {
          default = package;
          check-fast = checkFast;
          check-operational = checkOperational;
          debug-bundle = debugBundle;
          membership-record-repro = membershipRecordRepro;

          releaseArchive = pkgs.runCommand "p2p-vpn-0.1.0-${system}.tar.gz" {
          nativeBuildInputs = [ pkgs.gnutar ];
        } ''
          release_dir="$TMPDIR/p2p-vpn-0.1.0-${system}"
          mkdir -p \
            "$release_dir/bin" \
            "$release_dir/docs/user" \
            "$release_dir/docs/developer" \
            "$release_dir/examples" \
            "$release_dir/nix" \
            "$release_dir/scripts"
          cp ${package}/bin/p2p-vpn "$release_dir/bin/"
          cp ${./README.md} "$release_dir/README.md"
          cp ${./docs/user/README.md} "$release_dir/docs/user/README.md"
          cp ${./docs/user/quick-start.md} "$release_dir/docs/user/quick-start.md"
          cp ${./docs/user/configuration.md} "$release_dir/docs/user/configuration.md"
          cp ${./docs/user/nixos.md} "$release_dir/docs/user/nixos.md"
          cp ${./docs/user/operations.md} "$release_dir/docs/user/operations.md"
          cp ${./docs/user/public-libp2p.md} "$release_dir/docs/user/public-libp2p.md"
          cp ${./docs/developer/README.md} "$release_dir/docs/developer/README.md"
          cp ${./docs/developer/architecture.md} "$release_dir/docs/developer/architecture.md"
          cp ${./docs/developer/feature-matrix.md} "$release_dir/docs/developer/feature-matrix.md"
          cp ${./docs/developer/testing.md} "$release_dir/docs/developer/testing.md"
          cp ${./docs/developer/network-debugging.md} "$release_dir/docs/developer/network-debugging.md"
          cp ${./docs/developer/public-bootstrap-smoke.md} "$release_dir/docs/developer/public-bootstrap-smoke.md"
          cp ${./flake.nix} "$release_dir/flake.nix"
          cp ${./flake.lock} "$release_dir/flake.lock"
          cp ${./Cargo.toml} "$release_dir/Cargo.toml"
          cp -R ${./examples/nixos-mesh} "$release_dir/examples/nixos-mesh"
          cp ${./nix/nixos-module.nix} "$release_dir/nix/nixos-module.nix"
          cp ${./scripts/debug-bundle.sh} "$release_dir/scripts/debug-bundle.sh"
          cp ${./scripts/membership-record-repro.sh} "$release_dir/scripts/membership-record-repro.sh"
          chmod +x "$release_dir/scripts/debug-bundle.sh"
          chmod +x "$release_dir/scripts/membership-record-repro.sh"
          tar --sort=name --mtime="UTC 1970-01-01" \
            --owner=0 --group=0 --numeric-owner \
            -czf "$out" -C "$TMPDIR" "p2p-vpn-0.1.0-${system}"
        '';
        } // lib.optionalAttrs pkgs.stdenv.isLinux {
          namespace-preflight = namespacePreflight;
          namespace-repro = namespaceRepro;
          public-relay-repro = publicRelayRepro;
          public-vpn-repro = publicVpnRepro;
          public-vpn-evidence-check = publicVpnEvidenceCheck;
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
          check-operational = {
            type = "app";
            program = "${checkOperational}/bin/p2p-vpn-check-operational";
            meta = {
              description = "Run the local operational release gate";
            };
          };
          membership-record-repro = {
            type = "app";
            program = "${membershipRecordRepro}/bin/p2p-vpn-membership-record-repro";
            meta = {
              description = "Generate signed membership-record repro artifacts";
            };
          };
          debug-bundle = {
            type = "app";
            program = "${debugBundle}/bin/p2p-vpn-debug-bundle";
            meta = {
              description = "Capture local debug metadata and optional fast-check artifacts";
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
          public-vpn-evidence-check = {
            type = "app";
            program = "${publicVpnEvidenceCheck}/bin/p2p-vpn-public-vpn-evidence-check";
            meta = {
              description = "Validate two-host public VPN repro evidence artifacts";
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
              "$root/docs/user/README.md" \
              "$root/docs/user/quick-start.md" \
              "$root/docs/user/configuration.md" \
              "$root/docs/user/nixos.md" \
              "$root/docs/user/operations.md" \
              "$root/docs/user/public-libp2p.md" \
              "$root/docs/developer/README.md" \
              "$root/docs/developer/architecture.md" \
              "$root/docs/developer/feature-matrix.md" \
              "$root/docs/developer/testing.md" \
              "$root/docs/developer/network-debugging.md" \
              "$root/docs/developer/public-bootstrap-smoke.md" \
              "$root/examples/nixos-mesh/flake.nix" \
              "$root/nix/nixos-module.nix" \
              "$root/scripts/debug-bundle.sh" \
              "$root/scripts/membership-record-repro.sh"
            do
              grep -Fx "$path" entries >/dev/null || {
                echo "release archive missing $path" >&2
                exit 1
              }
            done

            tar -xzf "$archive" "$root/scripts/debug-bundle.sh"
            test -x "$root/scripts/debug-bundle.sh"
            tar -xzf "$archive" "$root/scripts/membership-record-repro.sh"
            test -x "$root/scripts/membership-record-repro.sh"

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
              cargo clippy --all-targets -- -D clippy::correctness -D clippy::suspicious -D clippy::perf
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
            nativeBuildInputs = [ pkgs.jq ];
            execStart = moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.ExecStart;
            execStartNoSocket = moduleEval.config.systemd.services.p2p-vpn-node-b.serviceConfig.ExecStart;
            execStartSettings = moduleEval.config.systemd.services.p2p-vpn-node-c.serviceConfig.ExecStart;
            execStartSecret = moduleEval.config.systemd.services.p2p-vpn-node-d.serviceConfig.ExecStart;
            execStartMinimal = moduleEval.config.systemd.services.p2p-vpn-node-e.serviceConfig.ExecStart;
            execStartPreSecret = builtins.toJSON moduleEval.config.systemd.services.p2p-vpn-node-d.serviceConfig.ExecStartPre;
            execStartPreSecretScript = builtins.readFile (
              builtins.head moduleEval.config.systemd.services.p2p-vpn-node-d.serviceConfig.ExecStartPre
            );
            execStop = builtins.toJSON moduleEval.config.systemd.services.p2p-vpn-node-a.serviceConfig.ExecStop;
            execStopNoSocket = builtins.toJSON moduleEval.config.systemd.services.p2p-vpn-node-b.serviceConfig.ExecStop;
            generatedSettings = builtins.readFile moduleEval.config.environment.etc."p2p-vpn/node-c.json".source;
            generatedMinimalSettings = builtins.readFile moduleEval.config.environment.etc."p2p-vpn/node-e.json".source;
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
            case "$execStartSettings" in
              *"p2p-vpn up --config /etc/p2p-vpn/node-c.json --control-socket /run/p2p-vpn-node-c/control.sock"*) ;;
              *) echo "unexpected settings ExecStart: $execStartSettings" >&2; exit 1 ;;
            esac
            case "$execStartSecret" in
              *"p2p-vpn up --config /run/p2p-vpn-node-d/config.json --control-socket /run/p2p-vpn-node-d/control.sock"*) ;;
              *) echo "unexpected secret ExecStart: $execStartSecret" >&2; exit 1 ;;
            esac
            case "$execStartMinimal" in
              *"p2p-vpn up --config /etc/p2p-vpn/node-e.json --control-socket /run/p2p-vpn-node-e/control.sock"*) ;;
              *) echo "unexpected minimal ExecStart: $execStartMinimal" >&2; exit 1 ;;
            esac
            case "$execStartPreSecret" in
              *"p2p-vpn-node-d-write-config"*) ;;
              *) echo "unexpected secret ExecStartPre: $execStartPreSecret" >&2; exit 1 ;;
            esac
            case "$execStartPreSecretScript" in
              *"--rawfile private_key /run/secrets/p2p-vpn/node-d.key"*"/run/p2p-vpn-node-d/config.json"*) ;;
              *) echo "unexpected secret ExecStartPre script: $execStartPreSecretScript" >&2; exit 1 ;;
            esac
            case "$generatedSettings" in
              *'"name":"nixos-module"'*'"private_key":"BASE64_PRIVATE_KEY"'*'"vpn_ip":"10.44.0.1"'*'"ip":"192.168.0.203"'*'"vpn_ip":"10.44.0.2"'*) ;;
              *) echo "unexpected generated settings: $generatedSettings" >&2; exit 1 ;;
            esac
            printf '%s' "$generatedSettings" > generated-settings.json
            jq -e '
              .network.relay.auto == {
                "max_candidates": 12,
                "max_reservations": 3,
                "retry_interval_seconds": 45
              }
            ' generated-settings.json >/dev/null || {
              echo "generated settings missing auto relay policy: $generatedSettings" >&2
              exit 1
            }
            printf '%s' "$generatedMinimalSettings" > generated-minimal.json
            jq -e '
              keys == ["network", "peers"]
              and (.network | keys == ["name", "private_key"])
              and (.network.name == "nixos-module-minimal")
              and (.network.private_key == "BASE64_PRIVATE_KEY")
              and (.peers == [{"id":"5555555555555555555555555555555555555555555555555555555555555555"}])
            ' generated-minimal.json >/dev/null || {
              echo "minimal generated settings are not compact: $generatedMinimalSettings" >&2
              exit 1
            }
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
          nixos-vm-minimal-lan = nixosVmMesh;
          nixos-vm-mesh = nixosVmMesh;
          nixos-vm-forced-relay = nixosVmForcedRelay;
          nixos-vm-network-move = nixosVmNetworkMove;
          public-relay-repro-structure = pkgs.runCommand "p2p-vpn-public-relay-repro-structure" {
            nativeBuildInputs = [
              publicRelayRepro
            ];
          } ''
            script="$(command -v p2p-vpn-public-relay-repro)"
            test -x "$script"
            grep -Fq 'echo "[git rev-parse HEAD]"' "$script"
            grep -Fq 'git rev-parse HEAD 2>&1 || true' "$script"
            grep -Fq 'echo "[git status --short]"' "$script"
            grep -Fq 'git status --short 2>&1 || true' "$script"
            grep -q 'repro-dcutr-listen-host-a.sh' "$script"
            grep -q 'repro-dcutr-dial-host-b.sh' "$script"
            grep -q 'repro-retry-env.sh' "$script"
            grep -q 'repro-phases.tsv' "$script"
            grep -q 'repro-phase-logs.tsv' "$script"
            grep -q 'phase-logs' "$script"
            grep -q 'tee "$phase_stdout"' "$script"
            grep -q 'tee "$phase_stderr" >&2' "$script"
            grep -q 'write_retry_env' "$script"
            grep -q 'record_phase_result' "$script"
            grep -q 'using supplied public relay candidate' "$script"
            grep -q 'P2P_VPN_REPRO_RELAY_CANDIDATE' "$script"
            grep -q 'repro-summary.json' "$script"
            grep -Fq 'printf "jq . %q\n" "$summary_json"' "$script"
            grep -q 'write_machine_summary' "$script"
            grep -q 'relay_diagnostics' "$script"
            grep -q 'diagnoses=' "$script"
            grep -q 'diagnoses:' "$script"
            grep -q 'accepted_relay_reservations=' "$script"
            grep -q 'P2P_VPN_REPRO_MEMBERSHIP_DHT' "$script"
            grep -q 'public-membership-dht-bootstrap-check.json' "$script"
            grep -q -- '--require-membership-records' "$script"
            grep -q 'membership_dht' "$script"
            grep -q 'public-vpn-host-a.json' "$script"
            grep -q 'public-vpn-host-b.json' "$script"
            grep -q 'public-vpn-host-a-relay-reservation-check.json' "$script"
            grep -q 'public-vpn-host-b-relay-reservation-check.json' "$script"
            grep -q 'public-vpn-host-a.relay-check.json' "$script"
            grep -q 'public-vpn-host-b.relay-check.json' "$script"
            grep -Fq '.network.listen_addresses = ["/ip4/0.0.0.0/tcp/0"]' "$script"
            grep -q 'P2P_VPN_REPRO_REQUIRE_VPN_RELAY_RESERVATIONS' "$script"
            grep -q 'P2P_VPN_REPRO_REQUIRE_DCUTR' "$script"
            grep -q 'DCUtR success evidence check disabled' "$script"
            grep -q 'P2P_VPN_REPRO_PHASE_TIMEOUT_SECONDS' "$script"
            grep -q 'declare -F "$1"' "$script"
            grep -q 'timeout --kill-after=5s' "$script"
            grep -q 'checking generated two-host VPN relay reservations' "$script"
            grep -q -- '--listen-address /ip4/0.0.0.0/tcp/0' "$script"
            grep -q 'write_bootstrap_summary_json' "$script"
            grep -q -- '--write-host-a-config' "$script"
            grep -q -- '--write-host-b-config' "$script"
            grep -q -- '--require-relay-reservations' "$script"
            grep -q 'vpn_host_a_config' "$script"
            grep -q 'P2P_VPN_REPRO_VPN_HOST_A_ROUTE' "$script"

            touch $out
          '';
          debug-bundle-structure = pkgs.runCommand "p2p-vpn-debug-bundle-structure" {
            nativeBuildInputs = [
              debugBundle
              pkgs.jq
            ];
          } ''
            artifact_dir="$TMPDIR/debug-bundle"
            cd ${./.}
            P2P_VPN_DEBUG_BUNDLE_DIR="$artifact_dir" p2p-vpn-debug-bundle > "$TMPDIR/stdout" 2> "$TMPDIR/stderr"
            test -s "$artifact_dir/debug-summary.json"
            jq -e '.schema_version == 1 and .artifacts.daemon_packet_plane_summary != null' "$artifact_dir/debug-summary.json"
            grep -Fq '[p2p-vpn]' "$artifact_dir/debug-toolchain.txt"
            grep -Fq '/bin/p2p-vpn' "$artifact_dir/debug-toolchain.txt"
            grep -Fq 'daemon-packet-plane-summary.txt' "$artifact_dir/debug-summary.txt"
            grep -Fq 'enabled=false' "$artifact_dir/daemon-packet-plane-summary.txt"
            touch $out
          '';
          public-vpn-repro-structure = pkgs.runCommand "p2p-vpn-public-vpn-repro-structure" {
            nativeBuildInputs = [
              package
              publicVpnRepro
            ];
          } ''
            config="$TMPDIR/public-vpn-repro-config.json"
            host_a_config="$TMPDIR/public-vpn-repro-host-a-config.json"
            host_b_config="$TMPDIR/public-vpn-repro-host-b-config.json"
            artifacts="$TMPDIR/public-vpn-repro"
            public_relay_artifacts="$TMPDIR/public-relay-repro"
            mkdir -p "$public_relay_artifacts"
            p2p-vpn init-config \
              --output "$host_a_config" \
              --network public-vpn-repro-structure \
              --interface hs-repro0 \
              --local-route 10.42.0.1/32 \
              --disable-mdns \
              --disable-kademlia \
              --force
            p2p-vpn init-config \
              --output "$host_b_config" \
              --network public-vpn-repro-structure \
              --interface hs-repro0 \
              --local-route 10.42.0.2/32 \
              --disable-mdns \
              --disable-kademlia \
              --force
            cp "$host_a_config" "$public_relay_artifacts/public-vpn-host-a.json"
            cp "$host_b_config" "$public_relay_artifacts/public-vpn-host-b.json"

            P2P_VPN_VPN_REPRO_DIR="$artifacts" \
              P2P_VPN_VPN_REPRO_PUBLIC_RELAY_DIR="$public_relay_artifacts" \
              p2p-vpn-public-vpn-repro

            for script in \
              "$artifacts/vpn-repro-host-a.sh" \
              "$artifacts/vpn-repro-host-b.sh" \
              "$artifacts/vpn-repro-collect.sh" \
              "$artifacts/vpn-repro-shutdown.sh" \
              "$artifacts/vpn-repro-commands.sh"
            do
              test -x "$script"
              bash -n "$script"
            done

            grep -q '^trap on_exit EXIT$' "$artifacts/vpn-repro-host-a.sh"
            grep -q 'capture_host_network "$host_network_before"' "$artifacts/vpn-repro-host-a.sh"
            grep -q 'capture_host_network "$host_network_after"' "$artifacts/vpn-repro-host-a.sh"
            grep -q 'capture_final_artifacts' "$artifacts/vpn-repro-host-a.sh"
            grep -q 'record_status daemon_health "$health_status"' "$artifacts/vpn-repro-host-a.sh"
            grep -q "config=$public_relay_artifacts/public-vpn-host-a.json" "$artifacts/vpn-repro-host-a.sh"
            grep -q "config=$public_relay_artifacts/public-vpn-host-b.json" "$artifacts/vpn-repro-host-b.sh"
            grep -q 'ping_target=10.42.0.2' "$artifacts/vpn-repro-host-a.sh"
            grep -q 'ping_target=10.42.0.1' "$artifacts/vpn-repro-host-b.sh"
            grep -q 'P2P_VPN_VPN_REPRO_HOST_A_CONFIG' "$artifacts/vpn-repro-commands.sh"
            grep -q 'P2P_VPN_VPN_REPRO_HOST_B_CONFIG' "$artifacts/vpn-repro-commands.sh"
            grep -q 'P2P_VPN_VPN_REPRO_HOST_A_PING_TARGET' "$artifacts/vpn-repro-commands.sh"
            grep -q 'P2P_VPN_VPN_REPRO_HOST_B_PING_TARGET' "$artifacts/vpn-repro-commands.sh"
            grep -q 'daemon_log_tail=' "$artifacts/vpn-repro-summary.txt"
            grep -q 'result_log=' "$artifacts/vpn-repro-summary.txt"
            grep -q 'host_a_config=' "$artifacts/vpn-repro-summary.txt"
            grep -q 'host_b_config=' "$artifacts/vpn-repro-summary.txt"
            grep -q 'host_a_ping_target=10.42.0.2' "$artifacts/vpn-repro-summary.txt"
            grep -q 'host_b_ping_target=10.42.0.1' "$artifacts/vpn-repro-summary.txt"
            grep -q 'evidence_json=' "$artifacts/vpn-repro-summary.txt"
            grep -q 'vpn-repro-evidence.json' "$artifacts/vpn-repro-summary.txt"
            grep -q 'write_evidence_summary' "$artifacts/vpn-repro-host-a.sh"
            grep -q 'P2P_VPN_VPN_REPRO_WRITE_EVIDENCE_ONLY' "$artifacts/vpn-repro-host-a.sh"
            grep -q 'p2p_vpn_path_promotions_to_direct' "$artifacts/vpn-repro-host-a.sh"
            grep -q 'jq . ' "$artifacts/vpn-repro-commands.sh"
            grep -q 'host_network_before=' "$artifacts/vpn-repro-summary.txt"
            grep -q 'host_network_after=' "$artifacts/vpn-repro-summary.txt"
            grep -q 'host_network_after=' "$artifacts/vpn-repro-collect.sh"
            grep -q '^\[git rev-parse HEAD\]$' "$artifacts/vpn-repro-metadata.txt"
            grep -q '^\[git status --short\]$' "$artifacts/vpn-repro-metadata.txt"

            touch $out
          '';
          public-vpn-repro-evidence-structure = pkgs.runCommand "p2p-vpn-public-vpn-repro-evidence-structure" {
            nativeBuildInputs = [
              package
              publicVpnRepro
              pkgs.jq
            ];
          } ''
            config="$TMPDIR/public-vpn-repro-config.json"
            artifacts="$TMPDIR/public-vpn-repro"
            mkdir -p "$artifacts"

            p2p-vpn init-config \
              --output "$config" \
              --network public-vpn-repro-evidence-structure \
              --interface hs-repro0 \
              --disable-mdns \
              --disable-kademlia \
              --force

            cat > "$artifacts/daemon-health.txt" <<'EOF'
daemon_health_ready true
daemon_health_check daemon_running ok control socket responded
daemon_health_check validated_peers ok 1 peers validated
daemon_health_check supported_paths ok 1 peers have supported paths
daemon_health_check packet_plane_session ok 1 sessions active
EOF
            cat > "$artifacts/vpn-repro-result.txt" <<'EOF'
1970-01-01T00:00:00Z daemon_health exit=0
1970-01-01T00:00:01Z ping exit=0
EOF
            cat > "$artifacts/daemon-status-prometheus-final.txt" <<'EOF'
p2p_vpn_path_promotions_to_direct 1
p2p_vpn_dcutr_successes 2
p2p_vpn_direct_connections_established 3
p2p_vpn_relayed_connections_established 4
p2p_vpn_path_peers_with_supported_path 5
p2p_vpn_packet_plane_sessions 6
p2p_vpn_packet_plane_quic_sessions 7
p2p_vpn_path_healthy_direct_quic_datagram_paths 8
p2p_vpn_path_healthy_direct_quic_stream_paths 9
p2p_vpn_path_healthy_direct_tcp_stream_paths 10
p2p_vpn_path_healthy_relay_paths 11
EOF
            cat > "$artifacts/daemon-paths-final.json" <<'EOF'
{
  "lines": [
    "peer node-b direct true protocol quic",
    "peer node-b circuit relay true reservation active"
  ]
}
EOF
            cat > "$artifacts/daemon-state-final.json" <<'EOF'
{
  "lines": [
    "peer node-b validated true",
    "packet-plane session node-b active"
  ]
}
EOF

            P2P_VPN_VPN_REPRO_DIR="$artifacts" \
              P2P_VPN_VPN_REPRO_CONFIG="$config" \
              P2P_VPN_VPN_REPRO_PING_TARGET=10.42.0.2 \
              P2P_VPN_VPN_REPRO_EVIDENCE_ONLY=1 \
              p2p-vpn-public-vpn-repro

            test -s "$artifacts/vpn-repro-evidence.json"
            jq -e '
              .schema_version == 1
              and (.generated_utc | type == "string")
              and .artifact_dir == $artifact_dir
              and .config == $config
              and .ping_target == "10.42.0.2"
              and .health_ready == true
              and .ping_succeeded == true
              and .ping_exit == 0
              and .metrics.path_promotions_to_direct == 1
              and .metrics.dcutr_successes == 2
              and .metrics.direct_connections_established == 3
              and .metrics.relayed_connections_established == 4
              and .metrics.peers_with_supported_path == 5
              and .metrics.packet_plane_sessions == 6
              and .metrics.packet_plane_quic_sessions == 7
              and .metrics.healthy_direct_quic_datagram_paths == 8
              and .metrics.healthy_direct_quic_stream_paths == 9
              and .metrics.healthy_direct_tcp_stream_paths == 10
              and .metrics.healthy_relay_paths == 11
              and (.path_evidence.direct_lines | length) == 1
              and (.path_evidence.relay_lines | length) == 1
              and (.health_lines | length) == 5
              and (.result_lines | length) == 2
              and (.final_state_lines | length) == 2
              and (.final_path_lines | length) == 2
            ' \
              --arg artifact_dir "$artifacts" \
              --arg config "$config" \
              "$artifacts/vpn-repro-evidence.json"

            touch $out
          '';
          public-vpn-evidence-check = pkgs.runCommand "p2p-vpn-public-vpn-evidence-check" {
            nativeBuildInputs = [
              publicVpnEvidenceCheck
              pkgs.jq
            ];
          } ''
            host_a="$TMPDIR/host-a-evidence.json"
            host_b="$TMPDIR/host-b-evidence.json"
            host_b_no_relay="$TMPDIR/host-b-no-relay-evidence.json"
            report="$TMPDIR/evidence-report.json"

            cat > "$host_a" <<'EOF'
{
  "schema_version": 1,
  "artifact_dir": "/tmp/host-a",
  "health_ready": true,
  "ping_succeeded": true,
  "ping_exit": 0,
  "metrics": {
    "dcutr_successes": 1,
    "direct_connections_established": 1,
    "relayed_connections_established": 1,
    "peers_with_supported_path": 1,
    "packet_plane_sessions": 1,
    "packet_plane_quic_sessions": 1,
    "healthy_direct_quic_datagram_paths": 1,
    "healthy_direct_quic_stream_paths": 0,
    "healthy_direct_tcp_stream_paths": 0,
    "healthy_relay_paths": 1
  },
  "path_evidence": {
    "direct_lines": ["peer b selected_path direct_quic_datagram"],
    "relay_lines": ["peer b selected_path circuit relay"]
  }
}
EOF
            cat > "$host_b" <<'EOF'
{
  "schema_version": 1,
  "artifact_dir": "/tmp/host-b",
  "health_ready": true,
  "ping_succeeded": true,
  "ping_exit": 0,
  "metrics": {
    "dcutr_successes": 1,
    "direct_connections_established": 1,
    "relayed_connections_established": 1,
    "peers_with_supported_path": 1,
    "packet_plane_sessions": 1,
    "packet_plane_quic_sessions": 1,
    "healthy_direct_quic_datagram_paths": 1,
    "healthy_direct_quic_stream_paths": 0,
    "healthy_direct_tcp_stream_paths": 0,
    "healthy_relay_paths": 1
  },
  "path_evidence": {
    "direct_lines": ["peer a selected_path direct_quic_datagram"],
    "relay_lines": ["peer a selected_path circuit relay"]
  }
}
EOF
            jq '.metrics.relayed_connections_established = 0
              | .metrics.healthy_relay_paths = 0
              | .path_evidence.relay_lines = []' \
              "$host_b" > "$host_b_no_relay"

            p2p-vpn-public-vpn-evidence-check \
              --host-a "$host_a" \
              --host-b "$host_b" \
              --require-direct \
              --require-relay \
              --require-dcutr \
              --require-quic-session \
              --write-report "$report" \
              | tee "$TMPDIR/pass-output.txt"

            jq -e '
              .ok == true
              and .requirements.require_direct == true
              and .requirements.require_relay == true
              and .requirements.require_dcutr == true
              and .requirements.require_quic_session == true
              and (.checks | length) == 16
            ' "$report"
            grep -q '^public VPN evidence check: ok$' "$TMPDIR/pass-output.txt"

            if p2p-vpn-public-vpn-evidence-check \
              --host-a "$host_a" \
              --host-b "$host_b_no_relay" \
              --require-relay \
              > "$TMPDIR/fail-output.txt" 2> "$TMPDIR/fail-error.txt"
            then
              echo "missing relay evidence was accepted" >&2
              exit 1
            fi
            grep -q 'failed: host_b.relay_evidence' "$TMPDIR/fail-error.txt"

            touch $out
          '';
        } // lib.optionalAttrs pkgs.stdenv.isLinux {
          namespace-smoke-preflighted = namespaceSmokePreflighted;
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
            pkgs.jq
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
