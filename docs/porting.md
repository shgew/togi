# Porting togi from Go to Rust

[ADR 0048](adr/0048-rust.md) decides the migration and [#634](https://github.com/shgew/togi/issues/634) stages it. This guide is what every porting pull request follows, and what `/review-pr` checks it against. [`rust-migration.md`](rust-migration.md) is the log that each stage appends to.

The contract: **the language changes, the behavior does not.** The `tuner.Ruleset`, journal schema 4, the config format, the CLI surface, `status`, `events`, `doctor` and dashboard output stay as they are. The Rust build must write journals byte-identical to Go's on every suite session, ignoring only build stamps (`just same`).

## The rule

A porting pull request may change structure, never behavior. Anything that would alter a journal byte, a golden file, a decision or an exit code lands separately: in Go before the freeze, or in Rust after the cutover. If a port exposes a Go bug, port the bug, file an issue, and fix it in its own pull request. Bugfixes in Go stay allowed; once the area is ported, the comparison in CI forces the same fix into Rust in the same pull request.

The comparison assets are protected from the moment stage 2 ([#637](https://github.com/shgew/togi/issues/637)) builds the Go-vs-Rust comparison: the Go reference, golden files, fixture journals, `just same` and the CI comparison job then change only in pull requests the owner approves. Before that, Go pull requests that implement a decided issue of #635 or Ruleset 11 update goldens, bench baselines and machine files as the issue requires, under the normal merge rules; changes to `just same` mechanics, the CI comparison job or the content of existing fixture journals still wait for the owner. A port that needs a golden to change is not a porting pull request.

## Go idiom, Rust idiom

Port to the Rust idiom from the first crate. A verbatim port refactored afterwards was rejected (ADR 0048): the probe's straight port ran eight times slower than the Rust-native one.

| Go | Rust | Why |
|---|---|---|
| String or int kinds (`journal.Kind`, outcomes, phases, hunt results) compared as literals | One `enum` per domain, matched exhaustively with no `_` arm over a domain enum | A new variant fails to compile at every site that must handle it, which `exhaustive` only approximates in Go |
| Payload structs plus a registry (`payloadTypes`) | One event enum whose variants carry the payloads; the wire name lives on the variant | The registry and the type switches cannot disagree |
| `int` for core IDs, offsets, counts, sequence numbers | Newtypes (`CoreId`, `Offset`, `Count`, `Seq`) with checked constructors, such as an offset clamped to [-50, 0] | A core ID cannot be passed as an offset |
| `map[int]T` plus sorting at every use | A dense per-core array (`[T; 16]` with a presence mask, as #629's `PerCore[T]` does), iterated in index order | No hashing, no sort, no allocation per sample; supported core IDs are 0–15 |
| Comments and review saying "record intent, write, read back" | Typestate for normal writes: `Intent` → `Written` → `Verified`, each step consuming the last; only the smu crate can construct them | A normal write that skips its durable `smu.intent` does not compile; the constrained emergency-zeroing path below remains separate |
| `defer` teardown of trials | A typestate for the trial lifecycle (`Running` → `Reaped` → `ReadersDone` → `Ended`) with an explicit async `teardown` that returns its errors; `Drop` only as a backstop that logs | `Drop` cannot await, cannot return an error and must not decide whether teardown was verified |
| `RawConfig` decoded, then checked in scattered places | `RawConfig` is parsed once into a validated `Config`; the rest of the code takes only `Config` | Invalid states are unrepresentable past the boundary |
| `fmt.Errorf("…: %w", err)` | A `Result` whose error enum names the operation and keeps the cause (`thiserror` in libraries, `anyhow` in the binary); preserve the message text where an exit code or output shows it | Operators see these strings; `errors.Is`/`As` become matches on variants |
| `context.Context` passed everywhere | A cancellation token passed explicitly to code that waits; synchronous crates (tuner, journal, simulator, session loop) take none | Cancellation is visible in signatures; tokio stays in the crates that wait on the outside world |
| Value or pointer receivers, shared pointers copied by accident | Ownership: borrow for reads, `Clone` explicitly where Go copied (`forecast` copies the folded state) | Aliasing that Go allowed silently becomes a compile error |

Preserve [runtime.md's journal-failure emergency](spec/runtime.md#exit-codes): after CPU family/model and driver codename validation, and only once workload teardown confirms every backend stopped, an append failure requires writing every core to 0 **without intent**, reading each back, reporting the original journal error and emergency outcome, then stopping without further journal events. Before validation, perform no mailbox command or SMN access; if teardown cannot confirm that workloads stopped, withhold emergency restoration too. The smu crate exposes this constrained zero-only operation separately from the normal intent/write/readback typestate, never as an arbitrary-offset bypass.

`defer` runs last-in-first-out. Rust drops locals in reverse declaration order, but struct fields in declaration order, and a moved value drops where it ends up, not where it was declared. Any teardown whose order the spec fixes (for example "process reaped, both readers done, exit event, joined", [#321](https://github.com/shgew/togi/issues/321)) is an explicit sequence of calls, not an arrangement of `Drop` impls.

## Byte hazards

The comparison catches these. Each is a place where a faithful-looking port writes different bytes. Add a test at the boundary for every one your crate touches.

1. **Float formatting.** Go's `encoding/json` writes `1.0` as `1`, `0.000001` as `0.000001`, `1e-7` as `1e-7`, `1e20` as `100000000000000000000` and `1e21` as `1e+21`: exponent form only below 1e-6 or from 1e21, with a signed exponent. `serde_json` writes `1.0` as `1.0` and uses its own exponent thresholds. Rule: write floats through one function in the model crate that reproduces Go's shortest-representation algorithm and thresholds, and test it at 1, 0.1, 1e-6, 1e-7, 1e20, 1e21 and the values in the golden journals. `fmt.Sprintf("%v", f)` is a different format: it writes `1e-05`, `1e-07` and `1e+20`, and `1.234567895e+08` for 123456789.5. Port each use to the format its Go call site uses.
2. **`omitempty` and null versus empty.** In Go, `var s []int` marshals to `null`, while `s := []int{}` marshals to `[]`; nil and empty maps similarly produce `null` and `{}`. With `omitempty`, nil and empty collections, zero numbers and `false` are omitted according to each field's Go type and tag. Rule: retain null versus empty with `Option<Vec<T>>` (`None` versus `Some(vec![])`) or the corresponding optional map; a bare `Vec<T>` loses the nil distinction. Separately implement each field's Go `omitempty` rule with explicit skip predicates, rather than treating `None` alone as empty.
3. **Integer-keyed map order.** Go sorts the keys of a `map[int]V` as strings: `{"1":…,"10":…,"2":…}` (`voltage_requests_v`, `ccd_mhz`). A Rust `BTreeMap<u32, V>` orders `1, 2, 10`. Rule: serialize these maps by sorting the keys' decimal strings; dense per-core arrays serialize through the same function, never in index order.
4. **Timestamps.** Journal times use the layout `2006-01-02T15:04:05.000000000Z`: always nine fractional digits, always UTC `Z`. Formatters that trim trailing zeros or switch to `+00:00` change every line. Rule: one function formats event times with a fixed nine-digit fraction, and nothing else formats them.
5. **Envelope field order.** An event is `seq`, then `mono_ms` (only when non-zero, or when stamping), then `time`, `boot`, `kind`, `msg`, then the payload's fields flattened into the same object (omitted entirely when the payload encodes as `{}`), then `cause` (only when non-empty). Rule: encode the envelope by hand in that order; do not rely on `#[serde(flatten)]` ordering. HTML characters are not escaped (`<`, `>`, `&` stay literal).
6. **Seeded random streams.** Simulated sessions draw from `math/rand/v2`: `rand.New(rand.NewPCG(seed, fnv64(seed, parts…)))`, where `fnv64` is FNV-1a 64 over `fmt.Sprintf("%v", part)` of every part joined with `|`. The hash input is Go's formatting (hazard 1 applies to float parts, and integers print in decimal). Rule: stage 4 ports the PCG generator and every sampling method togi calls on it (integer ranges, floats, normal and exponential draws) bit for bit, and reproduces the `%v` formatting for each seed part. Stage 11 replaces it with the `rand` crate after the cutover and records the generator next to each seed.
7. **Integer overflow.** In Go, `x := uint8(255); x++` yields 0; Rust's equivalent addition panics with overflow checks enabled and wraps when they are disabled. Rule: reproduce that operation as `x.wrapping_add(1)`, and use explicit wrapping arithmetic wherever Go relies on it, so debug, release and tests agree; do not substitute checked or saturating arithmetic that changes the result.
8. **Errors Go tolerated.** Go code that discards an error (`_ =`, an unchecked `Close`, a best-effort cleanup) keeps working; a Rust `unwrap()` or `expect()` on the same value panics, and the workspace lints deny both outside tests. Rule: port the handling, not the call. If Go ignored the error, ignore it explicitly and say why in the pull request; if Go logged it, log it.

## Checklist for reviewers

`REVIEW.md` links here. A porting pull request passes when every line holds:

- It changes structure only: no decision, schema, ruleset, config, CLI, exit-code or output change, and no new feature. Behavior changes are in their own pull request.
- It touches no protected comparison asset (Go reference, goldens, fixture journals, `just same`, the CI comparison job; protected since stage 2 built the comparison), or the owner approved that change in the pull request. Any weakening of a comparison (a wider ignore list, a skipped session, a looser tolerance) is a P1.
- The crate's comparison against Go passes on every suite session and seed, and the pull request shows the command and its result. A crate the suite does not reach shows how its Go tests were ported and that they pass.
- Each byte hazard above that the crate touches has a test at its boundary.
- It is Rust-native: enums, newtypes, dense arrays, typestate and validated config where the table says so, not a line-by-line copy of Go's shapes.
- Only the smu crate writes offsets: typestate enforces durable intent before normal writes and verified readback afterward, while the separate zero-only journal-failure emergency preserves the validation and confirmed-teardown prerequisites in `runtime.md`.
- Teardown, cancellation and failure paths keep the order the spec fixes, with explicit calls rather than `Drop` order.
- No `unwrap`, `expect`, `dbg!` or `todo!` outside tests, and `#[expect(…, reason = …)]` replaces `#[allow]`.
- Specs and docs that name the ported behavior still say what the code does.
- The pull request appends its measurements to `rust-migration.md`: dates, lines removed and added, compile and test times next to Go's, comparison failures and what they caught, regressions, review findings, and agent cost where known.

## Design inputs from Go issues superseded by the port

On 2026-10-10 the owner closed these Go issues as superseded by the migration, except #541, whose type cut alone was superseded. The Rust crates adopt the designs below from the start; the linked issue bodies hold the detailed decisions, evidence and required invariants. Each design is shape only: it must keep every decision byte-identical.

**Session and journal (stages 2, 3 and 5)**

- [#104](https://github.com/shgew/togi/issues/104), the run loop: use named phases returning explicit continue, stop or error results, with `run` listing startup/recovery, scheduling, profile lifecycle and shutdown/restore in order under one cleanup owner. Preserve the boot-specific dead-end/preflight ordering and bounded reconciliation on cancellation, make journal identity allocation mandatory and journal-owned, and share retry tables and recovery-read helpers without merging their distinct semantics.
- [#124](https://github.com/shgew/togi/issues/124), one owner per fact: a canonical journal replay model owns shared identity, topology, recorded phase, applied profile, open intent, dead end and workload rotation; the tuner owns strategy and idle-crash attribution, recovery owns operational evidence, facts owns provenance, and presentation projects from these owners. Every consumer uses one observer → shared → strategy fold recipe, with observers seeing pre-event state, distinct types for core phase and session activity, distinct meanings for recorded application versus hardware certainty and scheduled versus crashed trials, and prefix-invariant tests rather than merging reducers or testing duplicated reducers for agreement.
- [#550](https://github.com/shgew/togi/issues/550), markers and archives: one marker type with explicit lifetimes (compat-pending and carry-pending are removed when their step completes; reset-all is permanent and has no remove) and one archive listing, where each caller keeps its own filter, order and error policy. The sync, close and remove order and the file modes stay.
- [#552](https://github.com/shgew/togi/issues/552), test seams: one append trigger (a predicate or the nth match, before or after the append, one-shot, effect or error) in a small test-support crate below session and sim; wrappers that inject other faults stay, and the run-and-reboot harnesses stay separate because their lifecycles differ.

**Trial processes and hardware (stage 5)**

- [#321](https://github.com/shgew/togi/issues/321), trial evidence: give output buffering, framing and polling one bounded owner, and share one transition selector between the live runner and final adjudication rather than moving precedence into the session's final switch alone. Preserve live stop/report/suppression behavior, first-selected attribution, field clearing, signal order, fairness and budgets, and the teardown sequence: process reaped, both readers done, exit event, joined.
- [#322](https://github.com/shgew/togi/issues/322), test options: expose only the production inputs (`Dir`, `Backends`, `Cores`, `User`, the PM table); timing and filesystem roots are private construction parameters, and unit fakes run scoped with a valid identity. Unscoped execution exists only as a private path for integration helpers and is not containment.
- [#323](https://github.com/shgew/togi/issues/323), MCE parsing: one private parser with named local state and a separate unresolved-ownership state, since retrospective clearing across surplus continuations needs the two lifetimes. Preserve every MCE, retained line, conservative bank attribution, retrospective clearing and each fixture.

**Tuner (stage 3)**

- [#541](https://github.com/shgew/togi/issues/541), the type cut only: use exhaustive enums for hunt stage, hunt result and group outcome, separate from trial outcome, retaining unknown-string decoding and fallback but removing the unwritten `fail` group alternative. The event-line colour change is not part of the port; #541 stays open for it after the cutover, blocked by [#644](https://github.com/shgew/togi/issues/644).
- [#566](https://github.com/shgew/togi/issues/566), admitted passes: one non-allocating iterator over passes admitted for a class, profile and window up to a sequence serves the pass counts, the sequence queries and the checking exposure. R7 self-sufficiency status, the R7 voltage target, the temperature peak, provenance lookups and reset deletion keep their own scans, because the spec gives them different rules.
- [#567](https://github.com/shgew/togi/issues/567), failures: keep no second copy of failures. The short-failure veto scans the ledger's failures and the idle failures, and carried-failure handling uses the entry the evidence step returns.
- [#573](https://github.com/shgew/togi/issues/573), together trials: carry no profile on the trial. Skip-known-failure, `forecast` and the runner read the tuner's current profile.
- Tuner state in general follows [#614](https://github.com/shgew/togi/issues/614), selected in the migration epic: each rule group keeps its own state, `next` only reads, `forecast` copies the folded state.

**Simulator and tools (stages 4 and 11)**

- [#568](https://github.com/shgew/togi/issues/568), hazards: enumerate each hazard mechanism once, with rate, delay, source, core, CCD, joint identity, signal and attribution metadata, and let the draws, the steady rate, the integrated failure probability and the R7 attribution share all read that list without allocating. Keep the draw order (core, voltage, CCD, joint, background) and the RNG stream names, since they fix the journals (hazard 6).
- [#562](https://github.com/shgew/togi/issues/562), fitter: search primitives take their objective as an argument. Today's receiver-specific objectives stay, including the mixed objective of the shared-voltage anchor fit (searches minimize the raw likelihood, acceptance uses the augmented score), so committed machine files stay byte-identical; the mixed objective is a known limitation to document, not to fix in a port.

**Watch (stage 6)**

- [#565](https://github.com/shgew/togi/issues/565), words: use one exhaustive word table per event kind for history, tags, detailed story and compact story, with explicit silent entries and the unknown-kind fallback. Sequence-aware context such as grouping carried groups and later decisions naming moved cores stays outside the table.
- [#575](https://github.com/shgew/togi/issues/575), frames: fold the journal into the tuner once per frame and clone the folded state for each forecast premise, so branches cannot change the folded state or one another. Show frame time on the longest bench session before and after.
- [#577](https://github.com/shgew/togi/issues/577), plan types: watch reads the tuner's cycle and hunt plan types directly, using stored part and step identity for matching, counts, ordinals and schedule text rather than re-deriving them. Presentation timestamps and signals stay watch-owned and keyed by part or group, and watch and status output stay unchanged.
