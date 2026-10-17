# Tests and simulations run in memory

Status: see [the index](README.md).

Decides where tests and simulations keep their data and which I/O gets an interface ([#611](https://github.com/shgew/togi/issues/611)). It keeps the bench-child exception of [ADR 0060](0060-bench-children-keep-temporary-journals.md) and the test concurrency of [ADR 0063](0063-only-shared-hardware-tests-are-serialized.md).

## Context

How fast the check loop runs decides how fast agents iterate on togi. Tests and simulations that write to disk are slower and leave files behind. The Go simulator's in-memory journal mode still appended every event to `events.jsonl`, and the Go `just same` kept tens of gigabytes of run directories ([#603](https://github.com/shgew/togi/issues/603)). The Rust workspace already runs most tests on in-memory seams: `togi-journal`'s `Fs` with `MemFs`, and `togi-machine`'s `Clock`, `Smu`, `Host`, `Trials` and `Kernel`, which `togi-sim` and the tests implement in memory. The rule was written only in `AGENTS.md`, without its reasons or its exceptions.

## Decision

**Tests and simulations never touch disk** unless one of these holds:

- The test checks kernel-dependent filesystem or process behavior: a `real_fs_` or `real_process_` test (`AGENTS.md`, Testing), which confines its files to its own temporary directory and disables sync; a test behind the `hardware` Cargo feature, which `just hardware` runs on the target machine; or a VM test, which exercises the shipped binary's real durability.
- The caller asks for files: a flag such as `--state-dir` or `--keep`, a single `togi-tool-sim` session, whose announced state directory is its output ([simulating](../simulating.md)), or a failing run keeping its evidence for diagnosis.
- [ADR 0060](0060-bench-children-keep-temporary-journals.md) applies: bench, `same`, `smoke` and `audit` children run with a temporary `--state-dir`, and the parent reads and parses each journal after the child exits. `forecast` children run the same way, each resuming its own copy of the state directory it forecasts ([benchmarking](../benchmarking.md#the-forecast-command)).

Ordinary tests also do no network I/O, start no processes, leave no process-wide state behind and never call `fsync`, directly or through production code; they inject sync failures or record durability ordering instead. Production keeps its per-event durability: the in-memory path is a seam the tests choose, not a setting that weakens the shipped binary.

**I/O sits behind a narrow interface only at a real boundary:** the filesystem, the clock, processes, hardware and the SMU. Each such interface has an in-memory fake. A type with no boundary gets no interface; a test reaches it through its real implementation.

**Every change also looks for test and simulation speed wins** in the code it touches.

**Reviewers flag a test that writes to disk without needing to** (`REVIEW.md`, Tests).

Filesystem and process tests keep nextest's default concurrency, as [ADR 0063](0063-only-shared-hardware-tests-are-serialized.md) decides; only shared-hardware tests are serialized.

## Considered Options

- **Temporary directories for every test:** rejected. Each test pays for file creation, writes and cleanup, a failed cleanup leaves files behind, and a test that syncs pays for the disk too. Only kernel behavior needs the kernel.
- **An interface for every type that a test wants to replace:** rejected. An interface without a boundary adds indirection and a fake that can drift from the type it stands in for, and the test then checks the fake.
- **Leave the rule in `AGENTS.md` alone:** rejected. Without its reasons, its exceptions read as loopholes, and ADR 0060 had to narrow a rule that no ADR recorded.

## Consequences

- Ordinary runs stay fast and leave nothing behind; a run that needs files says so through a test's name or a caller's flag.
- A new boundary arrives with its in-memory fake, and a test of logic above it needs no disk, process or real clock.
- `AGENTS.md` and `REVIEW.md` link here instead of restating the rule.
- A later exception, like ADR 0060's, needs its own decision with measurements.
