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

      flake.nixosModules.default = import ./nix/module.nix { inherit (inputs.self) packages; };

      perSystem =
        {
          config,
          lib,
          pkgs,
          system,
          ...
        }:
        let
          testPhase = flags: ''
            runHook preCheck
            export GOFLAGS=''${GOFLAGS//-trimpath/}
            go test -p $NIX_BUILD_CORES ${flags} -shuffle=on ${lib.optionalString pkgs.stdenv.hostPlatform.isLinux "-tags integration"} ./...
            runHook postCheck
          '';
        in
        {
          packages.default = pkgs.buildGo127Module {
            pname = "togi";
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
            vendorHash = "sha256-OGYOqVtPseV1QvWhbVlQfXPcuTGIWne3Fk7JYtee1ak=";
            nativeCheckInputs = lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.util-linux ];
            ldflags = [
              "-X github.com/shgew/togi.rev=${inputs.self.shortRev or inputs.self.dirtyShortRev or "dev"}"
            ];
            subPackages = [ "cmd/togi" ];
            checkPhase = testPhase "";
            meta = {
              description = "Per-core Curve Optimizer tuner for Zen 5 desktop CPUs";
              license = lib.licenses.mit;
              mainProgram = "togi";
              platforms = [ system ];
            };
          };

          devShells.default = pkgs.mkShell {
            packages = [
              pkgs.go_1_27
              pkgs.gh
              pkgs.gopls
              pkgs.golangci-lint
              pkgs.just
              pkgs.nixfmt
            ];
            TOGI_DEV_SHELL = "1";
          };

          checks = {
            package = config.packages.default;
            lint = config.packages.default.overrideAttrs (old: {
              pname = "togi-lint";
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
            fmt =
              pkgs.runCommand "togi-fmt"
                {
                  nativeBuildInputs = [ config.formatter ];
                  src = lib.fileset.toSource {
                    root = ./.;
                    fileset = lib.fileset.unions [
                      ./flake.nix
                      ./justfile
                      (lib.fileset.fileFilter (f: f.hasExt "go" || f.hasExt "nix") ./.)
                    ];
                  };
                }
                ''
                  cp -r "$src" src
                  chmod -R +w src
                  cd src
                  HOME=$TMPDIR treefmt --ci --walk filesystem
                  touch "$out"
                '';
          }
          // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            vm = import ./nix/vm-test.nix {
              inherit pkgs;
              package = config.packages.default.overrideAttrs { doCheck = false; };
            };
          };

          formatter = pkgs.treefmt.withConfig {
            runtimeInputs = [
              pkgs.nixfmt
              pkgs.go_1_27
              pkgs.just
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
              formatter.just = {
                command = "just";
                options = [
                  "--fmt"
                  "--justfile"
                ];
                includes = [ "justfile" ];
              };
            };
          };
        };
    };
}
