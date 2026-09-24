{
  description = "Per-core Curve Optimizer tuner for Zen 5 desktop CPUs";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      inherit (pkgs) lib;
      shycler = pkgs.buildGoModule {
        pname = "shycler";
        version = "0.1.0";
        src = lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions [
            ./go.mod
            ./go.sum
            ./.golangci.yml
            ./cmd
            ./internal
          ];
        };
        vendorHash = "sha256-pbA/AlBz3cQYRTMnQ/qBPcinYOKokrBLNhkbRTq54gE=";
        meta = {
          description = "Per-core Curve Optimizer tuner for Zen 5 desktop CPUs";
          mainProgram = "shycler";
          platforms = [ system ];
        };
      };
    in
    {
      packages.${system}.default = shycler;
      devShells.${system}.default = pkgs.mkShell {
        packages = [
          pkgs.go
          pkgs.gopls
          pkgs.golangci-lint
        ];
      };
      checks.${system} = {
        package = shycler;
        lint = shycler.overrideAttrs (old: {
          pname = "shycler-lint";
          nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.golangci-lint ];
          buildPhase = ''
            runHook preBuild
            export HOME=$TMPDIR GOLANGCI_LINT_CACHE=$TMPDIR/golangci-lint
            golangci-lint run ./...
            runHook postBuild
          '';
          doCheck = false;
          installPhase = "mkdir -p $out";
          dontFixup = true;
        });
      };
      formatter.${system} = pkgs.treefmt.withConfig {
        runtimeInputs = [
          pkgs.nixfmt
          pkgs.go
        ];
        settings = {
          on-unmatched = "info";
          tree-root-file = "flake.nix";
          formatter.nixfmt = {
            command = "nixfmt";
            includes = [ "*.nix" ];
          };
          formatter.gofmt = {
            command = "gofmt";
            options = [ "-w" ];
            includes = [ "*.go" ];
          };
        };
      };
    };
}
