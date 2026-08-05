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
            metadata="$artifact_dir/repro-metadata.txt"
            host_network="$artifact_dir/repro-host-network.txt"
            commands="$artifact_dir/repro-commands.sh"
            summary="$artifact_dir/repro-summary.txt"
            scan_timeout="''${P2P_VPN_RELAY_SCAN_TIMEOUT_SECONDS:-30}"
            candidate_timeout="''${P2P_VPN_RELAY_CANDIDATE_TIMEOUT_SECONDS:-45}"
            max_candidates="''${P2P_VPN_RELAY_MAX_CANDIDATES:-8}"
            max_validation="''${P2P_VPN_RELAY_MAX_VALIDATION_CANDIDATES:-8}"
            base_config="''${P2P_VPN_REPRO_BASE_CONFIG:-}"
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
                echo "[ip route show]"
                ip route show || true
                echo
                echo "[ip -6 route show]"
                ip -6 route show || true
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
                printf "export P2P_VPN_RELAY_SCAN_TIMEOUT_SECONDS=%q\n" "$scan_timeout"
                printf "export P2P_VPN_RELAY_CANDIDATE_TIMEOUT_SECONDS=%q\n" "$candidate_timeout"
                printf "export P2P_VPN_RELAY_MAX_CANDIDATES=%q\n" "$max_candidates"
                printf "export P2P_VPN_RELAY_MAX_VALIDATION_CANDIDATES=%q\n" "$max_validation"
                echo
                echo "p2p-vpn relay-scan \\"
                echo "  --ipfs-bootstrap-peers \\"
                printf "  --timeout-seconds %q \\\\\n" "$scan_timeout"
                printf "  --max-candidates %q \\\\\n" "$max_candidates"
                printf "  --write-candidates %q \\\\\n" "$candidates"
                printf "  --write-report %q \\\\\n" "$scan_report"
                echo "  --force"
                echo
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
              } > "$commands"
              chmod +x "$commands"
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
                "  first_error=" + (
                  (
                    [(.candidates // [])[].error, (.peer_results // [])[].last_error]
                    | map(select(. != null and . != ""))
                    | first
                  ) // "none"
                )
              ' "$report" >> "$summary"
            }

            write_summary() {
              {
                echo "p2p-vpn public relay repro summary"
                echo "artifact_dir=$artifact_dir"
                echo "metadata=$metadata"
                echo "host_network=$host_network"
                echo "commands=$commands"
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
            }

            run_phase() {
              phase="$1"
              shift
              echo "$phase" >&2
              set +e
              "$@"
              phase_status="$?"
              set -e
              if [[ "$phase_status" -ne 0 ]]; then
                echo "$phase failed with exit status $phase_status" >&2
                status=1
              fi
              phase_results+=("$phase status=$phase_status")
            }

            echo "writing public relay repro artifacts to $artifact_dir" >&2
            write_metadata
            write_host_network
            write_commands
            run_phase "scanning IPFS-compatible bootstrap peers for public relay candidates" \
              p2p-vpn relay-scan \
              --ipfs-bootstrap-peers \
              --timeout-seconds "$scan_timeout" \
              --max-candidates "$max_candidates" \
              --write-candidates "$candidates" \
              --write-report "$scan_report" \
              --force

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
            write_summary
            echo "metadata: $metadata" >&2
            echo "host network: $host_network" >&2
            echo "replay commands: $commands" >&2
            echo "summary: $summary" >&2
            exit "$status"
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

            machine.succeed("systemctl stop p2p-vpn-smoke.service")
            machine.wait_until_fails("test -S /run/p2p-vpn-smoke/control.sock")
            machine.succeed("test \"$(systemctl show p2p-vpn-smoke.service -p Result --value)\" = success")
          '';
        };
      in
      {
        packages = {
          default = package;

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
        };

        apps = {
          default = {
            type = "app";
            program = "${self.packages.${system}.default}/bin/p2p-vpn";
            meta = {
              description = "Run the p2p-vpn CLI";
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
            test "$tcpPorts" = '[4001]'
            test "$udpPorts" = '[4001]'
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
