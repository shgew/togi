# Development

Read this guide when setting up a checkout, choosing checks, adding a crate, changing platform-gated code or working with a pull request stack. [AGENTS.md](../AGENTS.md) owns the everyday workflow, Rust idioms, testing and writing rules; this guide owns their on-demand development mechanics.

## Shell and checks

Enter the dev shell with `nix develop`, or `direnv allow` once per checkout. Recipes using Cargo, Nushell, DuckDB or `gh` stop with that hint outside it. `just hardware` enters it itself because `sudo` drops the environment; run it only on the target machine ([provisioning](howto.md#host-lock-and-delegated-hardware-tests)). Each worktree builds in its own `target/` using Cargo's default ([per-worktree rule](../AGENTS.md#commands)).

Choose checks by the action:

- While editing: `just test`, `just focus <crate>` or `just typecheck <crate>`.
- Before every push: `just gate`, except review fixes pushed through `just push-fix`. A pull request's first push uses `just ship`, which formats, gates and pushes. Commit first: it refuses a dirty tree, `main`, a detached HEAD or a `gh stack` layer. It force-pushes with lease after a rebase.
- For changes to `flake.nix` or `nix/`: also run `just check` before shipping; it builds every local-tier flake check supported by the host, including `package` and cargo-deny's advisory audit beyond the gate.
- During review: `just push-fix` has the same refusals as `just ship` and requires an existing remote branch. It formats, lints, builds `quality` and the local `nextest` check and publishes what it built, then pushes.
- Before landing: `just land N` waits for `check` and `review` on the head and resolved review threads, then reports readiness. It fails at once when the pull request conflicts with its base, since GitHub runs no pull request CI until the conflict is resolved. Neither it nor the push recipes merges.
- For a full CI run on `main`: `just check-main` dispatches and follows it. Release dispatches the full check on the shipping head ([CI and release gates](#ci-and-release-gates)); a check whose published output is cached counts as passed.
- For named hermetic checks: `just check-one <names>` builds exactly those flake checks for the host and streams their logs, including CI-tier checks when explicitly named.

The local gate stops at the first failure. It builds `quality` (formatting, changelog fragments, documentation links, board, coverage-selection and SQL fixtures, and the NixOS module), runs `just lint` (Clippy through Cargo), then builds the local `nextest` check, followed by Cargo doctests, cargo-deny's bans, licenses and sources, and cargo-shear. It never builds CI-tier checks, even on Linux with writable KVM. `just push-fix` also builds `quality` and local nextest checks. `just check` remains because the gate does not build its full local inventory: notably the package and advisory audit.

The gate and review-fix path [publish built outputs](#check-cache): on x86_64 Linux they `cachix push togi` every flake check output of the tree already in the local store. Pull request CI and the release's exact-head dispatch still select **every** flake check, including the CI tier, and build those absent from Cachix. The local skip does not remove a PR merge gate.

Pull request CI evaluates the pull request merged into current `main` and builds every check whose output the cache lacks. The gate publishes the outputs of the branch it built, so a check whose inputs `main` changed since the branch's base can miss the published output and be rebuilt; a Rust change on `main` can rerun each nextest concern whose source closure it touches. Rebase the branch onto the latest `main` right before `just ship` and before each `just push-fix`; the re-review reads the rebase through `git range-diff`, as the [review command](../.omp/commands/review-pr.md) says.

A check belongs to the **CI tier** when its derivation carries `passthru.tier = "ci"` (read as `checks.<system>.<name>.tier or null`); all other checks are local. Set this attribute when adding a heavy check: `gate`, `push-fix` and `check` need no new skip list. Do not make local checks depend on CI-tier checks. CI and release discovery do not filter the tier.

The CI tier (VM checks, `nextest-tools` and the recovery sweeps) stays out of the local loop because pull request CI runs them on every head; local runs would repeat minutes of work per push without new evidence. A successful gate does not claim that every PR check ran locally. Publishing already-built CI-tier outputs remains useful and does not build them implicitly.

Rejected: running `just check` before every push repeats CI's hermetic work on the same inputs; removing it loses the package and advisory audit beyond the gate. KVM-dependent defaults make identical edits acquire different checks merely because a machine can run VMs. A hand-maintained skip list would duplicate tier ownership. Skipping all local checks for review fixes trades cheap formatting, lint and test evidence for a CI round trip; running the full gate for every fix repeats work CI will check. A special gate only for safety-crate fixes adds another selection rule without new coverage.

The VM tests are CI-tier checks: `vm-grub`, `vm-runtime`, `vm-display` and `vm-efi` (`nix/vm-test.nix`). Run them explicitly with `just vm`, or select names with `just vm vm-grub vm-runtime`; this needs Linux and writable `/dev/kvm`. The opt-in recipe and CI's VM-specific setup discover checks by their `driver` attribute. GRUB and runtime checks activate configuration cohorts and reboot between them; display and EFI remain separate VMs. Their real-boot safety evidence runs on every PR in CI, while the [safety-evidence ledger](#safety-evidence-ledger) names default mechanism witnesses. [Benchmarking](benchmarking.md#what-each-check-proves) records the check scopes.

The `quality` flake check groups seven private members: `fmt`, `changes`, `doc-links`, `board-test`, `cover-test`, `sql-test` and `module`. Its output links to each member's independently cached output; failures retain the member's derivation name and logs. Both the gate and review-fix path build the whole bundle, and Nix reruns only members whose inputs changed. `just board-test`, `just cover-test` and `just sql-test` remain focused fixture commands without GitHub access; `just changes` checks changelog fragments. The documentation-link member rejects relative Markdown links to missing files or headings.

The default recovery witnesses in `togi-session` cover interrupted failure attribution, between-trial downtime and day-profile register settlement; the short-night integration checks independent `ReadState` replay. `togi-simrun` also keeps a two-boot warm/cold comparison of exact journal and state bytes. These bounded checks pin selected recovery boundaries, not every position in a full recovery sweep.

### Nextest by concern

`just test` and `just cover <base>` select the same product crates as the local `nextest` flake check: the shipped closure plus simulator and dashboard test support. `nextest-tools` runs unshipped tool and research tests in the CI tier. Use `just focus <crate>` while editing any one crate, including a tool, or `just check-one nextest-tools` for the complete tool concern. The flake is the single owner of product selection.

Each check keeps its tested crates whole, their test dependency closure as code, nextest's configuration and only the external fixtures registered for those crates in `crateFixtures`. Narrowed sources retain stubs for omitted targets, including discovered ones, so workspace metadata and named-binary filters remain valid. Workspace manifests, `Cargo.lock`, the toolchain and nextest profiles invalidate both concerns. Product fixtures must not require the complete research corpus. No test or assertion is dropped by separating concerns; tool correctness remains an automatic PR and release gate.

Rejected: dependency-layer checks and compatibility aliases retain overlapping closures and duplicate scheduling instead of matching product and tooling concerns. One check per crate repeatedly compiles shared closures and adds CI jobs; building once and partitioning only the test run leaves compilation on the critical path. A shared workspace-library artifact serializes checks behind another derivation and stores another target archive. Selecting crates or tests from a Git diff makes the result depend on a base commit rather than represent the complete hermetic concern. Deleting tool tests or deferring them to release leaves development tooling unchecked on a PR.

`just cover BASE [CRATE ...]` adds explicitly named workspace packages to that product set, validates names against `cargo metadata` before coverage starts, and deduplicates selections. For example, `just cover origin/main togi-tool-release togi-facts` runs product tests plus those packages' own tests, not the whole tool concern. Cargo also compiles their dependencies; that does not select dependency or consumer test suites. The `cover-test` member of `quality` runs the argument-selection regressions against fixed metadata.

Review coverage names the packages owning included changed tool Rust files, including tool-only libraries, and any consumers whose tests exercise changed library behavior; see [the review procedure](../.omp/commands/review-pr.md). Explicit selection closes the tool changed-line evidence gap without an automatic affected-package policy or a default whole-workspace run. Tool-only reviews deliberately retain the product run. Keep stderr test diagnostics separate from the stdout report imported by `just reviews snapshot --cover`; record the named extras, remaining uncovered/not-built paths and the reading or other checks that establish their behavior. Unmeasured integration-test source and platform/feature-excluded code still require an honest reading judgment. Coverage has no percentage target or ratchet and does not replace behavior assertions or complete CI checks.

The two exhaustive session cut sweeps and the long runner warm/cold matrix are excluded by nextest's default filter in both local and CI profiles. The `nextest-recovery` flake check selects those excluded tests from `togi-session` and `togi-simrun`, one test at a time, with two inner cut workers. It is CI tier: PR CI and release run it, while the local gate, check and review-fix paths skip it. Run `just recovery` to build it explicitly. The full-window sweep assertions remain unchanged; this changes the frequency of exhaustive evidence, not its scope.

The bench's orchestration and real-process tests use a simulated history through the first ended trial, with a command shutdown for successful fake commands and the same prefix without shutdown for censored runs. The snapshot cross-reader property generates bounded nights of fifteen to ninety minutes with varied seeds, core counts, stable/crashing machines and snapshot cuts; the crash/reload case stops at the first detected crash, while boot-cap measurement tests retain the longer histories their assertions need. These fixtures still exercise journal parsing, snapshot sealing, replay, measurement and audit rather than replacing those stages.

## CI and release gates

The flake's checks are the one inventory. `check.yml` runs on pull requests and `workflow_dispatch`, not pushes to `main`. Both modes select every `checks.x86_64-linux` attribute, regardless of tier. Evaluation checks each output's `https://togi.cachix.org/<store-hash>.narinfo`; an HTTP success counts as a pass without starting a build. In PR mode, uncached `quality`, `deny`, `shear` and `package` share one `light` build with `--keep-going`; other uncached checks get separate jobs. Dispatch mode gives every uncached check its own job. The aggregate `check` requires successful evaluation and every required build, even when all checks were cached. A newer PR push cancels the older run; dispatch runs are not cancelled by it.

PR CI gates landing on `main`. The release dispatches the full workflow on `main` and waits for the `workflow_dispatch` run whose `head_sha` is the exact commit it builds on, reusing an existing dispatch for that SHA. Its separate build of the release commit's package, `quality` and all nextest checks does not replace this dispatch. The target machine follows release tags, never bare `main` commits. [Releasing](releasing.md) owns the release commands, refusal conditions and publication mechanics.

No CI job runs on macOS. The target platform is x86_64 Linux; macOS is a development platform. Native macOS compilation repeated minutes of work after merge without gating PRs. Cross-target Clippy remains required, but proves type checking, not macOS linking, kernel behavior or test execution. A macOS failure can therefore merge and release; report platform evidence honestly and fix such failures like other bugs. Darwin flake outputs remain available locally, but CI builds and publishes none.

Rejected:

- VM checks only at release, or fast PR checks followed by slow checks on `main`: a regression can land before the real-boot evidence. Nightly runs are detached from the introducing change and do not prove the exact release head; pinned inputs do not change between pushes.
- A hand-kept PR inventory or changed-path selection: it duplicates the flake's exact input tracking and drifts.
- One `nix flake check` CI job: independent checks share one runner's CPUs instead of building in parallel. Cheap-job packing keeps the overhead saving without serializing the expensive checks.
- Larger paid runners: cost replaces duplicate work rather than removing it. Self-hosted runners: public PR code would run on private infrastructure and availability would depend on it.
- Native macOS checks on every PR or only on `main`: the former repeats Linux evidence with a long compile, while the latter delays releases without gating landing. Linux arm64 runners reach neither Darwin code nor its kernel or stdenv and are not the product's target.

## Check cache

Flake checks are hermetic: no `target/`, `CARGO_HOME` registry/git cache or other ambient machine state enters the sandbox through `extra-sandbox-paths`, `__noChroot`, impure derivations or `--impure`. Rust builds use the locked dependencies and crane dependency artifacts; reuse is a derivation with unchanged declared inputs. Board fixtures use fixed responses and a fixed clock, without `gh` or network access. Faster dev-shell recipes have narrower claims than hermetic checks.

Checks build the package with revision `dev`, so a new commit alone does not invalidate them. `packages.default` retains the commit revision so installed builds remain distinguishable in journals. The package check smoke-tests the binary and completions; nextest owns the tests. Narrowed check sources invalidate only the evidence that reads them.

CI substitutes from the public Cachix cache `togi` and publishes only each successful check's output and closure, not every path built along the way. Checks install no unused Cargo target archives; `cargo-deps` is its own public check so the shared dependency artifact is published explicitly. The `quality` link farm retains its private members' independent reuse and publishes them through its closure.

`gate` and `push-fix` publish every check output for the tree already in the local store on x86_64 Linux, including explicitly built CI-tier outputs and dependency artifacts. They do not build outputs just to publish them; Cargo Clippy and doctests need not have corresponding flake outputs present. Publication uses `CACHIX_AUTH_TOKEN` or the `authToken` saved by `cachix authtoken` in `${XDG_CONFIG_HOME:-~/.config}/cachix/cachix.dhall`, without printing the token. With no token, local checks still run and report that nothing was published. CI likewise reads without a token (including Dependabot runs); a failed cache push fails neither the gate nor CI, which builds outputs it cannot find.

A cache hit counts as passed regardless of whether CI or a local gate published it: it skips the run, including a VM boot, rather than providing a fresh observation. A token holder can publish any content under a check's store path, so token custody is a trust boundary. Opting into substitution trusts every path the cache serves for any build, not just togi checks.

Local cache trust is optional and configured per installation, never by the flake's `nixConfig`. Add these to the system Nix configuration, or run `cachix use togi` as a trusted user:

```text
extra-substituters = https://togi.cachix.org
extra-trusted-public-keys = togi.cachix.org-1:1EZ2zQlDkNhHZmROZzR0n0/CcYGLPfAmPs+VaGL72LU=
```

Restart the Nix daemon after changing its system configuration. Without opt-in, local checks build rather than substitute these outputs.

Rejected:

- Ambient build caches in the sandbox or an impure fast output counted as green: undeclared state invalidates the public cache's claim.
- CI-only cache access: local hermetic checks would repeat already-proved work. Flake `nixConfig`: it prompts users and cannot establish trust for an untrusted user.
- Publishing only from CI: it cannot spare CI work the local gate already built. Publishing nothing locally while selecting all checks repeats identical builds.
- Uploading every built store path or retaining unused target archives: VM systems, images and Cargo archives consume the cache without useful hits. A per-check GitHub Actions store cache or Magic Nix Cache cannot share PR-written results with the post-merge ref and adds restoration overhead.
- Dropping revision stamps: development builds become indistinguishable in journals. Serving prebuilt user packages as the goal: hits require the project's nixpkgs pin, duplicating runtime dependencies and fetching/evaluating another pin to avoid a small package build.

## Tests and simulations in memory

Ordinary tests and simulations use `togi-journal`'s `MemFs` and in-memory fakes of the `togi-machine` seams, with time from the injected `Clock` or Tokio's paused clock. They do no network I/O, start no processes, leave no process-wide state behind and never call `fsync`, directly or through production code. Inject sync failures or record durability ordering instead; production retains its per-event durability.

Disk is permitted only for:

- Kernel-dependent filesystem or process behavior in a named `real_fs_` or `real_process_` test, with files confined to its own temporary directory and sync disabled; tests behind the `hardware` Cargo feature run by `just hardware`; and VM tests exercising the shipped binary's real durability.
- Requested output: a flag such as `--state-dir` or `--keep`, a single `togi-tool-sim` session whose announced state directory is its output, or a failing run retaining evidence for diagnosis.
- The [temporary-journal child protocol](#temporary-journals-for-research-children) below.

Put I/O behind a narrow interface only at a real boundary: filesystem, clock, processes, hardware or SMU. Each boundary has an in-memory fake; a type with no boundary gets no interface and tests use its real implementation. A new boundary brings its fake, so logic above it needs no disk, process or real clock. Every change looks for test and simulation speed wins in the code it touches; reviewers flag needless disk writes, processes or real-clock use ([review criteria](../REVIEW.md)). New exceptions require a recorded decision with measurements. [Rust test execution](#rust-test-execution) owns concurrency.

Rejected: temporary directories for every test charge file creation, writes and cleanup to logic that needs no kernel, can leave files after failed cleanup and can pay for sync. An interface for every replaceable type adds indirection and a fake that can drift, so the test checks the fake. Keeping only a terse rule in `AGENTS.md` hides why its exceptions exist; this section owns the rule and rationale.

### Temporary journals for research children

Bench, `same`, `smoke` and `audit` children run with a temporary `--state-dir`; the parent reads and parses each journal after the child exits. This is the exception to in-memory simulation, not permission for other tests to write files. The child protocol remains its exit status and state directory, including when comparing a base tree's simulator. `--keep` retains run directories; `same` reruns a differing session with files kept, so the journal itself is the evidence.

The dominant cost was per-event `state.json` publication, removed in [#871](https://github.com/shgew/togi/pull/871). The [#868 measurements](https://github.com/shgew/togi/issues/868#issuecomment-6127558468) found a default session's writes fell from 347.6 MB to 5.04 MB; reading and parsing journals cost about 1% of the development split's CPU. After that fix, in-memory children were within CPU noise and used up to 30 MiB more resident memory. These measurements used scratch storage; they do not promise the same disk cost everywhere. Revisit the protocol only with new end-to-end measurements if parent reads become material.

Rejected: events on stdout or a dedicated pipe still require parsing the same bytes and leave a crashed child without its journal; the pipe also adds a protocol. Digest-only children move metric computation into the child and require the base tree to speak the digest protocol. In-memory children showed no CPU saving after the publication fix and used more memory. Temporary journals retain crash evidence without that protocol change.

## Rust test execution

There is no race-detector check. Safe Rust's `Send`/`Sync` rejects data races; it does not prove absence of logic races, deadlock or time-of-check/time-of-use errors. Allowed unsafe scopes and dependencies remain outside that guarantee. [Rust rules](../AGENTS.md#rust) own unsafe-code restrictions and safety comments; concurrent behavior needs real-thread and process tests, with deterministic seams and simulations as additional evidence, never a replacement.

Nextest runs every selected test on every invocation, with no result cache or shuffle. Each test has its own process, preventing shared in-memory state between tests but not coupling through files, clocks, ports or other resources. Cargo's incremental dev-shell builds and hermetic dependency artifacts provide build reuse; `just typecheck <crate>` runs no tests.

Filesystem and process tests use nextest's default concurrency, own their temporary paths and children, and retain longer process-test timeouts. Only shared-hardware tests are serialized: SMU tests modify the same CPU, while scope tests share systemd and the product host lock. VM hardware invocations and `just hardware` remain single-threaded. A test group is a semaphore, not a classification label; a new cap requires a concrete shared-resource constraint or measured resource-exhaustion evidence. Libtest doctests share a process within their binary, unlike nextest.

Rejected:

- Miri or ThreadSanitizer: both require a second, nightly toolchain instead of the stable pin; Miri cannot exercise most process, signal and terminal behavior, and ThreadSanitizer checks data races rather than safe-code logic races.
- Shuffling Rust tests: nextest has no shuffle option, while libtest's is unstable and lacks nextest's per-test isolation.
- A custom nextest result cache: no supported hook or measured saving justifies maintaining a correct binary-and-fixture cache. Manual stress runs remain a tool for investigating flakes, not a default check.
- A separate concurrency cap merely for filesystem/process tests: owned temporary paths and children are not shared resources, and a cap greater than one cannot exclude a real shared-resource race.

## Work that waits on the target machine

Record target-machine verification still owed; never quietly declare it verified. VM tests cannot exercise the SMU mailbox, the accepted physical CPU or the real mprime and y-cruncher backends. Otherwise-green changes may merge while that evidence is pending, but the obligation remains visible and blocks a new release and updating the target machine's togi input. [Issues, Work that waits on the target machine](issues.md#work-that-waits-on-the-target-machine) owns the label, exact run instructions, merge wording, board queue and failure/closure mechanics; [Releasing](releasing.md) owns the workflow's refusal and publication retry.

Rejected: a separate pinned checklist duplicates issue state and needs manual updates; a run-retrospective issue hides obligations among history and cannot drive the queue or release refusal. Holding every PR open until hardware is available blocks reviewed changes and dependent work. A VM backend check cannot supply the missing SMU or CPU, hosted runners do not guarantee AVX-512, and mprime's license does not clearly allow publishing its binaries in the public check cache. Manually unlinking an issue before merge can silently lose the obligation; querying the label even on closed issues fails safe.

## Simulations and research

Simulations are on-demand research and development tools, not correctness checks. No flake check, local gate, review-fix path, PR CI or release check runs the session suite. Ordinary nextest tests, including short in-memory simulations, pin expected conclusions, warm/cold recovery equivalence, replay acceptance and invariant-audit behavior in their owning crates.

Run `just sim-suite`, `togi-tool-sim suite` and `check`, `just same`, `just smoke` or `just bench` when a change raises their research question, and cite the result only for that question. A tuner or session change altering simulated decisions calls for such evidence before review. Behavior only long sessions reach is discovered on demand, not by CI. [Benchmarking](benchmarking.md) owns research methodology and [Simulating](simulating.md) owns simulator mechanics.

Rejected: retaining sharded suite checks repeats long simulations whose correctness properties ordinary tests prove. Running the suite only at release still makes release wait for research and discovers regressions after landing. A small smoke subset remains long simulation in the gate instead of direct owning-crate proofs.

## Releases and changelog

The owner chooses when to start the manual release workflow. Starting it approves the release commit, the only direct push to `main`; the workflow takes no version input because changelog entries alone determine the bump. `version.txt` remains the version source because flake builds cannot see tags. [Releasing](releasing.md) owns preview, exact-head checks, fast-forward publication and retry mechanics.

Operator-visible shipped changes use [per-PR changelog fragments](../changes/README.md), whose format and rationale live there; docs and development tooling changes need none.

Rejected: a workflow-opened release PR gets no check run with the automatic token; a locally opened release PR adds a second step instead of one workflow checking exactly what it publishes. A version override creates a second bump authority. A standing release PR takes release timing away from the owner. GoReleaser adds machinery for binaries the project does not ship and does not own bumping, tagging or opening the release.

## Safety-evidence ledger

This table records the evidence that leaves the default local tier and the named default witnesses that remain. They prove mechanisms, not all VM environments or every physical power-loss outcome. Later tier migrations append rows here and name every moved test; no test or assertion is deleted by this decision.

| Evidence moved to CI tier | Proof retained in CI | Named default witnesses that remain |
|---|---|---|
| `vm-grub`: `tuning-boot`, `restart-limit`, `night` | Real PID1 watchdog/reset, GRUB selection and enter/reset/leave lifecycles; topology-specific scope execution | `crates/togi-session/src/whole.rs::a_night_continues_across_a_crash_reboot` continues the same night after a write crash/reboot. It does not replace bootloader integration. |
| `vm-runtime`: `host-lock`, `trial-scope`, `emergency-stop` | Installed root/delegated lock setup, deployed SIGKILL hook ordering, real systemd scope teardown, zero/readback before restart | `crates/togi-hostlock/src/tests.rs::real_process_excludes_until_it_exits`; `crates/togi-trial/src/tests/teardown.rs::teardown_shares_one_fifteen_second_budget_across_instances`; `crates/togi-session/src/restore/tests.rs::without_the_proof_that_nothing_runs_nothing_is_written`; `crates/togi-session/src/emergency/tests.rs::{a_gated_emergency_zeroes_every_core_reads_each_back_and_reports,an_interruption_of_any_emergency_access_keeps_the_original_error_and_appends_nothing}`; `crates/togi/tests/emergency_stop/mod.rs::a_stop_sweeps_then_zeroes_every_core_and_reads_each_back`. |
| `vm-runtime`: `day-observer` | Real peer credentials and service activation; journal reopen after VM reset | `crates/togi-journal/src/journal/tests.rs::an_event_is_one_write_then_one_fsync` pins the durability call order; `crates/togi-journal/tests/disk.rs::real_process_an_abrupt_exit_loses_no_acknowledged_event` retains acknowledged events across process death, not loss of the kernel page cache. |
| `vm-display`: `kmscon-display` | GPU/VT/OCR and installed display fallback | `crates/togi-watch/src/frames/frame_tests/bounds.rs::mapping::screens` and the other staged frame goldens/bounds remain local; they do not prove the real display integration. The safety mechanisms named in the other rows are unchanged. |
| `vm-efi`: `night-systemd-boot` | Real EFI variables, immutable attributes and systemd-boot enter/reset/leave lifecycle | The session crash/reboot witness above remains local; it does not prove EFI state restoration. |
| No product SMU transaction or receipt test moves | VM deployed integration remains additional evidence, not authority for the transaction rules; additional unshipped auditor evidence is listed below | `crates/togi-journal/src/journal/tests.rs::{a_receipt_follows_a_durable_intent_and_the_write_cites_it,a_receipt_is_refused_by_another_journal_and_by_a_reopened_one,a_receipt_from_before_a_reboot_is_refused}`; `crates/togi-session/src/write/tests.rs::a_readback_that_disagrees_is_durable_then_an_smu_dead_end`. |
| No reset-kind witness moves | Recovery sweeps exercise reset kinds as inputs; they are **not** credited with asserting reset-kind semantics | `crates/togi-simrun/tests/sim/crash.rs:43–135`: `a_power_button_reset_during_a_trial_is_a_conclusive_failure`, `a_thermal_trip_reset_during_a_trial_dead_ends_without_a_failure`, `a_thermal_trip_while_a_trial_runs_dead_ends`, `a_power_button_between_trials_is_inconclusive` assert the recorded reason, conclusiveness, failure presence/absence and dead-end behavior. |
| `crates/togi-tool-audit/tests/audit/writes.rs::{a_write_follows_its_intent_and_is_read_back,an_intent_authorizes_one_write_in_its_own_boot,a_reboot_may_interrupt_a_write_before_its_readback,a_readback_matches_the_write_it_cites,a_mismatching_readback_ends_the_session_in_an_smu_dead_end,a_mismatching_readback_of_a_set_all_write_leaves_its_other_cores_unread,a_day_application_is_a_boundary_for_the_write_protocol}` (all parameter cases) | The unshipped auditor diagnoses synthetic write-protocol violations; this is additional evidence, not the authority for SMU writes | `crates/togi-journal/src/journal/tests.rs::{a_receipt_follows_a_durable_intent_and_the_write_cites_it,a_receipt_is_refused_by_another_journal_and_by_a_reopened_one,a_receipt_from_before_a_reboot_is_refused}`; `crates/togi-session/src/write/tests.rs::a_readback_that_disagrees_is_durable_then_an_smu_dead_end`. |
| `crates/togi-tool-audit/tests/audit/snapshot.rs::{a_current_state_document_equals_a_fresh_fold_of_its_journal,a_current_document_that_disagrees_with_the_journal_is_a_violation}` | The unshipped auditor compares a published document with an independent journal fold | `crates/togi-session/src/resume/tests.rs::a_capsule_verifies_the_state_it_folded`; `crates/togi-session/src/whole.rs::a_night_continues_across_a_crash_reboot` retains independent replay locally. |
| `crates/togi-tool-sim/src/bin/togi-tool-sim/tests/resume.rs::a_session_resumed_from_recorded_memory_ends_where_an_uninterrupted_one_does` | Simulator-command reconstruction preserves journal and state output | `crates/togi-simrun/tests/sim/warm_cold.rs::a_bounded_crash_resume_has_identical_warm_and_cold_journal_and_state_bytes`; the product session crash/reboot witness above also remains local. |
| `crates/togi-tool-sim/tests/cli/main.rs::real_process_a_signal_stops_the_session_in_order` | The simulator command handles a real process signal and completes session shutdown in order | `crates/togi-session/src/restore/tests.rs::without_the_proof_that_nothing_runs_nothing_is_written`; `crates/togi-trial/src/tests/teardown.rs::teardown_shares_one_fifteen_second_budget_across_instances`. These prove product mechanisms, not the tool's signal adapter. |
| `crates/togi-session/src/whole/cut.rs::killing_the_scenario_after_every_durable_event_resumes_to_the_same_projection_and_commitments` | Every durable-event cut in the full scenario window retains its prefix, projection and commitments; the full-window guards remain unchanged | `crates/togi-session/src/whole/cut/witnesses.rs::{an_interrupted_failing_trial_is_committed_once::test_case_1_same_boot_killed_process,a_between_trial_power_cut_preserves_commitments_with_matched_downtime}`; `crates/togi-session/src/resume/tests.rs::a_capsule_verifies_the_state_it_folded`. |
| `crates/togi-session/src/whole/cut.rs::crashes_at_every_durable_event_and_smu_access_account_for_trials_and_preserve_decisions` | Every durable-event and SMU-access crash in the full scenario window accounts for trials, preserves decisions and settles registers; the full-window guards remain unchanged | `crates/togi-session/src/whole/cut/witnesses.rs::{an_interrupted_failing_trial_is_committed_once::test_case_2_new_boot_crash,a_between_trial_power_cut_preserves_commitments_with_matched_downtime,a_day_smu_power_cut_resumes_with_the_nonzero_applied_profile}`. Reset kinds remain inputs, not a claimed reset-kind assertion. |
| `crates/togi-simrun/tests/sim/warm_cold.rs::warm_equals_cold::{test_case_1_flat_hazard_102,test_case_2_flat_hazard_7,test_case_3_misleading_mce_3,test_case_4_shared_voltage_1000}` | Exact journal and state bytes match across the long warm/cold matrix | `crates/togi-simrun/tests/sim/warm_cold.rs::a_bounded_crash_resume_has_identical_warm_and_cold_journal_and_state_bytes` retains an exact two-boot comparison locally. |

## Crates and toolchain

Development and debugging programs live in `crates/togi-tool-*`, with `publish = false`, never in the operator-facing `togi` crate. The package builds only `-p togi`, which depends on no tool crate. `just sim` runs `togi-tool-sim`; `togi-simrun` owns the simulated crash/reboot loop, separate from `togi-session`. Product tests build simulated journals in process through test dependencies, not an operator simulation flag.

A program with a parser, a versioned format or tests pinning refusals belongs in a Rust tool crate; Nushell orchestrates `gh`/`git` output into text, as `tools/board` does. `tools/` holds unshipped benchmark/research data, board orchestration and SQL views, not shipped runtime behavior. Rust tools share workspace types, errors, lints and in-memory unit tests; a pipeline needs no crate merely to reshape JSON. [The language decision](adr/0071-no-go-in-the-repository.md) records why Go was removed.

Rejected: a build-gated `togi debug` command keeps harness code in the operator command and adds a second build to maintain. Simulation inside the release binary tells an operator nothing about their chip while complicating real commands with simulation flags and exclusions. Simulating therefore requires a source checkout; real-machine diagnostics such as `togi doctor` stay free of simulation. Nushell for every tool loses typed parsers and precise refusal tests; Rust for every orchestration pipeline adds a build and test harness without a corresponding boundary.

A new crate needs a `Cargo.toml` with `[lints] workspace = true`. The `productCrates` closure in `flake.nix` discovers dependencies of `togi`, `togi-simrun` and `togi-watchtest`; register an unshipped crate outside that closure in `toolCrates`. Evaluation rejects missing, repeated or unknown registrations. Register fixtures read outside `crates/` in `crateFixtures` there, for the crate whose tests read them. The exact toolchain in `rust-toolchain.toml` changes only in its own pull request.

`togi`'s ordinary command integration tests compile as `commands`, with command modules such as `cli`, `read` and `reset`. Filter them with `cargo nextest run -p togi -E 'binary(commands) & test(/^read::/)'`, not the former per-command binary names. `run` remains its own binary because its panic and terminal tests re-execute exact helper names; `vm_recovery` remains the executable the VM checks install. Both remain in the default test selection.

The package and VM checks are built from production source only, so a test-only edit leaves their derivations alone. A module that compiles only under `cfg(test)` is test-only when its file is named `tests.rs` or `*_tests.rs`, or sits in a `tests` or `*_tests` directory under `src`; any other one is listed in `crates/test-only.txt`, which `nix/sources.nix` and the journal's snapshot fingerprint both read. Listing a production file there fails its build. The whole-tree documentation-link member of `quality` rejects listed paths that do not exist; the journal's nextest tests check fingerprint coverage against files present in their source.

`just lint` and the gate run Clippy with `--all-targets` for the host with default features, then all features (compiling, not running, hardware tests), and all features for the other supported target. The `clippy` flake check does the same. [CI](#ci-and-release-gates) selects every flake check and builds those missing from Cachix; inspect the flake for the current check inventory.

The [Rust guidance](../AGENTS.md#rust) leaves complexity to reviewers. The old Go cognitive-complexity limit was not ported: Clippy's `cognitive_complexity` documentation says it does not measure understandability; its default threshold mostly flagged test assertion sequences ([#874](https://github.com/shgew/togi/issues/874)).

## Platform-gated code and cross-lint

Development supports x86_64-linux and aarch64-darwin; hardware runs and NixOS VM tests require Linux. There is [no macOS CI](#ci-and-release-gates). `just lint` and the gate cross-lint with `cargo clippy --target aarch64-apple-darwin` on Linux and `--target x86_64-unknown-linux-gnu` on macOS. Keep this check. A pull request touching `cfg(target_os = ...)` or `cfg(target_vendor = ...)` says "not built on macOS" unless it was built there. File and fix macOS breakage like any other bug.

Cross-lint runs C build scripts (`alloca`, through Criterion) for the other target. The flake sets `CC_<target>` and `AR_<target>` to unwrapped Clang and `llvm-ar` in both the dev shell and the Clippy check. A C dependency needs no further setup only while it builds with these tools without SDK headers.

Linux-only code has `#[cfg(target_os = "linux")]` and a non-Linux counterpart returning an unsupported error, as in `togi-detect` and `togi-trial`. Adjacent kernel-independent parsers and fixture logic use `#[cfg(any(test, target_os = "linux"))]` so tests also run on macOS. Only real-process, inotify and other kernel-behavior tests, or tests of Linux-only code such as the trial supervisor harness, stay Linux-gated.

## Pull request stacks

Stack only under the merge-unit rule in [Workflow](../AGENTS.md#workflow). Use [`gh stack`](https://github.com/github/gh-stack), installed with `gh extension install github/gh-stack`, for every stack. Each layer has its own branch and pull request based on the layer below; pass that branch as BASE to `just claim` when creating the next layer. Lower layers hold their dependants' prerequisites. Every layer passes `just gate` independently.

Open with `gh stack submit`, keep current with `gh stack sync`, and merge with `gh stack merge` only under the owner's merge authorization; never merge layers individually by hand. A lower-layer fix restacks and re-reviews every layer above it. During review, follow the [review command's frozen-base and restacking procedure](../.omp/commands/review-pr.md), not a routine sync onto moving `main`.

## Review integration

To invoke independent review in omp, use `/review-pr [PR numbers or URLs… | all]` ([review command](../.omp/commands/review-pr.md)); no arguments selects all open non-draft pull requests. The `.omp/rules/review-after-open.md` reminder runs only in the main session, not subagents or review coordinators.

GitHub identities follow [Workflow](../AGENTS.md#workflow). To publish the review record and `review` check through `just bot`, set `ROBOTOGI_KEY_FILE` to the App PEM private-key path in the environment.
