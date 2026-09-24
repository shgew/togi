{
  description = "Per-core Curve Optimizer tuner for Zen 5 desktop CPUs";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [ "x86_64-linux" ];

      flake.nixosModules.default = import ./nix/module.nix { inherit (inputs) self; };

      perSystem =
        {
          config,
          lib,
          pkgs,
          system,
          ...
        }:
        {
          packages.default = pkgs.buildGoModule {
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
            nativeCheckInputs = [ pkgs.util-linux ];
            meta = {
              description = "Per-core Curve Optimizer tuner for Zen 5 desktop CPUs";
              mainProgram = "shycler";
              platforms = [ system ];
            };
          };

          devShells.default = pkgs.mkShell {
            packages = [
              pkgs.go
              pkgs.gopls
              pkgs.golangci-lint
            ];
          };

          checks = {
            package = config.packages.default;
            vm = import ./nix/vm-test.nix {
              inherit pkgs;
              inherit (inputs) self;
            };
            lint = config.packages.default.overrideAttrs (old: {
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

          formatter = pkgs.treefmt.withConfig {
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
    };
}
