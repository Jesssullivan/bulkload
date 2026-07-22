{
  description = "Manifest-first repository and agent-context migration tooling";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "aarch64-darwin" "x86_64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs { inherit system; }));
    in {
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            bazelisk
            git
            gh
            gitleaks
            just
            (python312.withPackages (pythonPackages: [ pythonPackages.pyyaml ]))
            rsync
            ruff
            shellcheck
          ];
        };
      });
    };
}
