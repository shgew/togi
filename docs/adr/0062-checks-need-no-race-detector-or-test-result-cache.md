# Checks need no race detector or test-result cache

Status: see [the index](README.md).

Supersedes [ADR 0022](0022-run-the-race-detector-in-ci.md) and [ADR 0045](0045-the-tight-loop-reuses-cached-test-results.md), and only those two. The Go tree they describe is gone ([#634](https://github.com/shgew/togi/issues/634), [#918](https://github.com/shgew/togi/issues/918)), and with it the tools they decided about: `go test -race`, Go's cacheable test results and `-shuffle`.

## Context

ADR 0022 ran Go's race detector as a flake check on every pull request and in `just test -race`, because a Go data race compiles and the detector finds it at runtime. ADR 0045 kept `go test` results cacheable in the tight loop by leaving `-shuffle` out, and shuffled at `just gate` and in CI instead. The Rust workspace has neither mechanism, so the question is whether it needs a substitute for either: a race check, and a way to avoid rerunning unchanged tests.

## Decision

- **No race check.** Safe Rust rejects data races at compile time: a value crosses threads only if its type is `Send`, and is shared only if it is `Sync`. The workspace sets `unsafe_code = "deny"` and clippy's `undocumented_unsafe_blocks = "deny"`, and every crate inherits them. The policy (`AGENTS.md`) lets only the `togi-trial` and `togi-hardware` crates add a local `#[expect(unsafe_code, reason = ...)]` inside a safe API, each with a `// SAFETY:` comment, and sends any other unsafe to the owner. When this ADR was written the only such scopes were in `togi-trial`: the `pre_exec` closure that runs between `fork` and `exec`, and a test helper that installs a signal disposition; the policy, not that list, is the rule. This is not a proof that the program has no data race. The scopes the policy allows and the unsafe code inside dependencies sit outside the compiler's guarantee, and `Send`/`Sync` rejects data races, not race conditions: ordering, deadlock, and time-of-check/time-of-use on files and processes. Go's detector did not report those either.
- **Concurrent code is tested on real threads and processes.** This is the policy in `AGENTS.md`. Tests in `togi-hostlock`, `togi-machine`, `togi-smu`, `togi-trial`, `togi-watch` and `togi` spawn real threads or processes, and nextest's `real-process` and `hardware` test groups limit the ones that share machine resources. A change to concurrent code adds such a test; no tool stands in for it. Deterministic seams and simulation add evidence about logic races and do not replace it.
- **No Miri and no ThreadSanitizer.** Both need a nightly toolchain, while `rust-toolchain.toml` pins stable and the Nix checks build from that file. They would add a second toolchain to pin, update and cache. Miri also runs only a subset of the process, signal and terminal calls these tests make.
- **No test-result cache and no shuffle for Rust tests.** `just test` runs `cargo nextest run` and `just focus <crate>` runs `cargo nextest run -p <crate>`. Nextest keeps no result cache, so every invocation runs every selected test, and the nextest in the dev shell (0.9.146) has no shuffle option. Nextest runs each test as its own process, so one test cannot read another's in-memory state and the order within a test binary cannot leak any. Reuse comes from cargo: its incremental build in the dev shell, and, in the flake, the crane dependency artifacts that `clippy`, `nextest`, `doc` and `trial-scope-tests` share.
- **The inner loop is `just typecheck <crate>` and `just focus <crate>`.** `just typecheck` runs `cargo check --all-targets -p <crate>` and runs no test. `just test` runs the whole workspace. `just gate` runs `cargo nextest run --locked --profile ci` and `cargo test --locked --doc`.
- **Process separation does not remove every coupling.** Tests still share paths, the wall clock, ports and host resources, which is why `.config/nextest.toml` caps `real-process` and `hardware` tests, and `just hardware` and `cargo test --doc` run under libtest, where a binary's tests share a process (`just hardware` sets `RUST_TEST_THREADS=1`).
- **The retained Go tools are unchanged.** `just gate` and the `go-tools` check still run their tests with `-shuffle=on`; `just push-fix` runs `go test` on touched Go packages without it.

## Considered Options

- **Miri on the concurrent crates:** rejected. Nightly-only against a stable pin, and it cannot run most of the real process, signal and terminal behavior those crates exist to handle.
- **ThreadSanitizer (`-Zsanitizer=thread`):** rejected. It needs nightly and a rebuilt standard library, and it reports data races, which safe code cannot contain.
- **A shuffle for Rust tests:** rejected. Nextest has none, and libtest's `--shuffle` is unstable, which means nightly, and would apply only inside one test binary's single process, where the isolation nextest gives is missing.
- **A result cache around nextest:** rejected. Nextest offers no hook for one, and a hand-built cache keyed on binaries and fixtures would be code to keep correct, for a saving no measurement here supports.
- **Repeating runs by hand (`cargo nextest run --stress-count N`):** available to anyone chasing a flake, and in no recipe or check.

## Consequences

- The compiler does not rule out logic races in safe code. Real-thread tests and review have to find them, with deterministic seams and simulation as further evidence. A new `unsafe` scope within the policy is backed by its `// SAFETY:` comment, which the lint requires to exist and a reviewer to judge; no tool checks that it is right.
- `just test` reruns every test on every invocation. How long that takes, and how long `just focus` takes, is a measurement, not a decision of this ADR.
- A test that depends on another test's file effects, or on the clock, is not made to fail by an order change. It fails intermittently or not at all, and the test groups, not shuffling, contain the shared-resource cases.
- ADR 0021 and ADR 0042 speak of fresh `-shuffle=on` seeds. Rust checks draw none; a substituted check skips only the run.
