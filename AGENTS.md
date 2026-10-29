# togi

Rust CLI that finds and keeps testing per-CCD Curve Optimizer offsets on Zen 5 desktop CPUs, with per-core tuning as the later goal. Runs on NixOS; development also supports macOS.

## Docs

- `README.md`: status and common commands. `GLOSSARY.md`: vocabulary for code, events and docs.
- `docs/how-togi-tunes.md`: read first for nights, days, costs and ruleset history; specs own the rules.
- `docs/spec/`: normative behavior (`tuner.md`, `workloads.md`, `journal.md`, `runtime.md`). Read the relevant spec before changing behavior; update spec and code together, in one pull request or stack merged together.
- `docs/adr/README.md`: product, architecture and safety decisions and their rejected alternatives. Specs win disagreements; reversing an ADR requires a new ADR and index update. Development-process choices live in `docs/development.md` and the review docs, each with a short rationale, and change there.
- `docs/issues.md`: read before filing, triaging or claiming issues; owns labels, lifecycle, `Touches:` and the target-machine queue.
- `REVIEW.md`: read before reviewing; `docs/review-record.md`: read before recording review. `.omp/commands/review-pr.md` owns independent review mechanics; `.omp/commands/retro.md` owns run retros.
- `docs/prior-art.md`: check before proposing features. `docs/rust-migration.md`: history only; append migration measurements there.
- `docs/benchmarking.md`: run before and after tuner-decision changes; also research campaigns.
- [Development](docs/development.md): read for shell setup, choosing checks, new crates, platform-gated code or stacks. `docs/simulating.md`, `docs/reviewing.md`, `docs/releasing.md`, `CHANGELOG.md`, `changes/README.md`: read for their named activities.

## Workflow

Every change, docs included, lands through a pull request against `main` on `github.com/shgew/togi`, directly or through a stack. Only the release workflow's release commit bypasses this. The owner merges; agents merge only with explicit authorization for that pull request. Open the finished pull request without asking unless told otherwise.

