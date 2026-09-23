{
  description = "Bulkload: live-host estate mover (Rust agent, wire protocol, benchmark) and its CI validation";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      systems = [ "aarch64-darwin" "x86_64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system:
        f (import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        }));
    in {
      devShells = forAllSystems (pkgs:
        let
          # The toolchain channel and components come from rust-toolchain.toml,
          # so cargo in CI and in a local shell resolve the same compiler.
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        in {
          default = pkgs.mkShell {
            packages = with pkgs; [
              actionlint
              bazelisk
              git
              gh
              gitleaks
              just
              (python312.withPackages (pythonPackages: [ pythonPackages.pyyaml ]))
              ruff
              shellcheck
              rustToolchain
            ];
          };
        });
    };
}
