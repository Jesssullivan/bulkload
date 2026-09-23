{
  description = "Bulkload: live-host estate mover (Rust agent, wire protocol, benchmark) and its CI validation";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "aarch64-darwin" "x86_64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs { inherit system; }));
    in {
      devShells = forAllSystems (pkgs: {
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
            # Rust workspace gates (just rust-check). The toolchain comes from
            # the pinned nixpkgs so the flake keeps a single audited input.
            cargo
            clippy
            rustc
            rustfmt
          ];
        };
      });
    };
}
