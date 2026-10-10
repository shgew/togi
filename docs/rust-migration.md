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
