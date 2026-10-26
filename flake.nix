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
          rustPkgs = pkgs.extend inputs.rust-overlay.overlays.default;
          rustToolchain = rustPkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          craneLib = (inputs.crane.mkLib rustPkgs).overrideToolchain rustToolchain;
          # Each derivation reads the narrowest source its build needs; see nix/sources.nix.
          cargoSources = import ./nix/sources.nix { inherit lib; };
          allCrates = cargoSources.members;
          # The test fixtures outside `crates/` that each crate's tests read in place.
          crateFixtures =
            let
              bench = ./tools/bench;
            in
            {
              togi = [ (bench + "/machines") ];
              togi-sim = [ (bench + "/machines") ];
              togi-simrun = [ (bench + "/machines") ];
              togi-tool-bench = [
                (bench + "/machines")
                (bench + "/facts")
                (bench + "/suite.json")
                (bench + "/baseline.jsonl")
                (bench + "/gates")
              ];
              togi-tool-fit = [ (bench + "/facts") ];
              togi-tool-sim = [
                (bench + "/machines")
                (bench + "/suite.json")
              ];
              togi-tool-trialfacts = [ (bench + "/facts") ];
            };
          # nextest's profiles, and the fixtures the named crates' tests read.
          testFixtures = crates: [ ./.config ] ++ lib.concatMap (crate: crateFixtures.${crate} or [ ]) crates;
          # Each group is one `nextest-<group>` check, so a change reruns only the groups whose tests build it.
          # Every workspace member is in exactly one group.
          nextestGroups = {
            core = [
              "togi-machine"
              "togi-journal"
              "togi-config"
              "togi-requests"
              "togi-tuner"
              "togi-defect"
              "togi-facts"
              "togi-render"
              "togi-tuningboot"
              "togi-trialfiles"
              "togi-version"
              "togi-sim"
              "togi-statecopy"
            ];
            platform = [
              "togi-command"
              "togi-hostlock"
              "togi-backend"
              "togi-mprime"
              "togi-ycruncher"
              "togi-smu"
              "togi-detect"
              "togi-trial"
              "togi-hardware"
            ];
            session = [
              "togi-session"
              "togi-simrun"
              "togi-watch"
              "togi-watchtest"
              "togi"
            ];
            tools = [
              "togi-tool-audit"
              "togi-tool-bench"
              "togi-tool-modelcheck"
              "togi-tool-trialfacts"
              "togi-tool-copy-state"
              "togi-tool-facts"
              "togi-tool-fit"
              "togi-tool-replay"
              "togi-tool-sim"
              "togi-tool-stats"
            ];
            repo = [
              "togi-tool-reviews"
              "togi-tool-release"
              "togi-tool-cover"
            ];
          };
          grouped = lib.concatLists (lib.attrValues nextestGroups);
          ungrouped = lib.subtractLists grouped allCrates;
          misgrouped = lib.filter (
            crate: lib.count (c: c == crate) grouped > 1 || !lib.elem crate allCrates
          ) (lib.unique grouped);
          emptyGroups = lib.attrNames (lib.filterAttrs (_: crates: crates == [ ]) nextestGroups);
          validGroups =
            lib.assertMsg (ungrouped == [ ] && misgrouped == [ ] && emptyGroups == [ ])
              "nextestGroups must name every workspace member exactly once and no group may be empty; in no group: ${toString ungrouped}; repeated or not a member: ${toString misgrouped}; empty: ${toString emptyGroups}";
          cliSource = cargoSources.source { code = cargoSources.closure [ "togi" ]; };
          simSource = cargoSources.source { code = cargoSources.closure [ "togi-tool-sim" ]; };
          trialSource = cargoSources.source {
            code = cargoSources.closureWithDev [ "togi-trial" ];
            full = [ "togi-trial" ];
          };
          # The recovery tests compile `togi` whole (its tests and their data) with the dev closure as code.
          recoverySource = cargoSources.source {
            code = cargoSources.closureWithDev [ "togi" ];
            full = [ "togi" ];
          };
          # Documentation tests need crate-local fixtures, not external benchmark inputs.
          cratesSource = cargoSources.source { full = allCrates; };
          # clippy compiles every crate's tests, which include some fixtures at compile time.
          testSource = cargoSources.source {
            full = allCrates;
            files = testFixtures allCrates;
          };
          nextestChecks =
            assert validGroups;
            lib.mapAttrs' (
              group: crates:
              lib.nameValuePair "nextest-${group}" (
                craneLib.cargoNextest (
                  cargoCheck
                    (cargoSources.source {
                      code = cargoSources.closureWithDev crates;
                      full = crates;
                      files = testFixtures crates;
                    })
                    {
                      pname = "togi-${group}";
                      cargoExtraArgs = "--locked ${lib.concatMapStringsSep " " (crate: "-p ${crate}") crates}";
                      cargoNextestExtraArgs = "--profile ci";
                      nativeCheckInputs = [
                        pkgs.gitMinimal
                        pkgs.bashInteractive
                        pkgs.nushell
                      ];
                    }
                )
              )
            ) nextestGroups;
          # Tools that read code or manifests and no data.
          codeSource = cargoSources.source { rs = allCrates; };
          manifestSource = cargoSources.source { files = [ ./deny.toml ]; };
          rustLinker = lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "-C link-arg=-fuse-ld=mold";
          };
          # Clippy checks the other of the two supported targets from each host: aarch64-apple-darwin from
          # Linux and x86_64-unknown-linux-gnu from macOS.
          crossTarget =
            if pkgs.stdenv.hostPlatform.isLinux then "aarch64-apple-darwin" else "x86_64-unknown-linux-gnu";
          # Linting the cross target runs C build scripts (criterion's alloca) for it, which needs a compiler
          # that takes that target's flags; the host's wrapped compiler does not. The unwrapped clang and
          # llvm-ar are cross compilers; alloca.c needs only the compiler's own stddef.h and stdint.h.
          crossCc = {
            "CC_${lib.replaceStrings [ "-" ] [ "_" ] crossTarget}" =
              lib.getExe' pkgs.llvmPackages.clang-unwrapped "clang";
            "AR_${lib.replaceStrings [ "-" ] [ "_" ] crossTarget}" = lib.getExe' pkgs.llvm "llvm-ar";
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
          # Checks keep no Cargo target archive: nothing builds on their outputs, and Cachix would store it.
          cargoCheck =
            source: args:
            cargoWith source args
            // {
              cargoArtifacts = cargoDeps;
              doInstallCargoArtifacts = false;
            };
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
                pkgs.gh
                pkgs.gh-token
                pkgs.just
                pkgs.nixfmt
                pkgs.bashInteractive
                pkgs.nushell
                pkgs.duckdb
                rustToolchain
                pkgs.cargo-deny
                pkgs.cargo-fuzz
                pkgs.cargo-llvm-cov
                pkgs.cargo-nextest
                pkgs.cargo-shear
                pkgs.mold
              ];
              TOGI_DEV_SHELL = "1";
            }
            // rustLinker
            // crossCc
          );

          checks = {
            board-test =
              pkgs.runCommand "togi-board-test"
                {
                  nativeBuildInputs = [ pkgs.nushell ];
                  src = ./tools/board;
                }
                ''
                  cd "$src"
                  HOME=$TMPDIR nu --no-config-file tests.nu
                  touch "$out"
                '';
            sql-test =
              pkgs.runCommand "togi-sql-test"
                {
                  nativeBuildInputs = [ pkgs.duckdb ];
                  src = ./tools/sql;
                }
                ''
                  cp -R "$src" sql
                  chmod -R u+w sql
                  patchShebangs sql
                  HOME=$TMPDIR sql/test.sh
                  touch "$out"
                '';
            # Relative links and heading anchors in every Markdown file, offline: a link may name any file.
            doc-links =
              pkgs.runCommand "togi-doc-links"
                {
                  nativeBuildInputs = [ pkgs.lychee ];
                  src = lib.fileset.toSource {
                    root = ./.;
                    fileset = ./.;
                  };
                  # lychee builds its HTTP client even offline, and the client needs a certificate store.
                  SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
                }
                ''
                  cd "$src"
                  HOME=$TMPDIR lychee --offline --hidden --include-fragments --no-progress --format compact './**/*.md'
                  touch "$out"
                '';
            clippy = craneLib.mkCargoDerivation (
              cargoCheck testSource {
                pnameSuffix = "-clippy";
                buildPhaseCargoCommand = ''
                  cargoWithProfile clippy --locked --all-targets -- --deny warnings
                  cargoWithProfile clippy --locked --all-targets --all-features -- --deny warnings
                  cargoWithProfile clippy --locked --all-targets --all-features --target ${crossTarget} -- --deny warnings
                '';
              }
              // crossCc
            );
            doc = craneLib.cargoDocTest (cargoCheck cratesSource { });
            # No other check's output references the shared dependency artifact; as a check, CI pushes it to Cachix.
            cargo-deps = cargoDeps;
            deny = craneLib.mkCargoDerivation (
              cargoNoDeps manifestSource {
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
              }
            );
            shear = craneLib.mkCargoDerivation (
              cargoNoDeps codeSource {
                pnameSuffix = "-shear";
                nativeBuildInputs = [ pkgs.cargo-shear ];
                buildPhaseCargoCommand = "cargo shear";
              }
            );
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
            fmt =
              pkgs.runCommand "togi-fmt"
                {
                  nativeBuildInputs = [ config.formatter ];
                  src = lib.fileset.toSource {
                    root = ./.;
                    fileset = lib.fileset.unions [
                      ./flake.nix
                      ./justfile
                      (lib.fileset.fileFilter (f: f.hasExt "nix" || f.hasExt "rs") ./.)
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
            changes = craneLib.mkCargoDerivation (
              cargoCheck
                (cargoSources.source {
                  code = cargoSources.closure [ "togi-tool-release" ];
                  files = [ ./changes ];
                })
                {
                  pnameSuffix = "-changes";
                  buildPhaseCargoCommand = "cargoWithProfile run --locked -p togi-tool-release -- -check changes";
                  installPhaseCommand = "touch $out";
                }
            );
            module = import ./nix/module-test.nix {
              pkgs = inputs.nixpkgs.legacyPackages.x86_64-linux;
              hostPkgs = pkgs;
              package = inputs.self.checks.x86_64-linux.package.package;
            };
          }
          // nextestChecks
          // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux (
            let
              vm = import ./nix/vm-test.nix {
                inherit pkgs;
                trialTests = config.checks.trial-scope-tests;
                vmRecovery = config.checks.vm-recovery-tests;
                package = config.checks.package.package;
              };
            in
            {
              trial-scope-tests = craneLib.mkCargoDerivation (
                cargoCheck trialSource {
                  pnameSuffix = "-trial-scope-tests";
                  nativeBuildInputs = cargoArgs.nativeBuildInputs ++ [
                    pkgs.jq
                    craneLib.removeReferencesToRustToolchainHook
                    craneLib.removeReferencesToVendoredSourcesHook
                  ];
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
                }
              );
              vm-recovery-tests = craneLib.mkCargoDerivation (
                cargoCheck recoverySource {
                  pnameSuffix = "-vm-recovery-tests";
                  nativeBuildInputs = cargoArgs.nativeBuildInputs ++ [
                    pkgs.jq
                    craneLib.removeReferencesToRustToolchainHook
                    craneLib.removeReferencesToVendoredSourcesHook
                  ];
                  buildPhaseCargoCommand = ''
                    cargoWithProfile test --locked -p togi --test vm_recovery --no-run \
                      --message-format json-render-diagnostics > cargo-messages.json
                  '';
                  installPhaseCommand = ''
                    mkdir -p "$out/bin"
                    jq -r 'select(.reason == "compiler-artifact" and .target.name == "vm_recovery" and .executable != null) | .executable' \
                      cargo-messages.json > executables
                    test "$(wc -l < executables)" = 1
                    cp "$(cat executables)" "$out/bin/togi-vm-recovery"
                  '';
                }
              );
              vm-grub = vm.grub;
              vm-runtime = vm.runtime;
              vm-display = vm.display;
              vm-efi = vm.efi;
            }
            // simSuiteChecks
          );

          formatter = pkgs.treefmt.withConfig {
            runtimeInputs = [
              pkgs.nixfmt
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
