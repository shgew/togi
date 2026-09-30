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
          testPhase = flags: packages: ''
            runHook preCheck
            export GOFLAGS=''${GOFLAGS//-trimpath/}
            go test -p $NIX_BUILD_CORES ${flags} -shuffle=on ${lib.optionalString pkgs.stdenv.hostPlatform.isLinux "-tags integration"} ${packages}
            runHook postCheck
          '';
          togi =
            rev:
            pkgs.buildGo127Module {
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
                  ./version_test.go
                  ./cmd
                  ./internal
                  ./tools
                ];
              };
              vendorHash = "sha256-OGYOqVtPseV1QvWhbVlQfXPcuTGIWne3Fk7JYtee1ak=";
              nativeCheckInputs = lib.optionals pkgs.stdenv.hostPlatform.isLinux [
                pkgs.util-linux
                pkgs.gitMinimal
              ];
              ldflags = [ "-X github.com/shgew/togi.rev=${rev}" ];
              subPackages = [ "cmd/togi" ];
              checkPhase = testPhase "" "./...";
              meta = {
                description = "Per-core Curve Optimizer tuner for Zen 5 desktop CPUs";
                license = lib.licenses.mit;
                mainProgram = "togi";
                platforms = [ system ];
              };
            };
        in
        {
          packages.default = togi (inputs.self.shortRev or inputs.self.dirtyShortRev or "dev");

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
            package = togi "dev";
            race = config.checks.package.overrideAttrs {
              pname = "togi-race";
              buildPhase = ''
                runHook preBuild
                runHook postBuild
              '';
              checkPhase = testPhase "-race" "./internal/trial ./internal/session ./internal/journal ./internal/watch";
              installPhase = "mkdir -p $out";
              dontFixup = true;
            };
            lint = config.checks.package.overrideAttrs (old: {
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
          // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux (
            let
              vm = import ./nix/vm-test.nix {
                inherit pkgs;
                trialTests = config.checks.trial-scope-tests;
                package = config.checks.package.overrideAttrs { doCheck = false; };
              };
            in
            {
              module = import ./nix/module-test.nix {
                inherit pkgs;
                package = config.checks.package;
              };
              trial-scope-tests = config.checks.package.overrideAttrs {
                pname = "togi-trial-scope-tests";
                buildPhase = ''
                  runHook preBuild
                  go test -c -tags hardware -o togi-trial-tests ./internal/trial
                  runHook postBuild
                '';
                doCheck = false;
                installPhase = ''
                  runHook preInstall
                  mkdir -p "$out/bin"
                  cp togi-trial-tests "$out/bin/"
                  runHook postInstall
                '';
              };
              vm = vm.tuning-boot;
              vm-restart-limit = vm.restart-limit;
            }
          );

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
