# Development

Read this guide when setting up a checkout, choosing checks, adding a crate, changing platform-gated code or working with a pull request stack. [AGENTS.md](../AGENTS.md) owns the everyday workflow, Rust idioms, testing and writing rules; this guide owns their on-demand development mechanics.

## Shell and checks

Enter the dev shell with `nix develop`, or `direnv allow` once per checkout. Recipes using Cargo, Nushell, DuckDB or `gh` stop with that hint outside it. `just hardware` enters it itself because `sudo` drops the environment; run it only on the target machine ([provisioning](howto.md#host-lock-and-delegated-hardware-tests)). Each worktree builds in its own `target/` using Cargo's default ([per-worktree rule](../AGENTS.md#commands)).

Run heavy commands through the host job queue where your host has one; otherwise run them bare ([ADR 0086](adr/0086-heavy-runs-wait-for-memory.md)).

Choose checks by the action:

- While editing: `just test`, `just focus <crate>` or `just typecheck <crate>`.
- Before every push: `just gate`, except review fixes pushed through `just push-fix`. A pull request's first push uses `just ship`, which formats, gates and pushes. Commit first: it refuses a dirty tree, `main`, a detached HEAD or a `gh stack` layer. It force-pushes with lease after a rebase.
- For changes to `flake.nix` or `nix/`: also run `just check` before shipping; it builds every local-tier flake check supported by the host, including `package` and cargo-deny's advisory audit beyond the gate.
- During review: `just push-fix` has the same refusals as `just ship` and requires an existing remote branch. It formats, lints, builds `quality` and the local `nextest` check and publishes what it built, then pushes.
- Before landing: `just land N` waits for `check` and `review` on the head and resolved review threads, then reports readiness. It fails at once when the pull request conflicts with its base, since GitHub runs no pull request CI until the conflict is resolved. Neither it nor the push recipes merges.
- For a full CI run on `main`: `just check-main` dispatches and follows it. Release dispatches the full check on the shipping head ([ADR 0074](adr/0074-pull-request-ci-gates-landing-on-main.md)); a check whose published output is cached counts as passed.

The local gate stops at the first failure. It builds `quality` (formatting, changelog fragments, documentation links, board, coverage-selection and SQL fixtures, and the NixOS module), runs `just lint` (Clippy through Cargo), then builds the local `nextest` check, followed by Cargo doctests, cargo-deny's bans, licenses and sources, and cargo-shear. It never builds CI-tier checks, even on Linux with writable KVM. `just push-fix` also builds `quality` and local nextest checks. `just check` remains because the gate does not build its full local inventory: notably the package and advisory audit.

The gate and review-fix path publish built outputs ([ADR 0085](adr/0085-the-gate-publishes-and-pull-request-ci-runs-every-check.md), amended by [ADR 0089](adr/0089-heavy-checks-run-on-ci-runners-not-on-the-contributors-machine.md)): on x86_64 Linux they `cachix push togi` every flake check output of the tree already in the local store. Pull request CI and the release's exact-head dispatch still select **every** flake check, including the CI tier, and build those absent from Cachix. The local skip does not remove a PR merge gate. Publication needs the `authToken` stored by `cachix authtoken` in `${XDG_CONFIG_HOME:-~/.config}/cachix/cachix.dhall`, or `CACHIX_AUTH_TOKEN` as an override. Without a token local checks still run and print that nothing was published.

Pull request CI evaluates the pull request merged into current `main` and builds every check whose output the cache lacks. The gate publishes the outputs of the branch it built, so a check whose inputs `main` changed since the branch's base can miss the published output and be rebuilt; a Rust change on `main` can rerun each nextest concern whose source closure it touches. Rebase the branch onto the latest `main` right before `just ship` and before each `just push-fix`; the re-review reads the rebase through `git range-diff`, as the [review command](../.omp/commands/review-pr.md) says.

A check belongs to the **CI tier** when its derivation carries `passthru.tier = "ci"` (read as `checks.<system>.<name>.tier or null`); all other checks are local. Set this attribute when adding a heavy check: `gate`, `push-fix` and `check` need no new skip list. Do not make local checks depend on CI-tier checks. CI and release discovery do not filter the tier.

The VM tests are the first CI-tier checks: `vm-grub`, `vm-runtime`, `vm-display` and `vm-efi` (`nix/vm-test.nix`). Run them explicitly with `just vm`, or select names with `just vm vm-grub vm-runtime`; this needs Linux and writable `/dev/kvm`. The opt-in recipe and CI's VM-specific setup discover checks by their `driver` attribute. GRUB and runtime checks activate configuration cohorts and reboot between them; display and EFI remain separate VMs. Their real-boot safety evidence runs on every PR in CI, while the [ADR 0089 safety-evidence ledger](adr/0089-heavy-checks-run-on-ci-runners-not-on-the-contributors-machine.md#safety-evidence-ledger) names default mechanism witnesses. [Benchmarking](benchmarking.md#what-each-check-proves) records the check scopes.

The `quality` flake check groups seven private members: `fmt`, `changes`, `doc-links`, `board-test`, `cover-test`, `sql-test` and `module`. Its output links to each member's independently cached output; failures retain the member's derivation name and logs. Both the gate and review-fix path build the whole bundle, and Nix reruns only members whose inputs changed. `just board-test`, `just cover-test` and `just sql-test` remain focused fixture commands without GitHub access; `just changes` checks changelog fragments. The documentation-link member rejects relative Markdown links to missing files or headings.

The default recovery witnesses in `togi-session` cover interrupted failure attribution, between-trial downtime and day-profile register settlement; the short-night integration checks independent `ReadState` replay. `togi-simrun` also keeps a two-boot warm/cold comparison of exact journal and state bytes. These bounded checks pin selected recovery boundaries, not every position in a full recovery sweep.

`just test` and `just cover <base>` select the same product crates as the local `nextest` flake check: the shipped closure plus simulator and dashboard test support. `nextest-tools` runs unshipped tool and research tests in the CI tier. Use `just focus <crate>` while editing any one crate, including a tool, or `just check-one nextest-tools` for the complete tool concern. The product and tool checks retain only their crates' external fixtures ([ADR 0090](adr/0090-nextest-runs-by-concern.md)).

`just cover BASE [CRATE ...]` adds explicitly named workspace packages to that product set, validates names against `cargo metadata` before coverage starts, and deduplicates selections. For example, `just cover origin/main togi-tool-release togi-facts` runs product tests plus those packages' own tests, not the whole tool concern. Cargo also compiles their dependencies; that does not select dependency or consumer test suites. The `cover-test` member of `quality` runs the argument-selection regressions against fixed metadata.

Review coverage names the packages owning included changed tool Rust files, including tool-only libraries, and any consumers whose tests exercise changed library behavior; see [the review procedure](../.omp/commands/review-pr.md). Explicit selection closes the tool changed-line evidence gap without an automatic affected-package policy or a default whole-workspace run. Tool-only reviews deliberately retain the product run. Keep stderr test diagnostics separate from the stdout report imported by `just reviews snapshot --cover`; record the named extras, remaining uncovered/not-built paths and the reading or other checks that establish their behavior. Unmeasured integration-test source and platform/feature-excluded code still require an honest reading judgment. Coverage has no percentage target or ratchet and does not replace behavior assertions or complete CI checks.

The two exhaustive session cut sweeps and the long runner warm/cold matrix are excluded by nextest's default filter in both local and CI profiles. The `nextest-recovery` flake check selects those excluded tests from `togi-session` and `togi-simrun`, one test at a time, with two inner cut workers. It is CI tier: PR CI and release run it, while the local gate, check and review-fix paths skip it. Run `just recovery` to build it explicitly. The full-window sweep assertions remain unchanged; this changes the frequency of exhaustive evidence, not its scope.

The bench's orchestration and real-process tests use a simulated history through the first ended trial, with a command shutdown for successful fake commands and the same prefix without shutdown for censored runs. The snapshot cross-reader property generates bounded nights of fifteen to ninety minutes with varied seeds, core counts, stable/crashing machines and snapshot cuts; the crash/reload case stops at the first detected crash, while boot-cap measurement tests retain the longer histories their assertions need. These fixtures still exercise journal parsing, snapshot sealing, replay, measurement and audit rather than replacing those stages.

## Crates and toolchain

A new crate needs a `Cargo.toml` with `[lints] workspace = true`. The `productCrates` closure in `flake.nix` discovers dependencies of `togi`, `togi-simrun` and `togi-watchtest`; register an unshipped crate outside that closure in `toolCrates`. Evaluation rejects missing, repeated or unknown registrations. Register fixtures read outside `crates/` in `crateFixtures` there, for the crate whose tests read them. The exact toolchain in `rust-toolchain.toml` changes only in its own pull request.

`togi`'s ordinary command integration tests compile as `commands`, with command modules such as `cli`, `read` and `reset`. Filter them with `cargo nextest run -p togi -E 'binary(commands) & test(/^read::/)'`, not the former per-command binary names. `run` remains its own binary because its panic and terminal tests re-execute exact helper names; `vm_recovery` remains the executable the VM checks install. Both remain in the default test selection.

The package and VM checks are built from production source only, so a test-only edit leaves their derivations alone. A module that compiles only under `cfg(test)` is test-only when its file is named `tests.rs` or `*_tests.rs`, or sits in a `tests` or `*_tests` directory under `src`; any other one is listed in `crates/test-only.txt`, which `nix/sources.nix` and the journal's snapshot fingerprint both read. Listing a production file there fails its build. The whole-tree documentation-link member of `quality` rejects listed paths that do not exist; the journal's nextest tests check fingerprint coverage against files present in their source.

`just lint` and the gate run Clippy with `--all-targets` for the host with default features, then all features (compiling, not running, hardware tests), and all features for the other supported target. The `clippy` flake check does the same. Pull request CI selects every flake check, builds those missing from the Cachix cache, and retains the [CI policy](adr/0085-the-gate-publishes-and-pull-request-ci-runs-every-check.md) with [nextest by concern](adr/0090-nextest-runs-by-concern.md); inspect the flake for the current check inventory.

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
