# Porting togi from Go to Rust

[ADR 0048](adr/0048-rust.md) decides the migration, [ADR 0058](adr/0058-native-rust-rewrite.md) sets its contract and [#634](https://github.com/shgew/togi/issues/634) stages it. This guide is what a porting pull request follows and what `/review-pr` checks it against. [`rust-migration.md`](rust-migration.md) is the log.

The contract: **togi becomes idiomatic Rust that does what [`docs/spec/`](spec/) and the README describe.** Go is a reference for intent, not an oracle. Journal format, schema, `state.json`, config, CLI and rendered output may change; change the spec in the same pull request when they do. The Rust build reads no Go-written journal, state or archive. A Go bug found while porting is fixed in the port and named in the pull request. What may not change is the safety below.

## Go idiom, Rust idiom

| Go | Rust | Why |
|---|---|---|
| String or int kinds (`journal.Kind`, outcomes, phases, hunt results) compared as literals | One `enum` per domain, matched exhaustively with no `_` arm over a domain enum; an `Unknown` variant only where the spec needs to carry an unknown value | A new variant fails to compile at every site that must handle it |
| Payload structs plus a registry (`payloadTypes`) | One event enum whose variants carry the payloads; the wire name lives on the variant | The registry and the type switches cannot disagree |
| `int` for core IDs, offsets, counts, sequence numbers | Newtypes (`CoreId`, `Count`, `Seq`) with checked constructors, and two offset types (below) | A core ID cannot be passed as an offset |
| `map[int]T` plus sorting at every use | A dense per-core array (`PerCore<T>`), iterated in index order | No hashing, no sort, no allocation per sample; supported core IDs are 0–15 |
| Comments and review saying "record intent, write, read back" | Typestate for normal writes: `Intent` → `Written` → `Verified`, each step consuming the last; only the smu crate can construct them, and only the journal crate mints the receipt of a durable `smu.intent` | A normal write that skips its durable `smu.intent` does not compile; the constrained emergency-zeroing path below remains separate |
| `defer` teardown of trials | A typestate for the trial lifecycle (`Running` → `Reaped` → `ReadersDone` → `Ended`) with an explicit async `teardown` that returns its errors; `Drop` only as a backstop that logs | `Drop` cannot await, cannot return an error and must not decide whether teardown was verified |
| `RawConfig` decoded, then checked in scattered places | `RawConfig` is parsed once into a validated `Config`; the rest of the code takes only `Config` | Invalid states are unrepresentable past the boundary |
| `fmt.Errorf("…: %w", err)`; `(value, error)` pairs | A `Result` whose error enum names the operation and keeps the cause (`thiserror` in libraries, `anyhow` in the binary); a small struct where a value and an error both matter | `errors.Is`/`As` become matches on variants |
| `encoding/json` structs and hand-written encoders | serde derive with `serde_json` defaults | No custom text rules to carry |
| `math/rand/v2` PCG and its sampling methods | The `rand` crate's `StdRng`; each seed names its generator and locked `rand` version | A reproducible stream, with no copy of Go's generator |
| `context.Context` passed everywhere | A cancellation token passed explicitly to code that waits; synchronous crates (tuner, journal, simulator, session loop) take none | Cancellation is visible in signatures; tokio stays in the crates that wait on the outside world |
| Value or pointer receivers, shared pointers copied by accident | Ownership: borrow for reads, `Clone` explicitly where Go copied (`forecast` copies the folded state) | Aliasing that Go allowed silently becomes a compile error |

Go code that discards an error (`_ =`, an unchecked `Close`, a best-effort cleanup) becomes explicit handling, never `unwrap()` or `expect()`, which the workspace lints deny outside tests. Where the error is ignored on purpose, say why in the pull request.

## Safety boundaries

These survive every rewrite.

**Journal-failure emergency.** Preserve [runtime.md's journal-failure emergency](spec/runtime.md#exit-codes): after CPU family/model and driver codename validation, and only once workload teardown confirms every backend stopped, an append failure requires writing every core to 0 **without intent**, reading each back, reporting the original journal error and emergency outcome, then stopping without further journal events. Before validation, perform no mailbox command or SMN access; if teardown cannot confirm that workloads stopped, withhold emergency restoration too. The smu crate exposes this constrained zero-only operation separately from the normal intent/write/readback typestate, never as an arbitrary-offset bypass.

**Offset write targets and observed offsets are different types.** A write target is validated to [-50, 0]. An observed offset (a readback, a baseline, any offset decoded from the journal) keeps the raw signed value, never clamped or refused: a firmware readback of +10 is recorded as +10, and clamping it would turn a failed zero restoration into a success. Offsets reach hardware only through the smu crate.

**What typestate proves, and what it does not.** It proves order and authority: no normal write without a journal-minted receipt of a durable `smu.intent`, and no continuation, restoration or shutdown without a verified readback or a confirmed teardown. Safe Rust can drop or forget a value, so types cannot force a readback or a teardown to happen. The write-and-readback transaction and the trial teardown therefore each live inside one owner function that callers cannot split, and runtime fault tests cover every transition, including a failure straight after the write.

**Teardown order.** `defer` runs last-in-first-out. Rust drops locals in reverse declaration order, but struct fields in declaration order, and a moved value drops where it ends up, not where it was declared. Any teardown whose order the spec fixes (for example "process reaped, both readers done, exit event, joined") is an explicit sequence of calls, not an arrangement of `Drop` impls.

**Journal durability.** The journal is append-only with an fsync per event, intent before action, torn-tail repair and the lock.

## Checklist for reviewers

`REVIEW.md` links here. A porting pull request passes when every line holds:

- It does what the spec says, and a changed behavior or format changes the spec in the same pull request. Fixed Go bugs are named in the pull request.
- Each Go test of the ported package maps to a Rust test of the behavior it proved (rules, boundaries, invariants, crash and resume, safety), or the pull request says why it no longer applies. Tests pin behavior, not Go's bytes or random streams.
- It is Rust-native: enums, newtypes, dense arrays, typestate, validated config and serde defaults where the table says so, not a line-by-line copy of Go's shapes, and no code that only reproduces a Go quirk.
- Only the smu crate writes offsets: typestate enforces durable intent before normal writes and verified readback afterward, while the separate zero-only journal-failure emergency keeps the validation and confirmed-teardown prerequisites in `runtime.md`.
- Teardown, cancellation and failure paths keep the order the spec fixes, with explicit calls rather than `Drop` order.
- No `unwrap`, `expect`, `dbg!` or `todo!` outside tests, and `#[expect(…, reason = …)]` replaces `#[allow]`.
- Specs and docs that name the ported behavior still say what the code does.
- The pull request appends its measurements to `rust-migration.md`: dates, lines removed and added, compile and test times next to Go's, regressions, review findings, and agent cost where known.

## Designs the port adopts

On 2026-10-10 the owner closed these Go issues as superseded by the migration, except #541, whose type cut alone was superseded. Their bodies hold the designs the Rust crates adopt from the start: [#104](https://github.com/shgew/togi/issues/104) (run loop), [#124](https://github.com/shgew/togi/issues/124) (one owner per fact), [#550](https://github.com/shgew/togi/issues/550) (markers and archives), [#552](https://github.com/shgew/togi/issues/552) (test seams), [#321](https://github.com/shgew/togi/issues/321) (trial evidence), [#322](https://github.com/shgew/togi/issues/322) (test options), [#323](https://github.com/shgew/togi/issues/323) (MCE parsing), [#541](https://github.com/shgew/togi/issues/541) (hunt enums), [#566](https://github.com/shgew/togi/issues/566) (admitted passes), [#567](https://github.com/shgew/togi/issues/567) (failures), [#573](https://github.com/shgew/togi/issues/573) (together trials), [#614](https://github.com/shgew/togi/issues/614) (incremental tuner state), [#568](https://github.com/shgew/togi/issues/568) (hazards), [#562](https://github.com/shgew/togi/issues/562) (fitter), [#565](https://github.com/shgew/togi/issues/565) (watch words), [#575](https://github.com/shgew/togi/issues/575) (frames) and [#577](https://github.com/shgew/togi/issues/577) (plan types). Where a body says a design must keep a decision byte-identical, ADR 0058 lifts that.
