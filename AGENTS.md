# togi

Rust CLI that finds per-core Curve Optimizer offsets on Zen 5 desktop CPUs and keeps testing them. Runs on NixOS; development also works on macOS.

## Docs

- `README.md`: what togi does, what works today, and the common commands.
- `GLOSSARY.md`: the vocabulary. Name code, events and docs with its terms.
- `docs/how-togi-tunes.md`: read first to understand the tuning sequence, phase costs and ruleset history; the specs own the rules.
- `REVIEW.md`: defect criteria and recurring lessons. Read it before reviewing a pull request.
- `docs/review-record.md`: the review record format. The review coordinator reads it before writing a record.
- `.omp/`: reviewer rules, the `review-coordinator` agent, `/review-pr`, the omp command for reviewing pull requests in parallel and recording their gates, and `/retro`, which writes the retro of a target-machine run and files its actions.
- `docs/spec/`: normative behavior (`tuner.md`, `workloads.md`, `journal.md`, `runtime.md`). Read the relevant spec before changing behavior, and change spec and code in the same pull request or in layers of one stack merged together.
- `docs/issues.md`: labels, lifecycle, `Touches:`, claiming and the target-machine queue. Read it before filing, triaging or claiming an issue.
- `docs/adr/`: decisions and the alternatives rejected, with statuses in [the index](docs/adr/README.md). Where a spec disagrees with an ADR, the spec wins. Reversing a decision needs a new ADR, which updates the index in the same pull request.
- `docs/prior-art.md`: before proposing a feature, check whether it was deliberately left out.
- `docs/rust-migration.md`: the Go-to-Rust migration log, history only; append what a pull request of the migration measured ([ADR 0048](docs/adr/0048-rust.md), [ADR 0058](docs/adr/0058-native-rust-rewrite.md)). The porting rules that last are in [Rust](#rust), [Code](#code) and the specs.
- `docs/benchmarking.md`: run it before and after any change to how the tuner decides; it also covers `just forecast` and research campaigns (`just research`).
- `docs/simulating.md`, `docs/reviewing.md`, `docs/releasing.md`, `CHANGELOG.md` and `changes/README.md`: read when you do what they name.

## Workflow

Every change, docs included, lands as a pull request against `main` on `github.com/shgew/togi`, or as a layer of a stack of pull requests that ends on `main`. The owner controls what reaches `main`: the owner merges, and an agent merges only a pull request the owner has explicitly told it to merge. When a change is done, open its pull request without asking, unless told otherwise. The one commit that reaches `main` without a pull request is the release commit the release workflow pushes (`docs/releasing.md`).

- Claim the issue first (`docs/issues.md`): `just claim` assigns it and posts the start comment; then create the branch from `main`, with a short descriptive name, in your own worktree. In a stack, each layer above the bottom branches from the layer below: pass that layer's branch as BASE.
- Agents act on GitHub with the owner's account through `gh`: open pull requests, push, comment, reply, resolve threads, and merge when the owner explicitly permits it. `robotogi[bot]` only posts reviews: the review record and the `review` check, through `just bot`. Set `ROBOTOGI_KEY_FILE` to the App's PEM private key file path in the environment.
- Plan the pull requests while designing the implementation, before writing code. A pull request is one merge unit: a change the owner would accept or revert whole, usually one issue or one entry in a design's Pull requests list. Review scales to a large diff by splitting it among reviewers, so size alone is no reason to split a merge unit. Commits carry the structure inside it: a behavior-preserving refactor in its own commit apart from the change it enables, a new seam apart from the behavior built on it.
- Stack only when a lower layer is a merge unit of its own, worth merging even if the layers above never land; every fix to a lower layer restacks and re-reviews the layers above it. Stacked pull requests always use [`gh stack`](https://github.com/github/gh-stack) (`gh extension install github/gh-stack`): each layer is a branch with its own pull request based on the layer below, a lower layer holds what the ones above depend on, and every layer passes `just gate` on its own. Open the stack with `gh stack submit`, keep it current with `gh stack sync`, and merge it with `gh stack merge`, never layer by layer by hand.
- Commit with short imperative messages.
- The pull request body follows `.github/pull_request_template.md`: a short summary, and the demo in a collapsed block.
- Opening a pull request starts its independent agent review before its author reports done. `.omp/commands/review-pr.md` says how the review runs (the omp command is `/review-pr [PR numbers or URLs… | all]`; no argument means all open non-draft pull requests). omp's `.omp/rules/review-after-open.md` sends the main session a reminder after each PR-opening command it runs; subagents, review coordinators included, get none. The ruleset enforces the merge gate: robotogi's `review` check on the head, resolved review threads and CI.
- Address every finding on the same branch. Fix real defects: behavior, safety, security, journal integrity, concurrency, unmet Acceptance, tests that fail to pin behavior, and docs that contradict behavior or other docs. Reply with a reason to the rest and change nothing; style, wording, optional tests, naming taste, optional refactors and findings another layer handles are not reasons to change this layer.
- A pull request that finishes an issue says `Closes #N` in its body; one that only makes progress says `Refs #N`.
- A pull request whose Acceptance needs a run on the target machine says `Refs #N` and merges once everything else is green; its issue keeps `needs-hardware` (`docs/issues.md`, ADR 0043).
- A pull request that bumps `togi_journal::SCHEMA` or `togi_tuner::RULESET` is breaking: its title starts with `[BREAKING]`, it carries the `breaking` label, and its changelog line starts with `**BREAKING**`.
- A pull request that fixes a bug that changed decisions adds a defect entry, with a test replaying a fixture journal from before the fix, when the affected decisions can be matched. Otherwise its changelog line tells the operator which `togi reset --core` to run.

## Keeping docs current

A pull request updates everything that describes the old state, in the same pull request:
- `--help` text for every command or flag it adds or changes;
- the README's Status when what works changes, and its Usage when the common commands change;
- a changelog fragment, `changes/<N>.md` named by the pull request's number and added once it is opened, for every change a user of togi would notice: commands, flags, behavior, output, configuration. One line per change under `### Added`, `### Changed`, `### Removed` or `### Fixed`, stating the effect and ending with a period; the release adds the pull request link (ADR 0033). A `**BREAKING**` line starts with what the operator must do or will see, then the mechanism. A change to something not yet released edits that change's fragment instead. Refactors, tests and doc edits that leave the tool unchanged get no fragment;
- the specs, and any comment the change makes wrong.
- `docs/how-togi-tunes.md` when tuner behavior changes.

Pull requests that change the operator's steps update `docs/howto.md`.

## Writing

- **Public:** this repository and its GitHub pages are public and permanent, history and pull request refs included: files, commits, branches, issues, comments, pull requests, reviews and releases. Publish what is true of togi and the hardware it runs on, never who or where is behind it: no personal details, whereabouts or routines, no credentials, no names, addresses or paths of private machines, networks or accounts, no identifier unique to one machine, nothing from other work or private projects. Machine output stays complete as evidence; replace only what identifies a person, place or machine, the same way each time. Reread the final text before it leaves the machine; when something ruled out here seems necessary, leave it out or ask the owner.
- **Portable:** write for any reader's setup, nothing specific to one machine or tool. Where there are several ways to get somewhere, name them, then continue as if the reader got there.
- Write commits, pull requests and docs for readers who have not seen the conversation that produced them.
- Each rule has one owner, the spec. README, howto, ADRs and tool docs link to it instead of restating it.
- Never overstate: claim only what the demo or the checks showed.
- Describe workflow steps by the action (open a pull request, set the milestone), so they hold whatever program performs them. Name the repository's own commands.

## Commands

Enter the dev shell with `nix develop`, or with `direnv allow` once per checkout if you use direnv. Recipes that run `cargo`, the retained Go tools, Nushell or `gh` stop with that hint outside it; `just hardware` enters it itself, since `sudo` drops the environment. Go is only for the two retained tools under `tools/`, `reviews` and `cover`, until their Rust rewrites land ([ADR 0071](docs/adr/0071-no-go-in-the-repository.md)): the shell sets `GOTOOLCHAIN=local`, so `go` uses the flake's pinned toolchain and fails instead of downloading another one when `go.mod` asks for a newer Go. `just --list` lists the recipes by group with their purpose; a command needed twice gets a recipe, in the same pull request.

The check ladder: `just test` or `just focus <crate>` while editing; `just gate` before every push, every non-VM flake check but the `sim-suite-*` simulator suite shards, which CI and `just check` on Linux run (`just sim-suite` or `just check-one sim-suite-<i>` reproduces them), and cargo-deny's advisory audit, which only the `deny` flake check runs, sequentially with a warm cargo cache, except a fix pushed during a review, which `just push-fix` pushes instead; `just check`, every flake check the host builds, only for changes to `flake.nix` or `nix/`. Agents sharing one host run `just gate`, `just check` and benchmarks under `flock` on one lock file shared by the agents on that host, so parallel sessions queue instead of overloading the machine. `just ship` formats, runs `just gate` under a lock that every worktree of the clone shares, releases the lock and pushes the current branch, force-pushing with lease after a rebase and refusing on a `gh stack` layer; run `just check` before it when the ladder asks for one. `just push-fix` refuses as `just ship` does, then formats, runs `just lint`, `go test` on the retained Go tool packages and `just test` on the Rust crates changed since the last pushed head and pushes, without the gate and the lock; a pull request's first push is `just ship`. `just land N` waits for `check` and `review` on pull request N's head and for every review thread to be resolved, then reports it ready for the owner; neither merges. CI is the definition of green (ADR 0036); its jobs are explained in `.github/workflows/check.yml`, the VM tests in `nix/vm-test.nix` and `nix/module-test.nix`. `just hardware` runs on the target machine only ([provisioning](docs/howto.md#host-lock-and-delegated-hardware-tests)).

`just board-test` runs the Nushell board's fixed-fixture assertions without GitHub access. It runs in `just gate`, the `board-test` flake check, and `just push-fix` when anything under `tools/board/` changed.

Development works on x86_64-linux and macOS (aarch64-darwin); hardware runs and the NixOS VM tests need Linux. No CI job runs on macOS ([ADR 0061](docs/adr/0061-no-macos-ci.md)), so stay mindful of it: the cross-platform check is `cargo clippy --target aarch64-apple-darwin` from Linux, in `just lint` and `just gate`, and it stays; code behind `cfg(target_os = "linux")` keeps a working non-Linux path; and a pull request that changes platform-gated code (`cfg(target_os = ...)`, `cfg(target_vendor = ...)`) says that it was not built on macOS unless the author built it there. A breakage found on macOS is filed and fixed like any other bug.

Linux-only code sits behind `#[cfg(target_os = "linux")]` with a `#[cfg(not(target_os = "linux"))]` counterpart that returns an unsupported error, as `togi-detect` and `togi-trial` do.

## Rust

The Cargo workspace has one directory per crate under `crates/`: libraries named `togi-<name>`, development programs named `togi-tool-<name>`, and the binary `togi`. A new crate is only a directory with a `Cargo.toml` that sets `[lints] workspace = true`; the toolchain is the exact version in `rust-toolchain.toml`, bumped only in its own pull request. `just focus <crate>` and `just typecheck <crate>` are the inner loop, `just test` runs the workspace, and `just gate` and `just lint` run the retained Go tools' checks and the Rust ones, including `cargo clippy --target aarch64-apple-darwin` from Linux. That cross lint runs C build scripts (`alloca`, from criterion) for macOS, so the flake sets `CC_aarch64_apple_darwin` and `AR_aarch64_apple_darwin` to the unwrapped clang and `llvm-ar` in the dev shell and the `clippy` check on Linux; a dependency that compiles C needs nothing more only while it builds with those and no Apple SDK header. The `clippy`, `nextest`, `doc`, `cargo-fmt`, `deny`, `shear` and `package` flake checks run in CI like every other check. A test that starts real processes is named `real_process_<name>`, which nextest gives a longer timeout.

Lints live in the workspace `Cargo.toml` and `clippy.toml`; CI denies warnings. Write `#[expect(lint, reason = "...")]`, never `#[allow]`. `unwrap`, `expect`, `dbg!` and `todo!` are denied outside tests, and printing is allowed only in output modules. `unsafe_code` is denied: only `togi-trial` and `togi-hardware` may add a local `#[expect(unsafe_code, reason = "...")]` inside a safe API, with a `// SAFETY:` comment; reach for rustix first, and park any other unsafe for the owner.

Idiom, which a pull request follows and `/review-pr` checks it against:

- **Enums:** one `enum` per domain, matched exhaustively with no `_` arm over a domain enum; an `Unknown` variant only where the spec must carry an unknown value. The event type is one enum, `togi_journal::Payload`, whose variants carry the payloads and the wire name. A question several consumers ask of every variant is answered once on the type, so a new variant does not compile until it answers; a one-variant probe is `if let`.
- **States:** alternatives are variants that carry their own data. No `Option` or `bool` fields that are valid only together or only in some phase, and no sentinel (`0`, an empty set, `MAX`) standing for "absent" or "done": absence is `Option`.
- **Newtypes:** `CoreId`, `Seq`, `CcdId`, `CpuId` and the like, with private fields, checked constructors and no `Default`, never a bare integer. `TargetOffset` and `ObservedOffset` are different types ([offset types](docs/spec/runtime.md#offset-types-and-the-write-transaction)).
- **Per-core data:** a dense `PerCore<T>` iterated in index order (cores 0–15), not a map sorted at every use; sparse per-core state is `PerCore<Option<T>>`. It serializes as an object keyed by core id; a map remains only where a serialized shape requires it, at that boundary.
- **Numbers:** a value from the journal, the kernel or the config that does not fit its type is refused with an error. An internal count that saturates by design goes through its domain's named saturating helper; never a bare `try_from(…).unwrap_or(MAX)`.
- **Config:** `RawConfig` is parsed once into the validated `Config`; the rest of the code takes only `Config`.
- **Errors:** an error enum names the operation and keeps the cause (`thiserror`); where a failure must keep the progress made, `Result<T, Partial<T>>`. The message names the operation only and never repeats the cause, which is the `#[source]`; one renderer prints the chain. `Result<_, String>` only where clap asks for it (value parsers). An error discarded on purpose is handled explicitly and the pull request says why, never `unwrap()` or `expect()`.
- **Serde:** derive with `serde_json` defaults; no custom text rules.
- **Randomness:** `StdRng` from `rand`, pinned to an exact version. A recorded seed reproduces its stream only with the build that recorded it; a `rand` bump changes the stream.
- **Cancellation:** a `CancellationToken` passed explicitly to code that waits. The tuner and journal take none; the session loop and the simulator never wait themselves but pass a token to the machine seams that wait (`Clock::sleep`, `Trials`, `Running::wait`): the run's token, or a fresh one where the spec requires uncancellable cleanup (same-boot reconciliation, the stale-scope sweep). tokio stays in the crates that wait on the outside world.
- **Ownership:** borrow for reads; `Clone` explicitly where a copy is meant (`forecast` copies the folded state). A capability that depends on another borrows it for its whole life, not only at construction (hardware access borrows the host lock). A guard, ticket or outcome whose drop releases a lock or loses a result is `#[must_use = "…"]`.
- **Text at the edge:** data stays typed until it is rendered. Code never parses text it formatted, and a test's name or label never selects its input.
- **One owner per rule:** a rule the tuner owns is queried from the tuner, never re-implemented in the dashboard or a tool.
- **Safety semantics:** the specs own them, with fault tests: [offset types, the intent-write-readback receipt chain and what typestate proves](docs/spec/runtime.md#offset-types-and-the-write-transaction), [journal-failure emergency zeroing](docs/spec/runtime.md#exit-codes), [explicit teardown order](docs/spec/workloads.md#containment) and [journal durability](docs/spec/journal.md#rules).

## Layout

Each crate's purpose is its crate doc comment (`cargo doc -p togi-<name>`). Two invariants hold across crates:

- Normal offset writes go only through the session's write transaction (`togi-session`) over the `togi-smu` driver, which clippy's `disallowed-methods` enforces; the constrained zero-only emergency is the one separate path ([Safe writes](#code)).
- `tools/` holds data, the Nushell board and the two retained Go tools and is never shipped; other development programs are the `togi-tool-*` crates, and development and debugging behavior lives there, never in the `togi` crate.

A crate owns one responsibility, and its public API is the seam. Split a crate when it holds two responsibilities that change for different reasons.

## Code

- **Traceable:** every action and decision is a journal event with a `msg` a stranger can follow (`journal.md`). Verbose is the goal: someone reading only the journal can reconstruct what togi did and why.
- **Safe writes:** offsets reach hardware only as [runtime.md](docs/spec/runtime.md#offset-types-and-the-write-transaction) specifies: a `TargetOffset` in [-50, 0] with an intent event before and a readback after for normal writes, and the separate constrained zero-only emergency.
- Comments only where names cannot carry the reason. They describe the Rust code; the deleted Go implementation appears only in history (ADRs, the changelog, the migration log).
- Error variants name the operation that failed and keep the cause.

## Testing

- **Fast and deterministic:** ordinary tests exercise logic in memory, with readers, writers, `MemFs` and fakes. They do no filesystem or network I/O, start no processes and leave no process-wide state behind; time comes from the injected `Clock` seam or tokio's paused clock. No test calls `fsync`, directly or through production code: inject sync failures or record durability ordering instead. Kernel-dependent checks use the named categories below.
- **Simulator first:** behavior is proven on `togi-sim` with fixed seeds, never by waiting for hardware.
- Tests pin spec behavior: rules, boundaries, invariants, crash-resume. Parametric cases are `#[test_case]` attributes or separate named tests, never a hand-written case list looped inside one test; a loop stays only for a generated or exhaustive sweep, and its assert names the failing input. Seeded `proptest` with failure persistence disabled for invariants, compile-time snapshot fixtures for rendered output, a fuzz target for the journal reader (`just fuzz`). Compare values with `assert_eq!`.
- Tests reach the tuner and the session through their public API, never through private fields or test-only mutators.
- Concurrent code is tested on real threads.
- Tests that need the real world are named: `real_fs_<name>` checks kernel filesystem behavior (locks, modes, symlinks, publication, inotify), never content parsing; `real_process_<name>` starts real processes (a helper program the test builds, never mprime or y-cruncher). Both run in `just test`, `just gate` and CI at nextest's default concurrency, with files confined to each test's temporary directory and sync disabled. Only shared-hardware checks are serialized ([ADR 0063](docs/adr/0063-only-shared-hardware-tests-are-serialized.md)). `just hardware` runs tests behind the `hardware` Cargo feature on the target machine, restoring every offset they change; VM tests exercise the shipped binary's real durability.
- **Coverage is evidence, not a target** (ADRs 0030, 0032): no threshold, ratchet or tracked percentage. `just cover BASE` shows reviewers changed lines no test reaches. A test exists to pin behavior, never only to reach a line.
