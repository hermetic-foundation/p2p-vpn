{
  pkgs,
  lib,
  module,
  system,
}:
let
  config =
    (lib.nixosSystem {
      inherit system;
      modules = [
        module
        {
          system.stateVersion = "25.11";
          services.p2p-vpn.instances = {
            minimal.enable = true;
            partial = {
              enable = true;
              autoRelay.maxCandidates = 8;
            };
            redundant = {
              enable = true;
              autoRelay.maxReservations = 3;
            };
            disabled = {
              enable = true;
              autoRelay = {
                maxCandidates = 0;
                maxReservations = 0;
              };
            };
          };
        }
      ];
    }).config;
  generated = config.services.p2p-vpn.generatedConfigs;
  contracts = {
    minimalUsesRuntimeDefault = !(generated.minimal.network ? relay);
    partialUsesOneReservation = generated.partial.network.relay.auto.max_reservations == 1;
    partialPreservesCandidateOverride = generated.partial.network.relay.auto.max_candidates == 8;
    explicitRedundancy = generated.redundant.network.relay.auto.max_reservations == 3;
    explicitDisable = generated.disabled.network.relay.auto.max_reservations == 0;
    retryUnchanged = generated.partial.network.relay.auto.retry_interval_seconds == 30;
    valid = lib.all (
      entry: entry.assertion || !(lib.hasPrefix "services.p2p-vpn" entry.message)
    ) config.assertions;
  };
  verified = lib.mapAttrs (
    name: passed:
    assert lib.assertMsg passed "automatic relay defaults evaluation failed: ${name}";
    passed
  ) contracts;
in
pkgs.writeText "p2p-vpn-relay-defaults-eval.json" (builtins.toJSON verified)
