# Pull request reviews

These criteria apply to every reviewer. Report concrete defects with their consequence, affected file and line, and priority: P0 blocks all use, P1 needs fixing before merge, P2 is a normal defect, P3 is a minor defect. Review the diff and enough surrounding code to trace its effects. Check the linked issue's `## Acceptance` against the evidence in the pull request.

The review coordinator writes each review's record in the format of [`docs/review-record.md`](docs/review-record.md). The [review command](.omp/commands/review-pr.md) owns orchestration, independence and publishing.

## Defects

- **Offset writes:** normal hardware writes go only through the session's write transaction over `togi-smu`, are typed `TargetOffset` in [-50, 0], record intent before the write and readback after it; the one other path is a constrained zero-only emergency ([runtime.md](docs/spec/runtime.md#offset-types-and-the-write-transaction)). Failure must not claim an offset was applied or verified. Weakening hardware safety or journal durability is P1.
- **Pure decisions:** `togi-tuner` has no I/O, clock or randomness except through injected seams.
- **Traceability:** the journal lets a reader reconstruct actions and decisions. Replay preserves their meaning, validates evidence and handles interrupted or incomplete writes without inventing state.
- **Containment:** trace teardown, cancellation, timeout and startup failure paths, not just successful trials. Follow a second failure while handling the first. No workload escapes containment, survives teardown unnoticed or permits unsafe continuation. Sources: [#174](https://github.com/shgew/togi/pull/174), [#176](https://github.com/shgew/togi/pull/176), [#215](https://github.com/shgew/togi/pull/215), [#255](https://github.com/shgew/togi/pull/255).
- **Root file opens:** inspect every file opened as root under a workload-writable path, including data and logs. Check symlinks, FIFOs, ownership, permissions, umask and relative paths. A path check alone does not secure a later open. Source: [#204](https://github.com/shgew/togi/pull/204).
- **Hardware identity:** physical core IDs, logical CPUs, CCDs and SMU slots are validated and mapped explicitly. Enumeration order, contiguity and identity between different numbering systems are not evidence. Sources: [#69](https://github.com/shgew/togi/issues/69), [#256](https://github.com/shgew/togi/pull/256).
- **Concurrency and deadlines:** shared state is synchronized; cancellation reaches blocked work; deadlines bound the whole operation and its failure paths. Cleanup must not wait forever or race continued use.
- **Contracts:** specs describe current behavior, and the linked issue's Acceptance is met for this pull request's scope. Docs may land in the lowest layer of a stack ahead of their code. Flag statements that are wrong or contradict code or other docs, not ones this layer does not implement yet.
- **Breaking changes:** a value change to `togi_journal::SCHEMA` or `togi_tuner::RULESET` carries a `[BREAKING]` title, `breaking` label and `**BREAKING**` changelog line. Nothing else carries these markers.
- **Public content:** the diff, commit messages, pull request body and review record follow the Public rule in `AGENTS.md`. A breach is P1. Say what leaked and where without quoting it, since the review is public too.
- **Tests:** tests pin consumer-visible behavior, boundaries, invariants and transitions. A bug fix has a test that fails before the fix and passes after it, or states why that proof is impractical and provides a scoped reproduction. A test that writes to disk, starts a process or uses the real clock without needing to is a finding ([ADR 0073](docs/adr/0073-tests-and-simulations-run-in-memory.md)). Evidence cited for a change must come from a check that exercises the changed package: [what each check proves](docs/benchmarking.md#what-each-check-proves).
- **Test cost and survivors:** apply [the test-cost rules](#test-cost-and-survivors) to new and changed tests, and check every assertion-to-survivor pairing when a test is deleted, replaced, shortened or narrowed. A missing pairing or unstated reason for a test over 1 s in CI is a finding. Weakening a safety test is P1.
- **Rust lints and unsafe:** Rust follows the lint and unsafe policy in `AGENTS.md`. A new `#[allow]`, an `#[expect]` without a `reason`, an `unsafe` block outside `togi-trial` and `togi-hardware`, or one without a `// SAFETY:` comment is a finding. A `disallowed_methods` expectation must name why that call site is approved.
- **Untested changed code:** judge every reported uncovered changed range and `not built for <platform>` item by [the coverage policy](#coverage). A gap is a finding only when a consumer could see behavior break unnoticed.

Style, wording, naming taste and optional refactors are not findings. Handle findings by the rule in `AGENTS.md`.

## Test cost and survivors

A test is kept for what it proves and made faster where it is slow. Test count is not a cost proxy: measure wall time, peak memory and build time, and rank speed fixes and deletions together by their savings.

- **Safety tests** for writes, receipts, emergency zeroing, teardown, journal durability and crash-resume are never removed or weakened, only made faster. A ruleset cutover does not retire their invariants.
- **One owner per behavior:** pin a behavior once, at the lowest layer that observes it. Higher layers keep one test per seam crossing rather than repeating each rule; delete higher-layer duplicates.
- **Boundary rows:** parametric tables keep just below, at and just above each boundary, plus one row per branch. A dimension the code branches on is distinct; values on an already-covered branch are not.
- **Rendered output:** keep one compile-time snapshot per distinct layout or state, not extra assertions of wording, positions or render counts that snapshot already pins.
- **Slow shared fixtures:** commit compile-time fixtures, with one test regenerating and checking them. Do not use a disk cache or fold separately reported cases into loops.
- **Process behavior:** a new test starts real processes once per distinct behavior in one `real_process_*` test. Every existing process contract keeps its test.
- **Regrowth:** a new or changed test taking over 1 s in CI states its reason in the pull request. Nextest's `slow-timeout` stays unchanged.
- **Removal:** each removal pull request covers one class of test.

Each pull request or fix that deletes, replaces, shortens or narrows a test, sweep or table maps every behavior and assertion no longer exercised to the retained or replacement test that pins it. Stopping a test earlier needs the same map for assertions it no longer reaches. A genuinely retired behavior cites the owner-approved spec change or decision retiring it. Matching counts of cases, cuts or seeds do not establish survivors. Put the map in the pull request body, or the fix's review reply; the first independent review checks every pairing.

Rejected:

- **`LazyLock` shared fixtures:** nextest runs each test in its own process, so they share nothing between tests.
- **Hard per-test time limits:** the `slow-timeout` period also sets the kill timeout; a valuable slow test needs a stated reason, not a shorter deadline.
- **A test-count campaign, including against tests under 10 ms:** count does not identify the slow families and rebuilds that dominate cost.
- **Permanent retired-invariant inventories:** survivor evidence belongs to the pull request that stops pinning the assertion, where review can check it.

## Merge units

Plan merge units before code so reviewability and safe landing order are design decisions rather than reactions to a finished diff. Each pull request is independently acceptable and revertible, even if later units never land.

Before coding a design spanning several crates or any `Ruleset N` change, list its merge units in the issue's Pull requests section, in landing order. Each entry states its own worth, how it reverts, dependencies and which unit is the atomic behavior or session switch. Behavior-preserving seams, additive types and readers, and removals after the switch are candidates for separate units; an uncalled module is not a worthwhile unit. A ruleset change has one atomic switch with its bench gate, not all preparation in the switch pull request.

Each entry estimates hand-written changed lines, added plus removed, separately from generated snapshots, goldens, corpora and input fixtures. An estimate over 10,000 hand-written lines is a reviewability signal. For a pull request over that size, planned or found, the review coordinator decides whether it is reviewable as planned, for example by partitioning review by invariant and module. It escalates to the owner only when recommending rejection. Size alone never mandates a split, a CI gate or a file-count limit.

Rejected:

- **Warnings only after opening, or file-count triggers:** the code is already written, file counts trigger too often and generated output inflates them.
- **Automatic splits:** coupled retirement and replacement can require an atomic cutover; forced layers create unwired scaffolding or reconstruct intermediate implementations rather than independently useful units.

## Coverage

Coverage of changed code is evidence for reviewer judgment, never a target: no threshold, ratchet or tracked overall percentage. A test pins behavior, not merely execution of a line. Mechanical thresholds reward assertion-free tests, self-echoing mocks and reshaping code to reach impossible paths, rather than fault detection.

Reviewers receive only uncovered changed lines, merged into `path:first-last` ranges, plus `path: not built for <platform>` items: changed files or changed items under a `cfg` the reviewing platform does not satisfy or cannot decide, with no measured lines. A line appearing repeatedly in the profile is reached if any listing reached it. Judge every range and not-built item in the assigned files. A range is a finding only when a consumer could see it break unnoticed: behavior, a boundary or an operator-visible error. Impossible defensive paths and tests that would merely reach a line are not findings. Code reached only by VM checks, hardware tests or the target machine may be uncovered or not built; read its tests and rely on CI for the platform that builds it. A profile's absence is not proof of missing tests.

The [review command](.omp/commands/review-pr.md#coordinator-freeze-and-split) owns running and distributing `just cover BASE`; the [record format](docs/review-record.md) keeps the range count and judgment, including not-built files, so the owner can assess the reasoning.

Rejected:

- **Percentage thresholds or ratchets:** they turn evidence into a target that can be met without checking behavior.
- **CI metric comments:** reviewers act on the evidence; a second reporting location adds automated traffic without judgment.
- **Whole-file uncovered blocks or whole blocks touching a diff:** unchanged gaps bury the change and make counts depend on manual matching; report only changed lines.
- **External diff-cover/conversion pipelines:** the repository's coverage tool intersects the diff and native trace without another toolchain and converter.
- **Mutation testing as the review mechanism:** it adds another tool; reviewers remain responsible for whether assertions catch consumer-visible faults.

## Lessons

When the same kind of finding occurs in two pull requests, add a lesson in the pull request that fixes the second. Cite the source pull requests or issues.

- **Net release behavior:** pending changelog fragments in `changes/` describe the behavior that will ship, not each intermediate commit. A later layer of the same release edits or deletes the fragment of a feature it supersedes or removes. Sources: [#269](https://github.com/shgew/togi/pull/269), [#285](https://github.com/shgew/togi/pull/285).
- **Hermetic package source:** package unit and integration tests run without the full checkout, its Git metadata or non-shipped documentation. Fixtures must own their inputs, including any temporary Git repository, or use files in the package source set rather than assuming checkout-only files exist. Check invariants about non-shipped documentation at repository review level. Sources: [#659](https://github.com/shgew/togi/pull/659), [#667](https://github.com/shgew/togi/pull/667).
- **Object-only decoding:** serde's derived `Deserialize` for a struct also accepts a JSON array of its fields in declaration order, and `deny_unknown_fields` does not stop it. A Rust type decoded from a JSON object, such as configuration, journal payloads and state files, must reject arrays, or a correctly sized array silently sets fields by position. Its strict-decoding tests include a correctly sized array. Sources: [#748](https://github.com/shgew/togi/pull/748), [#770](https://github.com/shgew/togi/pull/770).
- **Migrated consumers:** a change to what a tuner value means migrates every explanation, view and history line that consumes it, and no test keeps passing on the old meaning. A list that was once only a combination's member probes also carried a singleton's probes, and the watch panel and story still told a combination and a tested passing probe. Sources: [#1026](https://github.com/shgew/togi/pull/1026), [#1027](https://github.com/shgew/togi/pull/1027).
