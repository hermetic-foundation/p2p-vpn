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
        tunE2e = pkgs.writeShellApplication {
          name = "p2p-vpn-tun-e2e";
          runtimeInputs = [
            cargo
            rust
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
            scan_timeout="''${P2P_VPN_RELAY_SCAN_TIMEOUT_SECONDS:-30}"
            candidate_timeout="''${P2P_VPN_RELAY_CANDIDATE_TIMEOUT_SECONDS:-45}"
            max_candidates="''${P2P_VPN_RELAY_MAX_CANDIDATES:-8}"
            max_validation="''${P2P_VPN_RELAY_MAX_VALIDATION_CANDIDATES:-8}"
            status=0

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
            }

            echo "writing public relay repro artifacts to $artifact_dir" >&2
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
                --relay-candidates-file "$candidates" \
                --timeout-seconds "$candidate_timeout" \
                --max-validation-candidates "$max_validation" \
                --write-report "$relay_report" \
                --write-config "$relay_config" \
                --force

              run_phase "probing candidates for DCUtR success evidence" \
                p2p-vpn relay-check \
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
