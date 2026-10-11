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
          advisoryDb = pkgs.runCommand "advisory-db" { nativeBuildInputs = [ pkgs.gitMinimal ]; } ''
            cp -r ${inputs.advisory-db} $out
            chmod -R +w $out
            cd $out
            git init --quiet
            git add --all
            git -c user.name=nix -c user.email=nix@localhost commit --quiet --message advisory-db
          '';
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
          comparison = import ./nix/comparison.nix {
            inherit pkgs lib;
            package = config.checks.package;
            goSource = lib.fileset.toSource {
              root = ./.;
              fileset = goFiles;
            };
            revision = inputs.self.rev or inputs.self.dirtyRev or "dev";
          };
        in
        {
          packages = {
            default = togi (inputs.self.shortRev or inputs.self.dirtyShortRev or "dev");
          }
          // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux comparison.packages;

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
                pkgs.nushell
                rustToolchain
                pkgs.cargo-deny
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
                cargoWithProfile clippy --locked --all-targets --target aarch64-apple-darwin -- --deny warnings
              '';
            });
            rust-nextest = craneLib.cargoNextest (
              rustCheck (
                {
                  cargoNextestExtraArgs = "--profile ci";
                }
                # main's macOS job builds only package and rust-nextest, so on aarch64-darwin rust-nextest
                # depends on the native regeneration of the arm64 math, Lgamma and Pow vectors (#637).
                // lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
                  gocompatVectorsMath = config.checks.gocompat-vectors-math;
                  gocompatVectorsLgamma = config.checks.gocompat-vectors-lgamma;
                  gocompatVectorsPow = config.checks.gocompat-vectors-pow;
                }
              )
            );
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
            guard =
              pkgs.runCommand "togi-guard"
                {
                  nativeBuildInputs = [
                    pkgs.gitMinimal
                    pkgs.nushell
                  ];
                  src = lib.fileset.toSource {
                    root = ./.;
                    fileset = ./tools/guard;
                  };
                }
                ''
                  cd "$src"
                  HOME=$TMPDIR nu --no-config-file tools/guard/test.nu
                  touch "$out"
                '';
            gocompat-vectors =
              pkgs.runCommand "togi-gocompat-vectors"
                {
                  nativeBuildInputs = [ pkgs.go_1_27 ];
                  src = lib.fileset.toSource {
                    root = ./.;
                    fileset = lib.fileset.unions [
                      ./go.mod
                      ./go.sum
                      ./cmd
                      ./internal
                      ./tools
                      ./crates/togi-gocompat/testdata
                    ];
                  };
                }
                ''
                  cd "$src"
                  export HOME=$TMPDIR GOCACHE=$TMPDIR/go-cache GOPROXY=off GOTOOLCHAIN=local CGO_ENABLED=0
                  mkdir $TMPDIR/vectors
                  for mode in float; do
                    go run ./tools/gocompat-vectors $mode $TMPDIR/vectors
                  done
                  for f in float-format.txt float-json.txt float-v.txt; do
                    cmp $TMPDIR/vectors/$f crates/togi-gocompat/testdata/$f
                  done
                  touch "$out"
                '';
            # Regenerates the math vectors from Go and requires them equal to the committed ones.
            # x86_64-linux writes all three files, running the arm64 build under qemu-user;
            # aarch64-darwin runs the arm64 paths natively and compares math-arm64.txt.
            gocompat-vectors-math =
              pkgs.runCommand "togi-gocompat-vectors-math"
                {
                  nativeBuildInputs = [
                    pkgs.go_1_27
                  ]
                  ++ lib.optional pkgs.stdenv.hostPlatform.isLinux pkgs.qemu-user;
                  src = lib.fileset.toSource {
                    root = ./.;
                    fileset = lib.fileset.unions [
                      ./go.mod
                      ./go.sum
                      ./cmd
                      ./internal
                      ./tools
                      ./crates/togi-gocompat/testdata
                    ];
                  };
                }
                (
                  ''
                    cd "$src"
                    export HOME=$TMPDIR GOCACHE=$TMPDIR/go-cache GOPROXY=off GOTOOLCHAIN=local CGO_ENABLED=0
                    go build -o $TMPDIR/gocompat-vectors ./tools/gocompat-vectors
                    mkdir $TMPDIR/vectors
                  ''
                  + (
                    if pkgs.stdenv.hostPlatform.isLinux then
                      ''
                        $TMPDIR/gocompat-vectors math-amd64-fma $TMPDIR/vectors
                        GODEBUG=cpu.fma=off $TMPDIR/gocompat-vectors math-amd64-nofma $TMPDIR/vectors
                        $TMPDIR/gocompat-vectors math-arm64 $TMPDIR/vectors
                        for f in math-amd64-fma.txt math-amd64-nofma.txt math-arm64.txt; do
                          cmp $TMPDIR/vectors/$f crates/togi-gocompat/testdata/$f
                        done
                      ''
                    else
                      ''
                        $TMPDIR/gocompat-vectors math-arm64 $TMPDIR/vectors
                        cmp $TMPDIR/vectors/math-arm64.txt crates/togi-gocompat/testdata/math-arm64.txt
                      ''
                  )
                  + ''
                    touch "$out"
                  ''
                );
            # Regenerates the Lgamma vectors from Go and requires them equal to the committed ones.
            # x86_64-linux writes both files, running the arm64 build under qemu-user;
            # aarch64-darwin runs the arm64 path natively and compares lgamma-arm64.txt.
            gocompat-vectors-lgamma =
              pkgs.runCommand "togi-gocompat-vectors-lgamma"
                {
                  nativeBuildInputs = [
                    pkgs.go_1_27
                  ]
                  ++ lib.optional pkgs.stdenv.hostPlatform.isLinux pkgs.qemu-user;
                  src = lib.fileset.toSource {
                    root = ./.;
                    fileset = lib.fileset.unions [
                      ./go.mod
                      ./go.sum
                      ./cmd
                      ./internal
                      ./tools
                      ./crates/togi-gocompat/testdata
                    ];
                  };
                }
                (
                  ''
                    cd "$src"
                    export HOME=$TMPDIR GOCACHE=$TMPDIR/go-cache GOPROXY=off GOTOOLCHAIN=local CGO_ENABLED=0
                    go build -o $TMPDIR/gocompat-vectors ./tools/gocompat-vectors
                    mkdir $TMPDIR/vectors
                  ''
                  + (
                    if pkgs.stdenv.hostPlatform.isLinux then
                      ''
                        $TMPDIR/gocompat-vectors lgamma-amd64 $TMPDIR/vectors
                        $TMPDIR/gocompat-vectors lgamma-arm64 $TMPDIR/vectors
                        for f in lgamma-amd64.txt lgamma-arm64.txt; do
                          cmp $TMPDIR/vectors/$f crates/togi-gocompat/testdata/$f
                        done
                      ''
                    else
                      ''
                        $TMPDIR/gocompat-vectors lgamma-arm64 $TMPDIR/vectors
                        cmp $TMPDIR/vectors/lgamma-arm64.txt crates/togi-gocompat/testdata/lgamma-arm64.txt
                      ''
                  )
                  + ''
                    touch "$out"
                  ''
                );
            # Regenerates the Pow vectors from Go like gocompat-vectors-math does.
            gocompat-vectors-pow =
              pkgs.runCommand "togi-gocompat-vectors-pow"
                {
                  nativeBuildInputs = [
                    pkgs.go_1_27
                  ]
                  ++ lib.optional pkgs.stdenv.hostPlatform.isLinux pkgs.qemu-user;
                  src = lib.fileset.toSource {
                    root = ./.;
                    fileset = lib.fileset.unions [
                      ./go.mod
                      ./go.sum
                      ./cmd
                      ./internal
                      ./tools
                      ./crates/togi-gocompat/testdata
                    ];
                  };
                }
                (
                  ''
                    cd "$src"
                    export HOME=$TMPDIR GOCACHE=$TMPDIR/go-cache GOPROXY=off GOTOOLCHAIN=local CGO_ENABLED=0
                    go build -o $TMPDIR/gocompat-vectors ./tools/gocompat-vectors
                    mkdir $TMPDIR/vectors
                  ''
                  + (
                    if pkgs.stdenv.hostPlatform.isLinux then
                      ''
                        $TMPDIR/gocompat-vectors pow-amd64-fma $TMPDIR/vectors
                        GODEBUG=cpu.fma=off $TMPDIR/gocompat-vectors pow-amd64-nofma $TMPDIR/vectors
                        $TMPDIR/gocompat-vectors pow-arm64 $TMPDIR/vectors
                        for f in pow-amd64-fma.txt pow-amd64-nofma.txt pow-arm64.txt; do
                          cmp $TMPDIR/vectors/$f crates/togi-gocompat/testdata/$f
                        done
                      ''
                    else
                      ''
                        $TMPDIR/gocompat-vectors pow-arm64 $TMPDIR/vectors
                        cmp $TMPDIR/vectors/pow-arm64.txt crates/togi-gocompat/testdata/pow-arm64.txt
                      ''
                  )
                  + ''
                    touch "$out"
                  ''
                );
            module = import ./nix/module-test.nix {
              pkgs = inputs.nixpkgs.legacyPackages.x86_64-linux;
              hostPkgs = pkgs;
              package = inputs.self.checks.x86_64-linux.package;
            };
          }
          // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux comparison.checks
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
