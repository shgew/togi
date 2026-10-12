{
  description = "Per-core Curve Optimizer tuner for Zen 5 desktop CPUs";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
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
          rustPkgs = pkgs.extend inputs.rust-overlay.overlays.default;
          rustToolchain = rustPkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          craneLib = (inputs.crane.mkLib rustPkgs).overrideToolchain rustToolchain;
          rustFiles = lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./rust-toolchain.toml
            ./clippy.toml
            ./deny.toml
            ./.config
            ./crates
            ./internal/detect/testdata
            ./tools/bench/machines
            ./tools/bench/facts
            ./tools/bench/suite.json
            ./tools/bench/baseline.jsonl
            ./tools/bench/gates
            # The backend crates' tests read the Go adapters' output fixtures in place.
            ./internal/backend/mprime/testdata
            ./internal/backend/ycruncher/testdata
            ./version.txt
          ];
          rustLinker = lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "-C link-arg=-fuse-ld=mold";
          };
          rustArgs = {
            pname = "togi-rust";
            version = "0.0.0";
            src = lib.fileset.toSource {
              root = ./.;
              fileset = rustFiles;
            };
            strictDeps = true;
            # Crane builds the release profile by default; the checks build what `just gate` builds.
            CARGO_PROFILE = "dev";
            nativeBuildInputs = lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.mold ];
          }
          // rustLinker;
          rustDeps = craneLib.buildDepsOnly rustArgs;
          rustCheck = args: rustArgs // args // { cargoArtifacts = rustDeps; };
          rustNoDeps =
            args:
            rustArgs
            // args
            // {
              cargoArtifacts = null;
              doInstallCargoArtifacts = false;
              nativeBuildInputs = rustArgs.nativeBuildInputs ++ args.nativeBuildInputs;
            };
          simSuite = lib.importJSON ./nix/sim-suite.json;
          rustRelease = rustArgs // {
            CARGO_PROFILE = "release";
            CARGO_PROFILE_RELEASE_LTO = "false";
          };
          simRelease = rustRelease // {
            pname = "togi-tool-sim";
            cargoExtraArgs = "--locked -p togi-tool-sim";
          };
          rustReleaseDeps = craneLib.buildDepsOnly (
            rustRelease
            // {
              pname = "togi-release";
              cargoExtraArgs = "--locked -p togi -p togi-tool-sim";
            }
          );
          togiToolSim = craneLib.buildPackage (
            simRelease
            // {
              cargoArtifacts = rustReleaseDeps;
              doCheck = false;
            }
          );
          simSuiteSource = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./tools/bench/suite.json
              ./tools/bench/machines
              ./tools/bench/facts
            ];
          };
          simSuiteShard =
            shard:
            pkgs.runCommand "togi-rust-sim-suite-${toString shard}" { } ''
              sim=${togiToolSim}/bin/togi-tool-sim
              suite=${simSuiteSource}/tools/bench/suite.json
              jobs=''${NIX_BUILD_CORES:-0}
              [ "$jobs" -ge 1 ] || jobs=$(nproc)
              "$sim" suite --suite "$suite" --shards ${toString simSuite.shards} --shard ${toString shard} --jobs "$jobs" --out "$out"
              "$sim" check --suite "$suite" --shards ${toString simSuite.shards} --shard ${toString shard} "$out"
            '';
          simSuiteChecks = lib.listToAttrs (
            map (shard: lib.nameValuePair "rust-sim-suite-${toString shard}" (simSuiteShard shard)) (
              lib.range 0 (simSuite.shards - 1)
            )
          );
          advisoryDb = pkgs.runCommand "advisory-db" { nativeBuildInputs = [ pkgs.gitMinimal ]; } ''
            cp -r ${inputs.advisory-db} $out
            chmod -R +w $out
            cd $out
            git init --quiet
            git add --all
            git -c user.name=nix -c user.email=nix@localhost commit --quiet --message advisory-db
          '';
          testPhase = flags: packages: ''
            runHook preCheck
            export GOFLAGS=''${GOFLAGS//-trimpath/}
            go test -p $NIX_BUILD_CORES ${flags} -shuffle=on -tags integration ${packages}
            runHook postCheck
          '';
          version = lib.fileContents ./version.txt;
          rev = inputs.self.shortRev or inputs.self.dirtyShortRev or "dev";
          installCompletions = lib.optionalString (pkgs.stdenv.buildPlatform.canExecute pkgs.stdenv.hostPlatform) ''
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
          togi =
            rev:
            pkgs.buildGo127Module {
              pname = "togi";
              inherit version meta;
              src = lib.fileset.toSource {
                root = ./.;
                fileset = goFiles;
              };
              vendorHash = "sha256-w1Kcniaoea7gPne6mde/KhZiSAVNGJ/H9mNED8lmDMA=";
              nativeBuildInputs = [ pkgs.installShellFiles ];
              nativeCheckInputs = [
                pkgs.gitMinimal
                pkgs.nushell
              ]
              ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.util-linux ];
              ldflags = [ "-X github.com/shgew/togi.rev=${rev}" ];
              subPackages = [ "cmd/togi" ];
              checkPhase = testPhase "" "./...";
              postInstall = installCompletions;
            };
          togiRust =
            rev:
            craneLib.buildPackage (
              rustRelease
              // {
                pname = "togi-rs";
                inherit version meta;
                cargoExtraArgs = "--locked -p togi";
                cargoArtifacts = rustReleaseDeps;
                doCheck = false;
                TOGI_REV = rev;
                nativeBuildInputs = rustArgs.nativeBuildInputs ++ [ pkgs.installShellFiles ];
                postInstall = installCompletions;
              }
            );
        in
        {
          packages.default = togiRust rev;

          devShells.default = pkgs.mkShell (
            {
              packages = [
                pkgs.flock
                pkgs.go_1_27
                pkgs.gh
                pkgs.gh-token
                pkgs.gopls
                pkgs.golangci-lint
                pkgs.just
                pkgs.nixfmt
                pkgs.bashInteractive
                pkgs.nushell
                rustToolchain
                pkgs.cargo-deny
                pkgs.cargo-fuzz
                pkgs.cargo-llvm-cov
                pkgs.cargo-nextest
                pkgs.cargo-shear
                pkgs.mold
              ];
              GOTOOLCHAIN = "local";
              TOGI_DEV_SHELL = "1";
            }
            // rustLinker
          );

          checks = {
            package = togi "dev";
            rust-clippy = craneLib.mkCargoDerivation (rustCheck {
              pnameSuffix = "-clippy";
              buildPhaseCargoCommand = ''
                cargoWithProfile clippy --locked --all-targets -- --deny warnings
                cargoWithProfile clippy --locked --all-targets --all-features -- --deny warnings
                cargoWithProfile clippy --locked --all-targets --target aarch64-apple-darwin -- --deny warnings
              '';
            });
            rust-nextest = craneLib.cargoNextest (rustCheck {
              cargoNextestExtraArgs = "--profile ci";
              nativeCheckInputs = [
                pkgs.gitMinimal
                pkgs.bashInteractive
                pkgs.nushell
              ];
            });
            rust-doc = craneLib.cargoDocTest (rustCheck { });
            rust-fmt = craneLib.cargoFmt rustArgs;
            rust-deny = craneLib.mkCargoDerivation (rustNoDeps {
              pnameSuffix = "-deny";
              nativeBuildInputs = [
                pkgs.cargo-deny
                pkgs.gitMinimal
              ];
              buildPhaseCargoCommand = ''
                mkdir -p $CARGO_HOME/advisory-dbs
                cp -r ${advisoryDb} $CARGO_HOME/advisory-dbs/advisory-db-3157b0e258782691
                chmod -R u+w $CARGO_HOME/advisory-dbs
                cargo deny --offline --locked check bans licenses sources advisories
              '';
            });
            rust-shear = craneLib.mkCargoDerivation (rustNoDeps {
              pnameSuffix = "-shear";
              nativeBuildInputs = [ pkgs.cargo-shear ];
              buildPhaseCargoCommand = "cargo shear";
            });
            rust-package =
              let
                package = togiRust "dev";
              in
              # `package` lets the module check and the VM tests use this dev build from any system.
              pkgs.runCommand "togi-rust-package" { passthru = { inherit package; }; } ''
                togi=${lib.getExe package}
                test "$("$togi" --version)" = "togi ${version}+dev"
                for shell in bash zsh fish nushell; do
                  "$togi" completion "$shell" > "$shell"
                  grep -q togi "$shell"
                done
                touch "$out"
              '';
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
                      (lib.fileset.fileFilter (f: f.hasExt "go" || f.hasExt "nix" || f.hasExt "rs") ./.)
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
              package = inputs.self.checks.x86_64-linux.rust-package.package;
            };
          }
          // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux (
            let
              vm = import ./nix/vm-test.nix {
                inherit pkgs;
                trialTestsRs = config.checks.trial-scope-tests-rs;
                package = config.checks.rust-package.package;
              };
            in
            {
              trial-scope-tests-rs = craneLib.mkCargoDerivation (rustCheck {
                pnameSuffix = "-trial-scope-tests";
                doInstallCargoArtifacts = false;
                nativeBuildInputs = rustArgs.nativeBuildInputs ++ [ pkgs.jq ];
                buildPhaseCargoCommand = ''
                  cargoWithProfile test --locked -p togi-trial --features hardware --test hardware --test cached_errors \
                    --no-run --message-format json-render-diagnostics > cargo-messages.json
                '';
                installPhaseCommand = ''
                  mkdir -p "$out/bin"
                  install_test() {
                    jq -r --arg target "$1" \
                      'select(.reason == "compiler-artifact" and .target.name == $target and .executable != null) | .executable' \
                      cargo-messages.json > executables
                    test "$(wc -l < executables)" = 1
                    cp "$(cat executables)" "$out/bin/$2"
                  }
                  install_test hardware togi-trial-tests-rs
                  install_test cached_errors togi-trial-cached-errors-rs
                '';
              });
              vm = vm.tuning-boot;
              vm-restart-limit = vm.restart-limit;
              vm-trial-scope-rs = vm.trial-scope-rs;
            }
            // simSuiteChecks
          );

          formatter = pkgs.treefmt.withConfig {
            runtimeInputs = [
              pkgs.nixfmt
              pkgs.go_1_27
              pkgs.just
              rustToolchain
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
              formatter.rustfmt = {
                command = "rustfmt";
                options = [
                  "--edition"
                  "2024"
                ];
                includes = [ "*.rs" ];
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
