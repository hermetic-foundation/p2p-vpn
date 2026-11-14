{
  pkgs,
  lib,
  module,
  system,
  packages,
}:
let
  evaluate =
    package:
    (lib.nixosSystem {
      inherit system;
      modules = [
        module
        {
          system.stateVersion = "25.11";
          services.p2p-vpn = {
            inherit package;
            instances = {
              alpha = {
                enable = true;
                dns.enable = true;
              };
              beta.enable = true;
            };
          };
        }
      ];
    }).config;
  ordinary = evaluate packages.default;
  trial = evaluate packages.demand-lifecycle-trial;
  executable = "${packages.demand-lifecycle-trial}/bin/p2p-vpn";
  contracts = {
    ordinaryRemainsPassive = packages.default.cargoBuildFeatures == [ ];
    explicitTrial = trial.services.p2p-vpn.package.cargoBuildFeatures == [ "demand-lifecycle-trial" ];
    instanceCommands =
      lib.all
        (
          name:
          lib.hasPrefix "${executable} up " trial.systemd.services."p2p-vpn-${name}".serviceConfig.ExecStart
        )
        [
          "alpha"
          "beta"
        ];
    dnsGuardCommand = lib.hasPrefix "${executable} dns guard " trial.systemd.services.p2p-vpn-dns-guard.serviceConfig.ExecStart;
    cliPackage = lib.any (
      package: package.outPath == packages.demand-lifecycle-trial.outPath
    ) trial.environment.systemPackages;
    sameGeneratedConfiguration =
      ordinary.services.p2p-vpn.generatedConfigs == trial.services.p2p-vpn.generatedConfigs;
    sameInterfaces =
      ordinary.services.p2p-vpn.effectiveInterfaces == trial.services.p2p-vpn.effectiveInterfaces;
    sameIdentityFiles = ordinary.services.p2p-vpn.identityFiles == trial.services.p2p-vpn.identityFiles;
    sameMembershipFiles =
      ordinary.services.p2p-vpn.membershipStateFiles == trial.services.p2p-vpn.membershipStateFiles;
    samePairingFiles =
      ordinary.services.p2p-vpn.pairingStateFiles == trial.services.p2p-vpn.pairingStateFiles;
  };
  verified = lib.mapAttrs (
    name: passed:
    assert lib.assertMsg passed "lifecycle trial module evaluation failed: ${name}";
    passed
  ) contracts;
in
pkgs.writeText "p2p-vpn-lifecycle-trial-module.json" (builtins.toJSON verified)
