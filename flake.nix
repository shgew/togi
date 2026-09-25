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
      systems = [
        "x86_64-linux"
        "aarch64-darwin"
      ];

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
            version = lib.fileContents ./version.txt;
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./go.mod
                ./go.sum
                ./.golangci.yml
                ./version.txt
                ./version.go
                ./cmd
                ./internal
                ./tools
              ];
            };
            vendorHash = "sha256-XXgXzv6MARTUse1lf4RAaMp9xg8FfysaPMM7wq5zdlw=";
            nativeCheckInputs = lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.util-linux ];
            ldflags = [
              "-X code.marleb.org/shgew/shycler.rev=${inputs.self.shortRev or inputs.self.dirtyShortRev or "dev"}"
            ];
            subPackages = [ "cmd/shycler" ];
            checkPhase = ''
              runHook preCheck
              export GOFLAGS=''${GOFLAGS//-trimpath/}
              go test -p $NIX_BUILD_CORES ./...
              runHook postCheck
            '';
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
              pkgs.just
              pkgs.nixfmt
              pkgs.govulncheck
            ];
            SHYCLER_DEV_SHELL = "1";
          };

          checks = {
            package = config.packages.default;
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
          }
          // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            vm = import ./nix/vm-test.nix {
              inherit pkgs;
              inherit (inputs) self;
            };
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
