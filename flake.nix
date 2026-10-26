{
  description = "Self-hosted Docker image update checker";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      forAllSystems =
        function: nixpkgs.lib.genAttrs systems (system: function system nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (
        system: pkgs: {
          default = pkgs.callPackage ./nix/package.nix { };
          hublot = self.packages.${system}.default;
        }
      );

      devShells = forAllSystems (
        system: pkgs: {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              rustfmt
              clippy
              pkg-config
              sqlite
            ];
          };
        }
      );

      nixosModules.default = import ./nix/module.nix self;

      checks = forAllSystems (
        system: _: {
          default = self.packages.${system}.default;
        }
      );

      formatter = forAllSystems (_: pkgs: pkgs.nixfmt);
    };
}
