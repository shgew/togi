# Rust migration log

The log of the move from Go to Rust ([#634](https://github.com/shgew/togi/issues/634), [ADR 0048](adr/0048-rust.md)). The rules are in [`porting.md`](porting.md). Every stage appends an entry for what its pull requests measured and found: dates, lines removed and added, compile and test times next to Go's, comparison failures and what they caught, regressions, review findings, and agent cost where known. Stage 12 ([#647](https://github.com/shgew/togi/issues/647)) turns this file into a retrospective.

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

## Stage 2: the start (2026-10-11)

The owner started stage 2 ([#637](https://github.com/shgew/togi/issues/637)) on 2026-10-11, and the run continues unattended under the rules of [#634](https://github.com/shgew/togi/issues/634) (**Running unattended**), recorded in [ADR 0057](adr/0057-running-the-rust-migration-unattended.md). This entry opens the stage's log; its pull requests append their measurements below it.

**The plan council.** Before the start, a two-reviewer plan council reviewed the epic and stages 2–12 ([#637](https://github.com/shgew/togi/issues/637)–[#647](https://github.com/shgew/togi/issues/647)). Its findings were applied to the issue bodies, each marked "(orchestrator, 2026-10-11, plan council)" ([summary](https://github.com/shgew/togi/issues/634#issuecomment-6108195762)):

- A `guard` status runs from `main` through `pull_request_target`, so a pull request cannot unregister or rewire the comparison checks that judge it.
- The cutover gains a persistent backup, a rollback procedure, a live-session gate beside the live-journal gate, evidence renewed on every push, and closing only after the release.
- Offset write targets and raw observed offsets become separate types, and the claims for typestate narrow: it proves order and authority, not that a readback or teardown happens.
- The SMU write transaction lives in the session crate (stage 4), with the `smu.intent` receipt minted by the journal crate, so stage 5 reshapes nothing the simulations link.
- Stage 5's async ownership follows the owning object, with Go's single non-cancellable PM-table reader kept as the one exception.
- Hazard 9 names Go's amd64 assembly paths (`archExp` with its FMA branch, `archLog`), and a flake check regenerates every Go-generated vector file.
- Dependabot's `nix` and `github-actions` updates pause until stage 9, a red `main` stops merges until its Linux jobs are green again (a revert of the breaking merge comes first), the integration owner is defined, and the CI-budget fallback no longer halts the run.
- The stop list applies to merged code and to defects Go shares; findings in an open pull request are fixed there.

**Decisions this stage's first pull request records** ([#691](https://github.com/shgew/togi/issues/691)). ADR 0057 partly supersedes ADR 0048 and ADR 0046. Protected comparison assets change by the additive rule, the append-only hash manifest and the `guard` status, with the owner-only `owner-approved` label for the rest. The suite is every session of `tools/bench/suite.json`, 220 today (the dev and holdout seeds of its 11 scenarios, counted from the file), where ADR 0048's text says 252. On `main`, the macOS job also builds the Rust workspace and runs its tests.

**Not measured yet.** Compile and test times, the comparison's CI time and the heavy-job cap come from stage 2's measurements pull request.

## Stage 2: the root package (2026-10-11)

The `togi-version` crate ports `version.go` ([#696](https://github.com/shgew/togi/issues/696)): 33 lines of library and 32 of tests against Go's 48 and 84, with the Go package unchanged. `version()` is the trimmed `version.txt`, embedded at compile time; `rev()` is the `TOGI_REV` the build sets at compile time, which plays the part of Go's `-X github.com/shgew/togi.rev`; `string()` joins them with `+`.

- **Departure from Go, the owner's call.** A local cargo build reports `dev`. Go reads the commit and dirty flag from the build's VCS settings, which cargo does not record, so that branch has no port; `bench` and `forecast` read the commit from git at run time instead (#637, Version stamps).
- **Not yet stamped.** The flake builds no Rust package until the binary lands (stage 7), so nothing sets `TOGI_REV` yet; that package's derivation sets it from the flake revision as `packages.default` does for Go.
- **Not measured yet.** Compile and test times wait for the stage's measurements pull request.

## Stage 2: Go-compatible float formatting (2026-10-11)

The `togi-gocompat` crate reproduces Go's float formatting byte for byte ([#697](https://github.com/shgew/togi/issues/697)), checked against vectors that Go's own output generates. It covers these Go format sites:

- `encoding/json` of a `float64` (journal events, machine and facts files, bench results, caches) maps to `json_f64`.
- Explicit `(verb, precision)` pairs map to `format_float`: `f` with -1, 0, 1, 2, 3, 4 and 6, and `g` with -1, 6, 9 and 17. The runtime uses them in `internal/config/config.go`, `internal/sim/sim.go`, `internal/journal/payloads.go`, `internal/tuner/r7.go`, `internal/watch/story.go` and `internal/trial/run.go`, and the tools in `tools/bench`, `tools/fit`, `tools/forecast`, `tools/modelcheck`, `tools/stats` and `tools/audit`. Widths and `+` flags are padding and sign handling that the tool ports add.
- `fmt` `%v` of a scalar float maps to `format_v`. The simulator's seed parts (hazard 6) would need it for float parts, although no caller passes one today. Composite `%v` (slices, maps) belongs to stage 4.
- `encoding/json` of a `float32` (the PM-table arrays in samples, `internal/machine/pm_table.go`) is not covered and lands with its first consumer.

## Stage 2: Go's exp and log in `togi-gocompat` (#698, 2026-10-11)

**What it ports.** `togi_gocompat::math` has `exp`, `expm1`, `log` and `log1p` returning the bits Go 1.27.1's `math` returns on the target the crate is built for (hazard 9 of [`porting.md`](porting.md)). On amd64 Go runs assembly for `Exp` (`archExp`, with an FMA branch taken when the CPU has AVX and FMA) and `Log` (`archLog`), and pure Go without fusion for `Expm1` and `Log1p`. On arm64 Go runs assembly for `Exp` with fused multiply-adds, and pure Go fused as the arm64 compiler fuses it for the other three. The amd64 code picks the `archExp` branch at run time with `is_x86_feature_detected!`. Fused operations go through `f64::mul_add`, which is correctly rounded by contract, so the bits do not depend on whether it becomes the instruction or libm's `fma`. No transcendental libm function is called, and the crate has no `unsafe`.

**Size.** The library is 671 lines (`math/{mod,amd64,arm64,go}.rs`), its tests 356, and the math modes of the Go vector generator `tools/gocompat-vectors` 490 lines (`math.go` and the CPU feature files, including a few lines of assembly; `main.go` came with the float formatting entry above) (counted from the files after the review round below).

**Vectors.** `tools/gocompat-vectors` runs Go's own `math` functions on a fixed input set (specials, thresholds, grids, powers of two, a seeded PRNG, and the ranges togi's Go code calls them with) and writes `function input-bits output-bits` lines. `math-amd64-fma` and `math-amd64-nofma` (the latter under `GODEBUG=cpu.fma=off`, `exp` only) run natively; `math-arm64` is cross-built and run under `qemu-aarch64` on amd64, and natively on arm64. The committed files are 75,680, 21,909 and 75,696 lines. The `gocompat-vectors-math` flake check regenerates them and requires them byte-identical to the committed files; on aarch64-darwin it regenerates and compares `math-arm64.txt` natively, and the darwin `rust-nextest` depends on it, so `main`'s macOS job runs it. The Rust tests require every vector to match bit for bit, with no tolerance, and the amd64 tests also require the two `archExp` branches to differ on the vectors that were found to tell them apart.

**What the vectors caught.** Three operand choices of the arm64 fusions, guessed from `ARM64.rules`, were wrong, and each failed on the vectors before it was corrected. Some coefficient fusions change no vector and rest on the disassembly of Go's arm64 build.

**Review findings of the first round, and their outcomes.**

- The FMA instruction was reached through `core::arch` intrinsics and `#[target_feature]` in `unsafe`, which AGENTS.md allows only in the crates for `internal/trial` and `internal/hardware`, against the decision in #637 to fuse through `f64::mul_add`. Replaced: one `fma` helper calls `f64::mul_add` on every target, and the crate has no `unsafe`, `target_feature` or intrinsic. The FMA-branch and arm64-port tests no longer need an FMA host; only `dispatch_follows_the_cpu` reads the CPU.
- Go's own tests of these functions were not mapped (#698 Acceptance). The inputs of `TestExp`, `TestExpm1`, `TestLog` and `TestLog1p` (`vf[i]`, `vf[i]/100`, `vf[i]*10`, `Abs(vf[i])`, `10`, `9` and the `vf*SC` special lists) are now in the generator, each set citing its test and lines of Go's `all_test.go`, and in `go_test_exp`, `go_test_expm1`, `go_test_log` and `go_test_log1p`. Each requires the input to be in the vector file for its function and the port to return the vector's bits, and `log(10)` and `log1p(9)` to equal `LN_10` exactly as Go's tests do, on the amd64 and arm64 ports. Regenerating added 61 lines each to `math-amd64-fma` and `math-arm64` (15 `exp`, 22 `expm1`, 12 `log`, 12 `log1p`) and 15 `exp` lines to `math-amd64-nofma`, and removed none. `TestLog1p` iterates `vflog1pSC` with `len(vflogSC)`, 7 of its 8 entries, so the eighth (4503599627370496.5) is not added. Go's `testExp` also runs `ExpGo`, the pure-Go fallback, which no path of the crate runs.
- Native arm64 regeneration was added to `main`'s macOS job, through the darwin `rust-nextest`'s dependency on `gocompat-vectors-math`.
- Setting the CPU feature with `GODEBUG` in the Rust binary was rejected: togi never sets `GODEBUG`, and the Rust binary has no `GODEBUG` contract. The Go generator sets it only to force Go's non-FMA branch.

**Not measured.** Compile and test times of the crate wait for the stage's measurements pull request. `cargo test` and `cargo test --release` of the crate each passed all nine tests on one x86_64 host; neither run was timed.

## Stage 2: `internal/trialfiles` (2026-10-11)

The `togi-trialfiles` crate ports `internal/trialfiles` ([#695](https://github.com/shgew/togi/issues/695)): 219 lines of library and 331 of tests against Go's 140 and 295, with the Go package unchanged. It removes the placeholder crate and the `--no-tests=pass` flags the empty workspace needed.

- **Departures from Go that no consumer sees.** `Open` returns a reader that reports a malformed gzip header on its first read, where Go's `Open` reports it itself; every Go caller treats both the same. Rust's standard library cannot report the error of closing the compressed file after its `fsync`, so that check is gone. Error text names the operation as Go's does, but the cause is Rust's `io::Error` text, which differs from Go's `syscall` text.
- **Not measured yet.** Compile and test times wait for the stage's measurements pull request.
