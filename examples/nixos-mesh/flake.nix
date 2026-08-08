{
  description = "Example two-node NixOS p2p-vpn mesh deployment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    p2p-vpn.url = "github:hermetic-foundation/p2p-vpn";
  };

  outputs =
    {
      nixpkgs,
      p2p-vpn,
      ...
    }:
    let
      system = "x86_64-linux";
      commonModule =
        { ... }:
        {
          imports = [ p2p-vpn.nixosModules.default ];
          system.stateVersion = "25.11";
        };
    in
    {
      nixosConfigurations.node-a = nixpkgs.lib.nixosSystem {
        inherit system;
        modules = [
          commonModule
          (
            { ... }:
            {
              networking.hostName = "node-a";
              services.p2p-vpn.instances.node-a = {
                enable = true;
                networkName = "lab";
                localPeer = "NODE_A_PEER_ID";
                privateKeyFile = "/run/secrets/p2p-vpn/node-a.key";
                routes = [ "10.44.0.1/32" ];
                peers."NODE_B_PEER_ID".routes = [ "10.44.0.2/32" ];
                metricsIntervalSeconds = 10;
                openFirewall = true;
              };
            }
          )
        ];
      };

      nixosConfigurations.node-b = nixpkgs.lib.nixosSystem {
        inherit system;
        modules = [
          commonModule
          (
            { ... }:
            {
              networking.hostName = "node-b";
              services.p2p-vpn.instances.node-b = {
                enable = true;
                networkName = "lab";
                localPeer = "NODE_B_PEER_ID";
                privateKeyFile = "/run/secrets/p2p-vpn/node-b.key";
                routes = [ "10.44.0.2/32" ];
                peers."NODE_A_PEER_ID".routes = [ "10.44.0.1/32" ];
                metricsIntervalSeconds = 10;
                openFirewall = true;
              };
            }
          )
        ];
      };
    };
}
