{
  description = "Minimal external NixOS consumer of p2p-vpn";

  inputs = {
    p2p-vpn.url = "github:hermetic-foundation/p2p-vpn";
    nixpkgs.follows = "p2p-vpn/nixpkgs";
  };

  outputs =
    {
      nixpkgs,
      p2p-vpn,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      systemFor =
        system:
        nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [
            p2p-vpn.nixosModules.default
            {
              boot.loader.grub.devices = [ "nodev" ];
              fileSystems."/" = {
                device = "/dev/disk/by-label/nixos";
                fsType = "ext4";
              };
              system.stateVersion = "25.11";
              services.p2p-vpn.instances.lab.enable = true;
            }
          ];
        };
    in
    {
      nixosConfigurations = builtins.listToAttrs (
        map (system: {
          name = "consumer-${system}";
          value = systemFor system;
        }) systems
      );
    };
}
