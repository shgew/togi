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
          # Product tests include the shipped closure and the simulator/dashboard support they use.
          productCrates = cargoSources.closureWithDev [
            "togi"
            "togi-simrun"
            "togi-watchtest"
          ];
          # Unshipped libraries and programs register here; product dependencies are discovered above.
          toolCrates = [
            "togi-facts"
            "togi-statecopy"
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
            "togi-tool-reviews"
            "togi-tool-release"
            "togi-tool-cover"
          ];
          nextestConcerns = {
            nextest = {
              crates = productCrates;
              tier = "local";
            };
            nextest-tools = {
              crates = toolCrates;
              tier = "ci";
            };
          };
          registered = productCrates ++ toolCrates;
          unregistered = lib.subtractLists registered allCrates;
          misregistered = lib.filter (
            crate: lib.count (c: c == crate) registered > 1 || !lib.elem crate allCrates
          ) (lib.unique registered);
          validConcerns =
            lib.assertMsg (unregistered == [ ] && misregistered == [ ])
              "nextest concerns must name every workspace member exactly once; unregistered: ${toString unregistered}; repeated or not a member: ${toString misregistered}";
          cliSource = cargoSources.source { code = cargoSources.closure [ "togi" ]; };
          # The trial tests compile only the `hardware` and `cached_errors` targets of `togi-trial` against the
          # library; every other test file of the crate is a stub.
          trialSource = cargoSources.source {
            code = cargoSources.closureWithDev [ "togi-trial" ];
            files = [
              ./crates/togi-trial/tests/hardware.rs
              ./crates/togi-trial/tests/cached_errors.rs
            ];
          };
          # The recovery tests compile only the `vm_recovery` target of `togi`; its other test files are stubs.
          recoverySource = cargoSources.source {
            code = cargoSources.closureWithDev [ "togi" ];
            files = [ ./crates/togi/tests/vm_recovery.rs ];
          };
          # Documentation tests need crate-local fixtures, not external benchmark inputs.
          cratesSource = cargoSources.source { full = allCrates; };
          # clippy compiles every crate's tests, which include some fixtures at compile time, and reads the lint
          # configuration; no other derivation does.
          testSource = cargoSources.source {
            full = allCrates;
            files = testFixtures allCrates ++ [ ./clippy.toml ];
          };
          nextestChecks =
            assert validConcerns;
            lib.mapAttrs (
              name: concern:
              let
                inherit (concern) crates;
              in
              craneLib.cargoNextest (
                cargoCheck
                  (cargoSources.source {
                    code = cargoSources.closureWithDev crates;
                    full = crates;
                    files = testFixtures crates;
                  })
                  {
                    pname = "togi-${name}";
                    cargoExtraArgs = "--locked ${lib.concatMapStringsSep " " (crate: "-p ${crate}") crates}";
                    cargoNextestExtraArgs = "--profile ci";
                    passthru = {
                      inherit crates;
                      inherit (concern) tier;
                    };
                    nativeCheckInputs = [
                      pkgs.gitMinimal
                      pkgs.bashInteractive
                      pkgs.nushell
                    ];
                  }
              )
            ) nextestConcerns;
          recoveryCrates = [
            "togi-session"
            "togi-simrun"
          ];
          recoveryCheck = craneLib.cargoNextest (
            cargoCheck
              (cargoSources.source {
                code = cargoSources.closureWithDev recoveryCrates;
                full = recoveryCrates;
                files = testFixtures recoveryCrates;
              })
              {
                pname = "togi-recovery";
                cargoExtraArgs = "--locked ${lib.concatMapStringsSep " " (crate: "-p ${crate}") recoveryCrates}";
                # Run precisely the excluded campaigns; their inner cut pools are bounded separately.
                cargoNextestExtraArgs = "--profile ci --ignore-default-filter --test-threads 1 -E 'not default()'";
                passthru.tier = "ci";
                nativeCheckInputs = [
                  pkgs.gitMinimal
                  pkgs.bashInteractive
                  pkgs.nushell
                ];
              }
          );
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
          releaseArgs = cargoArgs // {
            CARGO_PROFILE = "release";
            CARGO_PROFILE_RELEASE_LTO = "false";
          };
          releaseDeps = craneLib.buildDepsOnly (
            releaseArgs
            // {
              inherit (depsArgs) dummySrc;
              pname = "togi-release";
              cargoExtraArgs = "--locked -p togi";
            }
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
          # Private members retain narrow inputs, named failure logs and independently cached outputs.
          qualityMembers = {
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
            cover-test =
              pkgs.runCommand "togi-cover-test"
                {
                  nativeBuildInputs = [ pkgs.nushell ];
                  src = ./tools/cover;
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
            # This source has the whole tree, unlike nextest concerns, so also validate every test-only path here.
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
                  while read -r path || [[ -n "$path" ]]; do
                    case "$path" in ""|\#*) continue ;; esac
                    if [[ ! -e "crates/$path" ]]; then
                      echo "Missing test-only path: crates/$path" >&2
                      exit 1
                    fi
                  done < crates/test-only.txt
                  HOME=$TMPDIR lychee --offline --hidden --include-fragments --no-progress --format compact './**/*.md'
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
          };
        in
        {
          packages.default = togiPackage rev;

          devShells.default = pkgs.mkShell (
            {
              packages = [
                pkgs.gh
                pkgs.cachix
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
            nextest-recovery = recoveryCheck;
            quality = pkgs.linkFarm "togi-quality" (
              lib.mapAttrsToList (name: path: { inherit name path; }) qualityMembers
            );
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
            # No other check's output references the shared dependency artifacts; as checks, CI pushes them to Cachix.
            cargo-deps = cargoDeps;
            release-deps = releaseDeps;
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
