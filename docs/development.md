# Development

Read this guide when setting up a checkout, choosing checks, adding a crate, changing platform-gated code or working with a pull request stack. [AGENTS.md](../AGENTS.md) owns the everyday workflow, Rust idioms, testing and writing rules; this guide owns their on-demand development mechanics.

## Shell and checks

Enter the dev shell with `nix develop`, or `direnv allow` once per checkout. Recipes using Cargo, Nushell, DuckDB or `gh` stop with that hint outside it. `just hardware` enters it itself because `sudo` drops the environment; run it only on the target machine ([provisioning](howto.md#host-lock-and-delegated-hardware-tests)). Set `CARGO_TARGET_DIR` according to the [per-worktree rule](../AGENTS.md#commands).

Run heavy commands through the host job queue where your host has one; otherwise run them bare ([ADR 0086](adr/0086-heavy-runs-wait-for-memory.md)).

Choose checks by the action:

- While editing: `just test`, `just focus <crate>` or `just typecheck <crate>`.
- Before every push: `just gate`, except review fixes pushed through `just push-fix`. A pull request's first push uses `just ship`, which formats, gates and pushes. Commit first: it refuses a dirty tree, `main`, a detached HEAD or a `gh stack` layer. It force-pushes with lease after a rebase.
- For changes to `flake.nix` or `nix/`: also run `just check` before shipping; it builds every flake check supported by the host.
- During review: `just push-fix` has the same refusals as `just ship` and requires an existing remote branch. It formats, lints, builds the `nextest-<group>` checks, runs the heavy checks as the gate does and publishes what it built, then pushes.
- Before landing: `just land N` waits for `check` and `review` on the head and resolved review threads, then reports readiness. It fails at once when the pull request conflicts with its base, since GitHub runs no pull request CI until the conflict is resolved. Neither it nor the push recipes merges.
- For a full CI run on `main`: `just check-main` dispatches and follows it. Release dispatches the full check on the shipping head ([ADR 0074](adr/0074-pull-request-ci-gates-landing-on-main.md)); a check whose published output is cached counts as passed.

The gate covers pull request CI's checks except `package` and cargo-deny's advisory audit, which are flake checks only. It runs in two stages without `--keep-going`, so the first failure stops it and a stage 1 failure skips stage 2. Stage 1 builds the `fmt` flake check, runs `just lint` (Clippy through Cargo), then one `nix build` of the `module`, `changes`, `doc-links`, `board-test` and `sql-test` checks and every `nextest-<group>` check (the same ones CI builds, so Nix reruns only groups whose inputs changed), then the Cargo doctests, cargo-deny's bans, licenses and sources, and cargo-shear. Stage 2, on Linux only, is one `nix build` of the VM tests; without writable `/dev/kvm` it builds nothing and explains that VMs were skipped. Off Linux it skips stage 2 and explains that pull request CI builds it. Nix's own limits per invocation (`max-jobs`, `cores`) apply to each build. The simulator suite is an on-demand research tool, not a gate check; ordinary tests may use short in-memory simulations ([ADR 0087](adr/0087-simulations-are-research-tools-not-checks.md)). Review fixes through `just push-fix` build the `nextest-<group>` checks and then the same stage 2 as separate steps, and both publish afterwards.

After those builds, the gate and the review-fix path publish ([ADR 0085](adr/0085-the-gate-publishes-and-pull-request-ci-runs-every-check.md)): on x86_64 Linux they `cachix push togi` the output of every flake check of the tree that is in the local store, so pull request CI finds it in the Cachix cache and builds no nextest group or VM check for that head. A pull request the gate did not publish, such as a Dependabot one, builds them in CI. The push needs a token on the host: the `authToken` that `cachix authtoken` stored in `${XDG_CONFIG_HOME:-~/.config}/cachix/cachix.dhall`, or `CACHIX_AUTH_TOKEN` in the environment as an override. The recipe tests for presence and never prints the token. Without one the gate still runs every check and prints one line saying nothing was published.

The VM tests are four grouped checks, `vm-grub`, `vm-runtime`, `vm-display` and `vm-efi` (`nix/vm-test.nix`). `vm-grub` and `vm-runtime` run configuration cohorts as named subtests in sequence in one VM: each cohort activates its complete generation and reboots, and host lock and trial scope share one configuration, and day observer runs first and emergency stop after an explicit reset of the units, state files and fake SMU; scenarios keep the crashes and reboots their own proofs need. `vm-display` (OCR and GPU) and `vm-efi` (EFI firmware) are separate single-scenario VMs. CI, the gate and the release find VM checks by whether a flake check has a `driver`, so adding a scenario or a check needs no list edit. [Benchmarking](benchmarking.md#what-each-check-proves) lists what each proves and does not.

`just board-test` runs the Nushell board's fixed-fixture assertions without GitHub access; `just sql-test` does the same for the SQL views over fixed state directories. Both run in the gate and their flake checks; review fixes run them when `tools/board/` or `tools/sql/`, respectively, changed. The `doc-links` flake check runs in the gate and review-fix path and rejects relative Markdown links to missing files or headings.

The default recovery witnesses in `togi-session` cover interrupted failure attribution, between-trial downtime and day-profile register settlement; the short-night integration checks independent `ReadState` replay. `togi-simrun` also keeps a two-boot warm/cold comparison of exact journal and state bytes. These bounded checks pin selected recovery boundaries, not every position in a full recovery sweep.

## Crates and toolchain

A new crate needs a `Cargo.toml` with `[lints] workspace = true` and registration in `nextestGroups` in `flake.nix`; evaluation rejects missing registrations. Register fixtures read outside `crates/` in `crateFixtures` there. The exact toolchain in `rust-toolchain.toml` changes only in its own pull request.

The package, simulator and VM checks are built from production source only, so a test-only edit leaves their derivations alone. A module that compiles only under `cfg(test)` is test-only when its file is named `tests.rs` or `*_tests.rs`, or sits in a `tests` or `*_tests` directory under `src`; any other one is listed in `crates/test-only.txt`, which `nix/sources.nix` and the journal's snapshot fingerprint both read. Listing a production file there fails its build. The whole-tree `doc-links` check rejects listed paths that do not exist; the journal's nextest tests check fingerprint coverage against files present in their group's source.

`just lint` and the gate run Clippy with `--all-targets` for the host with default features, then all features (compiling, not running, hardware tests), and all features for the other supported target. The `clippy` flake check does the same. Pull request CI selects every flake check, builds those missing from the Cachix cache, and so runs what the gate runs, per the [CI policy](adr/0085-the-gate-publishes-and-pull-request-ci-runs-every-check.md) (which supersedes [ADR 0074](adr/0074-pull-request-ci-gates-landing-on-main.md)'s exclusion of heavy checks) and [dependency-layer groups](adr/0075-nextest-runs-in-dependency-layer-groups.md); inspect the flake for the current check inventory.

The [Rust guidance](../AGENTS.md#rust) leaves complexity to reviewers. The old Go cognitive-complexity limit was not ported: Clippy's `cognitive_complexity` documentation says it does not measure understandability; its default threshold mostly flagged test assertion sequences ([#874](https://github.com/shgew/togi/issues/874)).

## Platform-gated code and cross-lint

Development supports x86_64-linux and aarch64-darwin; hardware runs and NixOS VM tests require Linux. There is no macOS CI ([ADR 0061](adr/0061-no-macos-ci.md)). `just lint` and the gate cross-lint with `cargo clippy --target aarch64-apple-darwin` on Linux and `--target x86_64-unknown-linux-gnu` on macOS. Keep this check. A pull request touching `cfg(target_os = ...)` or `cfg(target_vendor = ...)` says "not built on macOS" unless it was built there. File and fix macOS breakage like any other bug.

Cross-lint runs C build scripts (`alloca`, through Criterion) for the other target. The flake sets `CC_<target>` and `AR_<target>` to unwrapped Clang and `llvm-ar` in both the dev shell and the Clippy check. A C dependency needs no further setup only while it builds with these tools without SDK headers.

Linux-only code has `#[cfg(target_os = "linux")]` and a non-Linux counterpart returning an unsupported error, as in `togi-detect` and `togi-trial`. Adjacent kernel-independent parsers and fixture logic use `#[cfg(any(test, target_os = "linux"))]` so tests also run on macOS. Only real-process, inotify and other kernel-behavior tests, or tests of Linux-only code such as the trial supervisor harness, stay Linux-gated.

## Pull request stacks

Stack only under the merge-unit rule in [Workflow](../AGENTS.md#workflow). Use [`gh stack`](https://github.com/github/gh-stack), installed with `gh extension install github/gh-stack`, for every stack. Each layer has its own branch and pull request based on the layer below; pass that branch as BASE to `just claim` when creating the next layer. Lower layers hold their dependants' prerequisites. Every layer passes `just gate` independently.

Open with `gh stack submit`, keep current with `gh stack sync`, and merge with `gh stack merge` only under the owner's merge authorization; never merge layers individually by hand. A lower-layer fix restacks and re-reviews every layer above it. During review, follow the [review command's frozen-base and restacking procedure](../.omp/commands/review-pr.md), not a routine sync onto moving `main`.

## Review integration

To invoke independent review in omp, use `/review-pr [PR numbers or URLs… | all]` ([review command](../.omp/commands/review-pr.md)); no arguments selects all open non-draft pull requests. The `.omp/rules/review-after-open.md` reminder runs only in the main session, not subagents or review coordinators.

GitHub identities follow [Workflow](../AGENTS.md#workflow). To publish the review record and `review` check through `just bot`, set `ROBOTOGI_KEY_FILE` to the App PEM private-key path in the environment.
