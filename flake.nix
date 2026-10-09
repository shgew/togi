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
          goFiles = lib.fileset.unions [
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
          shippedFiles =
            lib.fileset.difference
              (lib.fileset.intersection goFiles (
                lib.fileset.fromSource (
                  lib.sources.cleanSourceWith {
                    src = ./.;
                    filter = path: type: type != "directory" || builtins.baseNameOf path != "testdata";
                  }
                )
              ))
              (
                lib.fileset.unions [
                  ./tools
                  ./internal/sim
                  ./internal/simrun
                  (lib.fileset.fileFilter (f: lib.hasSuffix "_test.go" f.name) ./.)
                ]
              );
          shippedPackage = config.checks.package.overrideAttrs {
            src = lib.fileset.toSource {
              root = ./.;
              fileset = shippedFiles;
            };
            # Keep the full dependency set and its hash when tools-only imports disappear.
            goModules = config.checks.package.goModules;
            doCheck = false;
          };
          testPhase = flags: packages: ''
            runHook preCheck
            export GOFLAGS=''${GOFLAGS//-trimpath/}
            go test -p $NIX_BUILD_CORES ${flags} -shuffle=on -tags integration ${packages}
            runHook postCheck
          '';
          togi =
            rev:
            pkgs.buildGo127Module {
              pname = "togi";
              version = lib.fileContents ./version.txt;
              src = lib.fileset.toSource {
                root = ./.;
                fileset = goFiles;
              };
              vendorHash = "sha256-KMcBcFvwlqulewo8hwkFBrsiy1PlEeUIWc71LdDEUT0=";
              nativeBuildInputs = [ pkgs.installShellFiles ];
              nativeCheckInputs = [
                pkgs.gitMinimal
                pkgs.nushell
              ]
              ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.util-linux ];
              ldflags = [ "-X github.com/shgew/togi.rev=${rev}" ];
              subPackages = [ "cmd/togi" ];
              checkPhase = testPhase "" "./...";
              postInstall = lib.optionalString (pkgs.stdenv.buildPlatform.canExecute pkgs.stdenv.hostPlatform) ''
                installShellCompletion --cmd togi \
                  --bash <($out/bin/togi completion bash) \
                  --zsh <($out/bin/togi completion zsh) \
                  --fish <($out/bin/togi completion fish) \
                  --nushell <($out/bin/togi completion nushell)
              '';
              meta = {
                description = "Per-core Curve Optimizer tuner for Zen 5 desktop CPUs";
                license = lib.licenses.gpl3Plus;
                mainProgram = "togi";
                platforms = [ system ];
              };
            };
        in
        {
          packages.default = togi (inputs.self.shortRev or inputs.self.dirtyShortRev or "dev");

          devShells.default = pkgs.mkShell {
            packages = [
              pkgs.flock
              pkgs.go_1_27
              pkgs.gh
              pkgs.gh-token
              pkgs.gopls
              pkgs.golangci-lint
              pkgs.just
              pkgs.nixfmt
              pkgs.nushell
            ];
            GOTOOLCHAIN = "local";
            TOGI_DEV_SHELL = "1";
          };

          checks = {
            package = togi "dev";
            race = config.checks.package.overrideAttrs {
              pname = "togi-race";
              src = lib.fileset.toSource {
                root = ./.;
                fileset = lib.fileset.difference goFiles ./tools;
              };
              goModules = config.checks.package.goModules;
              buildPhase = ''
                runHook preBuild
                runHook postBuild
              '';
              checkPhase = testPhase "-race" "./internal/trial ./internal/journal ./internal/watch ./internal/smu ./cmd/togi";
              installPhase = "mkdir -p $out";
              dontFixup = true;
            };
            lint = config.checks.package.overrideAttrs (old: {
              pname = "togi-lint";
              goModules = config.checks.package.goModules;
              nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.golangci-lint ];
              buildPhase = ''
                runHook preBuild
                export HOME=$TMPDIR GOLANGCI_LINT_CACHE=$TMPDIR/golangci-lint
                for target in linux/amd64 darwin/arm64; do
                  GOOS=''${target%/*} GOARCH=''${target#*/} CGO_ENABLED=0 golangci-lint run ./...
                done
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
            changes =
              pkgs.runCommand "togi-changes"
                {
                  nativeBuildInputs = [ pkgs.go_1_27 ];
                  src = lib.fileset.toSource {
                    root = ./.;
                    fileset = lib.fileset.unions [
                      ./go.mod
                      ./go.sum
                      ./tools/release
                      ./changes
                    ];
                  };
                }
                ''
                  cd "$src"
                  HOME=$TMPDIR GOCACHE=$TMPDIR/go-cache GOPROXY=off GOTOOLCHAIN=local CGO_ENABLED=0 go run ./tools/release -check changes
                  touch "$out"
                '';
            module = import ./nix/module-test.nix {
              pkgs = inputs.nixpkgs.legacyPackages.x86_64-linux;
              hostPkgs = pkgs;
              package = inputs.self.checks.x86_64-linux.package;
            };
          }
          // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux (
            let
              vm = import ./nix/vm-test.nix {
                inherit pkgs;
                trialTests = config.checks.trial-scope-tests;
                package = shippedPackage;
              };
            in
            {
              trial-scope-tests = shippedPackage.overrideAttrs {
                pname = "togi-trial-scope-tests";
                src = lib.fileset.toSource {
                  root = ./.;
                  fileset = lib.fileset.unions [
                    shippedFiles
                    (lib.fileset.fileFilter (
                      f: lib.hasPrefix "hardware_" f.name && lib.hasSuffix "_test.go" f.name
                    ) ./internal/trial)
                    ./internal/trial/helper_test.go
                    ./internal/trial/trial_test.go
                    ./internal/trial/fake_test.go
                  ];
                };
                buildPhase = ''
                  runHook preBuild
                  go test -c -tags hardware -o togi-trial-tests ./internal/trial
                  runHook postBuild
                '';
                doCheck = false;
                # The test binary has no bin/togi to print completion scripts.
                postInstall = "";
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
