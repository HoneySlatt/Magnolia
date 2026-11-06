{
  # The development shell that agents, the owner and CI use alike: `nix develop`, then
  # `scripts/check.sh`. It holds lychee, which checks the links between Markdown files; the Rust
  # toolchain arrives with the workspace (M1.1 of docs/PLAN.md).
  description = "Magnolia: a digital audio workstation";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      systems = [ "x86_64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [ pkgs.lychee ];
        };
      });
    };
}
