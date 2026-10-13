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
            ./tools
          ];
          rustPkgs = pkgs.extend inputs.rust-overlay.overlays.default;
          rustToolchain = rustPkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          craneLib = (inputs.crane.mkLib rustPkgs).overrideToolchain rustToolchain;
          # Each derivation reads the narrowest source its build needs; see nix/sources.nix.
          cargoSources = import ./nix/sources.nix { inherit lib; };
          allCrates = cargoSources.members;
          # The workspace's test fixtures that live outside `crates/`, which tests read in place.
          testFixtures = [
            ./.config
            ./tools/bench/machines
            ./tools/bench/facts
            ./tools/bench/suite.json
            ./tools/bench/baseline.jsonl
            ./tools/bench/gates
          ];
          cliSource = cargoSources.source { code = cargoSources.closure [ "togi" ]; };
          simSource = cargoSources.source { code = cargoSources.closure [ "togi-tool-sim" ]; };
          trialSource = cargoSources.source {
            code = cargoSources.closureWithDev [ "togi-trial" ];
            full = [ "togi-trial" ];
          };
          # Compiles every target; the crates' own test data is inside them.
          cratesSource = cargoSources.source { full = allCrates; };
          testSource = cargoSources.source {
            full = allCrates;
            files = testFixtures;
          };
          # Tools that read code or manifests and no data.
          codeSource = cargoSources.source { rs = allCrates; };
          manifestSource = cargoSources.source { files = [ ./deny.toml ]; };
          rustLinker = lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "-C link-arg=-fuse-ld=mold";
          };
          # Linting aarch64-apple-darwin from Linux runs C build scripts (criterion's alloca) for that target,
          # which needs a compiler that takes Apple's flags; the host gcc does not. The unwrapped clang and
          # llvm-ar are cross compilers; alloca.c needs only the compiler's own stddef.h and stdint.h.
          darwinCrossCc = lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            CC_aarch64_apple_darwin = lib.getExe' pkgs.llvmPackages.clang-unwrapped "clang";
            AR_aarch64_apple_darwin = lib.getExe' pkgs.llvm "llvm-ar";
          };
          # Dependencies read only the manifests and the lock: the dummy source and the vendor directory ignore code.
          depsSource = cargoSources.source { code = allCrates; };
          cargoArgs = {
            pname = "togi-workspace";
            version = "0.0.0";
            cargoVendorDir = craneLib.vendorCargoDeps { inherit (depsSource) src; };
            strictDeps = true;
            # Crane builds the release profile by default; the checks build what `just gate` builds.
            CARGO_PROFILE = "dev";
            nativeBuildInputs = lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.mold ];
          }
          // rustLinker;
          cargoWith = source: args: cargoArgs // source // args;
          depsArgs = cargoArgs // {
            dummySrc = craneLib.mkDummySrc { inherit (depsSource) src; };
          };
          cargoDeps = craneLib.buildDepsOnly depsArgs;
          cargoCheck = source: args: cargoWith source args // { cargoArtifacts = cargoDeps; };
          cargoNoDeps =
            source: args:
            cargoWith source args
            // {
              cargoArtifacts = null;
              doInstallCargoArtifacts = false;
              nativeBuildInputs = cargoArgs.nativeBuildInputs ++ args.nativeBuildInputs;
            };
          simSuite = lib.importJSON ./nix/sim-suite.json;
          releaseArgs = cargoArgs // {
            CARGO_PROFILE = "release";
            CARGO_PROFILE_RELEASE_LTO = "false";
          };
          simRelease = cargoWith simSource (
            releaseArgs
            // {
              pname = "togi-tool-sim";
              cargoExtraArgs = "--locked -p togi-tool-sim";
            }
          );
          releaseDeps = craneLib.buildDepsOnly (
            releaseArgs
            // {
              inherit (depsArgs) dummySrc;
              pname = "togi-release";
              cargoExtraArgs = "--locked -p togi -p togi-tool-sim";
            }
          );
          togiToolSim = craneLib.buildPackage (
            simRelease
            // {
              cargoArtifacts = releaseDeps;
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
            pkgs.runCommand "togi-sim-suite-${toString shard}" { } ''
              sim=${togiToolSim}/bin/togi-tool-sim
              suite=${simSuiteSource}/tools/bench/suite.json
              jobs=''${NIX_BUILD_CORES:-0}
              [ "$jobs" -ge 1 ] || jobs=$(nproc)
              "$sim" suite --suite "$suite" --shards ${toString simSuite.shards} --shard ${toString shard} --jobs "$jobs" --out "$out"
              "$sim" check --suite "$suite" --shards ${toString simSuite.shards} --shard ${toString shard} "$out"
            '';
          simSuiteChecks = lib.listToAttrs (
            map (shard: lib.nameValuePair "sim-suite-${toString shard}" (simSuiteShard shard)) (
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
          goTools = pkgs.buildGo127Module {
            pname = "togi-go-tools";
            inherit version;
            src = lib.fileset.toSource {
              root = ./.;
              fileset = goFiles;
            };
            vendorHash = "sha256-uPqabZgQGQulf+F3BvMLhv4O0h5jOq12F7K60u5xjtA=";
            nativeCheckInputs = [ pkgs.gitMinimal ];
            subPackages = [
              "tools/release"
              "tools/board"
              "tools/reviews"
              "tools/cover"
            ];
            checkPhase = testPhase "" "./...";
          };
          togiPackage =
            rev:
            craneLib.buildPackage (
              cargoWith cliSource (
                releaseArgs
                // {
                  pname = "togi";
                  inherit version meta;
                  cargoExtraArgs = "--locked -p togi";
                  cargoArtifacts = releaseDeps;
                  doCheck = false;
                  TOGI_REV = rev;
                  nativeBuildInputs = cargoArgs.nativeBuildInputs ++ [ pkgs.installShellFiles ];
                  postInstall = installCompletions;
                }
              )
            );
        in
        {
          packages.default = togiPackage rev;

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
            // darwinCrossCc
          );

          checks = {
            go-tools = goTools;
            clippy = craneLib.mkCargoDerivation (
              cargoCheck cratesSource {
                pnameSuffix = "-clippy";
                buildPhaseCargoCommand = ''
                  cargoWithProfile clippy --locked --all-targets -- --deny warnings
                  cargoWithProfile clippy --locked --all-targets --all-features -- --deny warnings
                  cargoWithProfile clippy --locked --all-targets --target aarch64-apple-darwin -- --deny warnings
                '';
              }
              // darwinCrossCc
            );
            nextest = craneLib.cargoNextest (cargoCheck testSource {
              cargoNextestExtraArgs = "--profile ci";
              nativeCheckInputs = [
                pkgs.gitMinimal
                pkgs.bashInteractive
                pkgs.nushell
              ];
            });
            doc = craneLib.cargoDocTest (cargoCheck cratesSource { });
            cargo-fmt = craneLib.cargoFmt (cargoWith codeSource { });
            deny = craneLib.mkCargoDerivation (cargoNoDeps manifestSource {
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
            shear = craneLib.mkCargoDerivation (cargoNoDeps codeSource {
              pnameSuffix = "-shear";
              nativeBuildInputs = [ pkgs.cargo-shear ];
              buildPhaseCargoCommand = "cargo shear";
            });
            package =
              let
                package = togiPackage "dev";
              in
              # `package` lets the module check and the VM tests use this dev build from any system.
              pkgs.runCommand "togi-package" { passthru = { inherit package; }; } ''
                togi=${lib.getExe package}
                test "$("$togi" --version)" = "togi ${version}+dev"
                for shell in bash zsh fish nushell; do
                  "$togi" completion "$shell" > "$shell"
                  grep -q togi "$shell"
                done
                touch "$out"
              '';
            lint = config.checks.go-tools.overrideAttrs (old: {
              pname = "togi-lint";
              goModules = config.checks.go-tools.goModules;
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
              package = inputs.self.checks.x86_64-linux.package.package;
            };
          }
          // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux (
            let
              vm = import ./nix/vm-test.nix {
                inherit pkgs;
                trialTests = config.checks.trial-scope-tests;
                package = config.checks.package.package;
              };
            in
            {
              trial-scope-tests = craneLib.mkCargoDerivation (cargoCheck trialSource {
                pnameSuffix = "-trial-scope-tests";
                doInstallCargoArtifacts = false;
                nativeBuildInputs = cargoArgs.nativeBuildInputs ++ [ pkgs.jq ];
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
                  install_test hardware togi-trial-tests
                  install_test cached_errors togi-trial-cached-errors
                '';
              });
              vm = vm.tuning-boot;
              vm-restart-limit = vm.restart-limit;
              vm-trial-scope = vm.trial-scope;
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
