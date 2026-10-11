# Rust migration log

The log of the move from Go to Rust ([#634](https://github.com/shgew/togi/issues/634), [ADR 0048](adr/0048-rust.md)). The rules are in [`porting.md`](porting.md), and the contract is [ADR 0058](adr/0058-native-rust-rewrite.md). Every stage appends an entry for what its pull requests measured and found: dates, lines removed and added, compile and test times next to Go's, regressions, review findings, and agent cost where known. Stage 12 ([#647](https://github.com/shgew/togi/issues/647)) turns this file into a retrospective.

Append entries below the last one, newest last. State what was measured and how, and name what was not measured.

## Stage 0: the probe (2026-10-10)

**Question.** Is Rust worth porting togi for, and how much of the cost in a heavy simulated session is the language rather than the code's design?

**Setup.** Branch [`rust-probe`](https://github.com/shgew/togi/tree/rust-probe/prototype/rustprobe), commit 4b20b151, `prototype/rustprobe/`. It is throwaway and never merges. It ports the hottest allocating path of a simulated session: simulated trial conditions → `session.sampleEvidence` → `requests.Summarize`. The input is all 30,177 trials listed in `trials.tsv`, drawn from the `flat-hazard` session at seed 102 (217,192 journal events). Five variants produce byte-identical output; hyperfine ran each for 10 runs.

- `go-verbatim`: the Go code as on `main` at the time, a fresh map per sample.
- `go-tuned`: the same logic without per-sample allocation.
- `rust-verbatim-std`: a straight port of `go-verbatim` with std `HashMap`.
- `rust-verbatim-fx`: the same port with the Fx hasher.
- `rust-tuned`: mirrors `go-tuned` line for line.

**Code size.** The probe adds 609 lines in its Go implementation and 653 in its Rust implementation, containing both verbatim and tuned variants; it removes no production code. This is an experiment's size, not a production port's reduction.

**Session profile on `main`** (the profile that picked the path): background garbage collection took about 30% of user CPU, 2.24 s against 1.57 s with `GOGC=off`, and the process peaked at 405 MB.

**Results** (`single.md`; peak memory is the largest `memory_usage_byte` of the 10 runs):

| Variant | Mean wall [s] | Min [s] | Max [s] | Relative | Peak memory [MB] |
|:---|---:|---:|---:|---:|---:|
| `go-verbatim` | 4.209 ± 0.219 | 3.991 | 4.501 | 13.80 ± 0.77 | 14.8 |
| `rust-verbatim-std` | 2.495 ± 0.038 | 2.463 | 2.598 | 8.18 ± 0.21 | 5.7 |
| `rust-verbatim-fx` | 1.258 ± 0.100 | 1.184 | 1.411 | 4.12 ± 0.34 | 5.3 |
| `go-tuned` | 0.469 ± 0.014 | 0.457 | 0.503 | 1.54 ± 0.06 | 12.0 |
| `rust-tuned` | 0.305 ± 0.006 | 0.300 | 0.320 | 1.00 | 5.4 |

**Findings.**

- Design dominates. Removing the per-sample allocation took Go from 4.21 s to 0.47 s, 9×. [#629](https://github.com/shgew/togi/issues/629) applied that fix to the Go code (PR [#648](https://github.com/shgew/togi/pull/648)): on the same session, user CPU fell from 2.48 s to 1.87 s and total allocation from 2.49 GB to 1.25 GB, with every journal and `samples.jsonl` byte-identical.
- The language adds about 1.5× (`go-tuned` 0.469 s against `rust-tuned` 0.305 s) and about half the peak memory on this path.
- A straight Rust port is not fast by itself: with std `HashMap` it took 2.50 s, 8× the Rust-native version. The decision to port Rust-native rather than verbatim (ADR 0048) rests on this.
- Rust is not free of the same design flaw: swapping the hasher alone halved the verbatim port's time, which is why dense per-core arrays replace `map[int]…` in the port guide.

**Limits.** One path, one session and one build of each language. It measures wall time and peak memory of that kernel, not compile time, test time, the effect on agent-driven development, or a whole-session Rust comparison. Comparison failures, regressions, independent review findings and agent cost were not recorded for the probe. The pilot (stage 2, [#637](https://github.com/shgew/togi/issues/637)) records compile and test times against Go's.

**Decisions.** The owner decided on 2026-10-10 to migrate ([#634](https://github.com/shgew/togi/issues/634)), and that agents merge migration pull requests once CI and the `review` check pass and every thread is resolved, except the cutover and changes to the protected comparison assets. The same day, the Go structure-only issues the port replaces were closed ([#104](https://github.com/shgew/togi/issues/104), [#124](https://github.com/shgew/togi/issues/124), [#321](https://github.com/shgew/togi/issues/321), [#322](https://github.com/shgew/togi/issues/322), [#323](https://github.com/shgew/togi/issues/323), [#550](https://github.com/shgew/togi/issues/550), [#552](https://github.com/shgew/togi/issues/552), [#562](https://github.com/shgew/togi/issues/562), [#565](https://github.com/shgew/togi/issues/565) to [#568](https://github.com/shgew/togi/issues/568), [#573](https://github.com/shgew/togi/issues/573), [#575](https://github.com/shgew/togi/issues/575), [#577](https://github.com/shgew/togi/issues/577)) and [#541](https://github.com/shgew/togi/issues/541)'s type cut was superseded; their designs are in the porting guide.

## Stage 2: the start and the reversal (2026-10-11)

The owner started stage 2 ([#637](https://github.com/shgew/togi/issues/637)) on 2026-10-11 under [ADR 0057](adr/0057-running-the-rust-migration-unattended.md). Its crates then matched Go: `togi-version` ([#696](https://github.com/shgew/togi/issues/696)), `togi-trialfiles` ([#695](https://github.com/shgew/togi/issues/695)), `togi-machine` ([#703](https://github.com/shgew/togi/issues/703)) and `togi-requests` ([#704](https://github.com/shgew/togi/issues/704)) landed, and a Go-compatibility crate with its vector generator, a comparison harness and a guard were built around them.

The same day the owner reversed the contract ([#737](https://github.com/shgew/togi/issues/737), [ADR 0058](adr/0058-native-rust-rewrite.md)): the port is native, journals and state are not compatible with Go's, and a Go bug found while porting is fixed in the port. The Go-compatibility crate, the vector generator, the comparison harness and the guard were deleted, and nothing they measured is carried forward. `togi-trialfiles` and `togi-machine` were rewritten on flate2, std errors and serde defaults.

## Stage 2: native rewrite of `togi-trialfiles` and `togi-machine`, `togi-requests` adapted (#737, 2026-10-11)

- **`togi-trialfiles`** reads and writes gzip with flate2 (`MultiGzDecoder` and `GzEncoder`) and reports std `io::Error`s that name the path. It keeps what the crate is for: compression writes the `.gz` whole under a temporary name, syncs it, renames it into place and only then removes the plain file, so a reader finds one complete form at every moment; a symlinked log or instance directory is never followed; `open` prefers the plain file and falls back to the compressed one. Its ported gzip and DEFLATE readers, Go errno tables and Go `os` behaviors are gone, with the tests that pinned their texts.
- **`togi-machine`** decodes and encodes with serde derive: no case folding, duplicate-key rule, nesting limit, U+FFFD replacement or Go escaping. Host text is a `String`, vocabulary enums keep an `Unknown` variant only where the spec needs unknown values, and the seams return `Result`. `TargetOffset` and `ObservedOffset`, the std-only cancellation token and the workload table are unchanged in behavior.
- **`togi-machine` API, second pass.**
  - A seam that keeps progress across a failure returns `Result<T, Partial<T>>` (`Running::wait`, `Host::ranking`); `Kernel::read_mces` returns `Result<KernelRead, Partial<Vec<Mce>>>`. The `Waited` and `Ranking` structs and `KernelRead.error` are gone.
  - `BootId` and `KernelCursor` replace string ids; reading a whole boot is `after: None`.
  - Optional facts are `Option`: `ResetReason.kind` (`None` is no reason line, `Some(Unknown)` an unrecognized one), `TrialResult.inconclusive` and `TrialResult.failure` (signal and core together).
  - Scalars are narrower: pids and stops/conts `u32`, `CpuId(u16)`, `CcdId(u8)`, MCE bank `u16`, `boost_limit_mhz` `u32`. `BiosContext::first_difference` replaces `compare_context`; `PerCore` has `iter` and `FromIterator`, and `CoreSet` is a `u16` bitmask of cores that serializes as a sorted array, and `CancellationToken::on_cancel` runs a callback once on cancellation and returns a deregistering `OnCancel` guard.
  - `LoadSchedule` (R3 random, R4 duty) landed with `Started.schedule`; random periods come from `rand`'s `StdRng`, pinned to one `rand` version, not Go's PCG stream.
  - `Stopped` and `SmuValidated` are proofs only a seam implementation mints (clippy-gated), required by `Running::stop`, `Trials::start` and `Smu::emergency_zero`.
- **`togi-requests`** lost its Go leftovers: `f64::total_cmp`, `f64::max` and `f64::midpoint` replace the ported Go ordering, maximum and midpoint, and the NaN and signed-zero tests went with them: `summarize` skips a sample with a non-finite voltage lane, and `groups`, `top` and `counts` take finite inputs (checked with `debug_assert!`). Medians sort in place. `summarize` takes a `CoreSet`, so a core cannot repeat (Go counted a repeated core twice per sample), and the `counts` rounding tolerance is a named constant.
- **Not measured.** Compile and test times against Go, and agent cost.

## Stage 5: `togi-backend`, `togi-mprime` and `togi-ycruncher` (#764)

- **`togi-backend`** is the contract between the trial runner and the stress programs: `Backend::{kind, check, plan, classify}`, `Plan { program, args, inputs, watched }` and `Line`. An empty argv cannot be built (the program is a field of its own), and `inputs` and `watched` hold `FileName`s, validated single path components (no `/`, NUL, `.`, `..` or empty name), so the trial opens them relative to a directory descriptor and none can leave the instance directory. `check_executable` follows symlinks and wants a regular file with any execute bit; `Error::is_missing` and `From<Error> for togi_machine::Error` turn only absence (missing binary, missing or empty Binaries directory, no Zen 5 binary) into `BackendMissing`. A test-only function that spawns a process carries `#[expect(clippy::disallowed_methods)]`: the spawn inventory must reject it, so backends can never gain a spawn.
- **`togi-mprime` and `togi-ycruncher`** return the configuration text as `Plan::inputs` and write nothing. The Go adapters called `os.WriteFile`, which follows symlinks and truncates; the trial now creates the files exclusively. Thread count, SMT flag and y-cruncher's memory (32 MiB per CPU, CPUs in the given order) derive from the CPU list alone, and an empty list is an error. y-cruncher's two binaries are the lexical minimum of the matching names, chosen explicitly because `read_dir` is unordered; both are checked for every `check` and `plan`, as in Go. Classifiers compile their regexes once; their digit and whitespace classes are ASCII, as in Go, not Unicode.
- **Go bugs fixed.** The y-cruncher classifier ignored the `Atoi` error of the affinity CPU, so an oversized number became a bogus CPU; the Rust `Line::AffinityError(Option<CpuId>)` carries `None` and is still an affinity error. The Go adapters wrote through symlinks and could block on a FIFO.
- **Tests.** Every Go test of the three packages has a Rust counterpart over the fixtures read in place, plan outputs compared whole, and new tests for discovery order, unrelated and non-UTF-8 entries, bare `24-ZN5`, dangling and cyclic symlinks, a directory instead of a file, a missing Zen 5 binary and `FileName` validation. The Go `TestHardwareWorkloads` needs `togi-trial` and moves to its hardware tests.
- **Not measured.** Compile and test times against Go, and agent cost.
