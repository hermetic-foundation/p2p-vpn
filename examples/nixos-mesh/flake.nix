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
                configFile = "/etc/p2p-vpn/node-a.json";
                metricsIntervalSeconds = 10;
                openFirewall = true;
                tcpPorts = [ 4001 ];
                udpPorts = [ 4001 ];
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
                configFile = "/etc/p2p-vpn/node-b.json";
                metricsIntervalSeconds = 10;
                openFirewall = true;
                tcpPorts = [ 4001 ];
                udpPorts = [ 4001 ];
              };
            }
          )
        ];
      };
    };
}
