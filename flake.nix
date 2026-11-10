{
  # The development shell that agents, the owner and CI use alike: `nix develop`, then
  # `scripts/check.sh`. CONTRIBUTING.md says what each tool is for.
  description = "Magnolia: a digital audio workstation";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # Rust's own builds of its toolchains, stable and nightly, by version or date (ADR-0013).
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      systems = [ "x86_64-linux" ];
      forAllSystems =
        f:
        nixpkgs.lib.genAttrs systems (
          system:
          f (
            import nixpkgs {
              inherit system;
              overlays = [ rust-overlay.overlays.default ];
            }
          )
        );
    in
    {
      devShells = forAllSystems (
        pkgs:
        let
          # The nightly that RealtimeSanitizer, Miri and cargo fuzz need, and nothing else: it never
          # builds what users run (ADR-0013). Bumped on purpose, at a stage's end or when a check
          # needs it.
          nightly = pkgs.rust-bin.nightly."2026-11-06".minimal.override {
            extensions = [
              "clippy"
              "miri"
              "rust-src"
            ];
          };
          # `nightly <command>` runs a command with the nightly first on the path, as
          # `cargo +nightly` does under rustup: `nightly cargo miri test`.
          nightlyCommand = pkgs.writeShellScriptBin "nightly" ''
            PATH=${nightly}/bin:$PATH exec "$@"
          '';
        in
        {
          default = pkgs.mkShell {
            packages = [
              # The stable toolchain pinned in rust-toolchain.toml, the one rustup users get too.
              (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
              nightlyCommand
              pkgs.cargo-deny
              pkgs.cargo-fuzz
              pkgs.lychee
            ];
          };
        }
      );
    };
}