- Claim first with `just claim` ([issue workflow](docs/issues.md)); use a short descriptive branch from `main` in your own worktree.
- Plan [merge units](REVIEW.md#merge-units) before coding: each pull request is independently acceptable and revertible. Follow that section for multi-crate designs, ruleset switches, hand-written-line estimates and the coordinator's reviewability decision. Commits separate behavior-preserving refactors and seams from the behavior they enable. Use short imperative messages.
- Stack only independently worthwhile merge units, even if upper layers never land; follow [stack mechanics](docs/development.md#pull-request-stacks).
- Use the owner's account through `gh`; `robotogi[bot]` only publishes reviews ([integration](docs/development.md#review-integration)).
- Follow `.github/pull_request_template.md`: Summary, Evidence (before and after), Merge Danger (door and blast radius). Say `Closes #N` when finishing, otherwise `Refs #N`. Hardware-pending Acceptance uses `Refs #N`, merges otherwise green and retains `needs-hardware` ([queue](docs/issues.md)).
- Start independent review immediately after opening, before reporting done. Merge requires CI, robotogi's `review` check on the head and resolved threads.
- Address every finding on the same branch. Fix behavior, safety, security, journal, concurrency, Acceptance, inadequate behavior tests and contradictory-doc defects. Otherwise reply with reasons without edits: taste, optional tests/refactors or another layer's work do not justify changing this layer.
- Schema/ruleset bumps (`togi_journal::SCHEMA`, `togi_tuner::RULESET`) require a `[BREAKING]` title, `breaking` label and `**BREAKING**` changelog line.
- Decision-changing bug fixes add a defect entry and pre-fix fixture-journal replay test when decisions can be matched; otherwise the changelog tells the operator to run `togi reset --all` to archive the session and start over.

## Keeping docs current

Update descriptions of the old state in the same pull request: changed commands' `--help`, README Status/Usage, specs and comments; `docs/how-togi-tunes.md` for tuner behavior and `docs/howto.md` for operator steps.

For changes an operator of the shipped `togi` binary notices, follow [the changelog fragment rules](changes/README.md).

## Writing

- **Public:** this repository and its GitHub pages are public and permanent, history and pull request refs included: files, commits, branches, issues, comments, pull requests, reviews and releases. Publish what is true of togi and the hardware it runs on, never who or where is behind it: no personal details, whereabouts or routines, no credentials, no names, addresses or paths of private machines, networks or accounts, no identifier unique to one machine, nothing from other work or private projects. Machine output stays complete as evidence; replace only what identifies a person, place or machine, the same way each time. Reread the final text before it leaves the machine; when something ruled out here seems necessary, leave it out or ask the owner.
- **Portable:** write for any reader's setup, nothing specific to one machine or tool. Where there are several ways to get somewhere, name them, then continue as if the reader got there.
- Write commits, pull requests and docs for readers who have not seen the conversation that produced them.
- Each rule has one owner, the spec. README, howto, ADRs and tool docs link to it instead of restating it.
- Never overstate: claim only what the evidence or the checks showed.
- Describe workflow steps by the action (open a pull request, set the milestone), so they hold whatever program performs them. Name the repository's own commands.

## Commands

Use the dev shell; each worktree builds in its own `target/` (Cargo's default). Follow the [check ladder](docs/development.md#shell-and-checks) before pushing and the [cross-lint setup](docs/development.md#platform-gated-code-and-cross-lint) for platform changes. A command needed twice gets a recipe in that pull request; discover recipes with `just --list`.

## Rust

Crates live under `crates/`: `togi-<name>` libraries, `togi-tool-<name>` development programs, `togi` binary. [Crate/toolchain setup](docs/development.md#crates-and-toolchain) owns registration and checks. Each crate documents its purpose; its public API is its seam. Split responsibilities that change for different reasons. Development/debugging behavior belongs in tool crates, never `togi`; `tools/` holds unshipped data, board and SQL views.

Lints live in workspace `Cargo.toml` and `clippy.toml`; CI denies warnings. Use `#[expect(lint, reason = "...")]`, never `#[allow]`. Production denies `unwrap`, `expect`, `dbg!`, `todo!`; printing belongs only in output modules. Only `togi-trial` and `togi-hardware` may locally expect `unsafe_code`, inside safe APIs with `// SAFETY:` comments; prefer rustix, refer other unsafe to the owner. Reviewers judge complexity, not a cognitive-complexity threshold.

Idiom, required in pull requests and checked by `/review-pr`:

- **Enums:** one `enum` per domain, matched exhaustively with no `_` arm over a domain enum; an `Unknown` variant only where the spec must carry an unknown value. The event type is one enum, `togi_journal::Payload`, whose variants carry the payloads and the wire name. A question several consumers ask of every variant is answered once on the type, so a new variant does not compile until it answers; a one-variant probe is `if let`.
- **States:** alternatives are variants that carry their own data. No `Option` or `bool` fields that are valid only together or only in some phase, and no sentinel (`0`, an empty set, `MAX`) standing for "absent" or "done": absence is `Option`.
- **Newtypes:** `CoreId`, `Seq`, `CcdId`, `CpuId` and the like, with private fields, checked constructors and no `Default`, never a bare integer. `TargetOffset` and `ObservedOffset` are different types ([offset types](docs/spec/runtime.md#offset-types-and-the-write-transaction)).
- **Per-core data:** a dense `PerCore<T>` iterated in index order (cores 0–15), not a map sorted at every use; sparse per-core state is `PerCore<Option<T>>`. It serializes as an object keyed by core id; a map remains only where a serialized shape requires it, at that boundary.
- **Numbers:** a value from the journal, the kernel or the config that does not fit its type is refused with an error. An internal count that saturates by design goes through its domain's named saturating helper; never a bare `try_from(…).unwrap_or(MAX)`.
- **Config:** `RawConfig` is parsed once into the validated `Config`; the rest of the code takes only `Config`.
- **Errors:** an error enum names the operation and keeps the cause (`thiserror`); where a failure must keep the progress made, `Result<T, Partial<T>>`. The message names the operation only and never repeats the cause, which is the `#[source]`; one renderer prints the chain. `Result<_, String>` only where clap asks for it (value parsers). An error discarded on purpose is handled explicitly and the pull request says why, never `unwrap()` or `expect()`.
- **Serde:** derive with `serde_json` defaults; no custom text rules.
- **Randomness:** `StdRng` from `rand`, pinned to an exact version. A recorded seed reproduces its stream only with the build that recorded it; a `rand` bump changes the stream.
- **Cancellation:** a `CancellationToken` passed explicitly to code that waits. The tuner and journal take none; the session loop and the simulator never wait themselves but pass a token to the machine seams that wait (`Clock::sleep`, `Trials`, `Running::wait`): the run's token, or a fresh one where the spec requires uncancellable cleanup (same-boot reconciliation, the stale-scope sweep). Tokio stays in the crates that wait on the outside world.
- **Ownership:** borrow for reads; `Clone` explicitly where a copy is meant. A capability that depends on another borrows it for its whole life, not only at construction (hardware access borrows the host lock). A guard, ticket or outcome whose drop releases a lock or loses a result is `#[must_use = "…"]`.
- **Text at the edge:** data stays typed until it is rendered. Code never parses text it formatted, and a test's name or label never selects its input.
- **One owner per rule:** a rule the tuner owns is queried from the tuner, never re-implemented in the dashboard or a tool.
- **Safety semantics:** the specs own them, with fault tests: [offset types, the intent-write-readback receipt chain and what typestate proves](docs/spec/runtime.md#offset-types-and-the-write-transaction), [journal-failure emergency zeroing](docs/spec/runtime.md#exit-codes), [explicit teardown order](docs/spec/workloads.md#containment) and [journal durability](docs/spec/journal.md#rules).

## Code

- **Traceable:** journal every action/decision with a stranger-readable `msg`; the journal alone reconstructs what happened and why.
- **Safe writes:** only `togi-smu` writes hardware, through the session transaction enforced by Clippy's disallowed methods: `TargetOffset` in [-50, 0], durable intent before, readback after. The constrained zero-only emergency is separate ([transaction](docs/spec/runtime.md#offset-types-and-the-write-transaction)).
- Comments explain reasons names cannot; describe current Rust, leaving deleted Go ancestry to ADRs, changelog and migration history.

## Testing

- **In memory:** [Development, Tests and simulations in memory](docs/development.md#tests-and-simulations-in-memory) owns the in-memory seams, permitted disk categories and I/O interfaces.
- **Simulator first:** behavior is proven on `togi-sim` with fixed seeds, never by waiting for hardware.
- Tests pin spec behavior: rules, boundaries, invariants, crash-resume. Parametric cases are `#[test_case]` attributes or separate named tests, never a hand-written case list looped inside one test; a loop stays only for a generated or exhaustive sweep, and its assert names the failing input. Seeded `proptest` with failure persistence disabled for invariants, compile-time snapshot fixtures for rendered output, a fuzz target for the journal reader (`just fuzz`), whose properties and seeds live in `togi-journal`'s `tests/reader_fuzz`, where every test run replays the seeds. Compare values with `assert_eq!`.
- Tests reach the tuner and the session through their public API, never through private fields or test-only mutators.
- Ordinary integration tests share one binary per crate. `togi` groups command tests in `commands`; `run` stays separate for its re-executed panic and terminal helpers, and `vm_recovery` is installed and invoked by the VM checks. Keep module prefixes when filtering tests.
- Concurrent code is tested on real threads.
- Tests that need the real world are named: `real_fs_<name>` checks kernel filesystem behavior (locks, modes, symlinks, publication, inotify), never content parsing; `real_process_<name>` starts real processes (a helper program the test builds, never mprime or y-cruncher). Product tests run in `just test`, `just gate` and CI's `nextest`; tool-crate tests run in the CI-tier `nextest-tools` check ([nextest by concern](docs/development.md#nextest-by-concern)), locally via `just focus <crate>` or `just check-one nextest-tools`. Both run at nextest's default concurrency, with files confined to each test's temporary directory and sync disabled. Only shared-hardware checks are serialized ([test execution](docs/development.md#rust-test-execution)). `just hardware` runs tests behind the `hardware` Cargo feature on the target machine, restoring every offset they change; VM tests exercise the shipped binary's real durability.
- **Coverage:** apply [coverage as review evidence](REVIEW.md#coverage), never a target. `just cover BASE` shows changed lines no test reaches and items not built for the platform; reviewers judge whether consumer-visible behavior lacks proof.
- **Test cost:** follow [test cost and survivors](REVIEW.md#test-cost-and-survivors) when adding, speeding up, deleting, replacing, shortening or narrowing tests. Preserve safety assertions, avoid duplicated behavior and map every assertion no longer exercised to its surviving proof or the decision retiring it.
