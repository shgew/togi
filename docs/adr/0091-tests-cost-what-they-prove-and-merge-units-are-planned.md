# Tests cost what they prove, and merge units are planned before code

Status: see [the index](README.md).

Records the test rules the owner decided on 2026-10-26 ([#1265](https://github.com/shgew/togi/issues/1265)) and the planning and retired-invariant rules decided on 2026-10-28 from [#1277](https://github.com/shgew/togi/issues/1277)'s investigation ([#1268](https://github.com/shgew/togi/issues/1268)). #1265's rule that every pull request runs all simulator sessions is not recorded: [ADR 0087](0087-simulations-are-research-tools-not-checks.md) made simulations research tools before this ADR landed. Which checks run where (the local tier, the CI tier, and what runs on demand) is owned by ADR 0087 and ADR 0089 ([#1328](https://github.com/shgew/togi/issues/1328)); this ADR does not restate it.

## Context

Test time sits in a few slow families and in rebuilds, not in the number of tests. On 2026-10-26 main had 11,841 tests. 74% took under 10 ms and accounted for 4% of summed test time; the 308 tests over 1 s accounted for 67% (#1265). Ruleset 13 was planned to remove about 2,700 of them.

Ruleset 13's implementation pull request, [#1261](https://github.com/shgew/togi/pull/1261), changed 845 files, +49,590/−149,287 lines. Generated output (snapshots, the generated narration corpus and input fixtures) was 170 files and 14.2% of changed lines; hand-written Rust was 609 files, +41,237/−126,081. Most of the size predates review: the first eight commits already changed 830 files. Its last review round alone took over three hours. The cutover deleted main's every-durable-event crash-resume sweeps with no replacement, and the first two review rounds missed it, although deleting old session tests does not retire their cross-ruleset safety invariants. Restoring the number of cuts a sweep covers does not show that each deleted assertion survives. #1277 found that the ruleset rule needs one atomic behavior and session switch, not one pull request holding all the preparation, and that some of #1261's preparation could have landed earlier only if it had been planned before coding. Splitting #1261 after the fact would have removed under 1% of its changed lines at days of rework ([#1277's findings](https://github.com/shgew/togi/issues/1277#issuecomment-6427409902)).

## Decision

**A test is kept for what it proves and made faster where it is slow. A pull request that stops a test from reaching an assertion maps that assertion to the test that still pins it. Merge units are planned before code, with their size in hand-written lines.**

### Tests

- **Safety tests** are never removed or weakened, only made faster: writes, receipts, emergency zeroing, teardown, journal durability and crash-resume.
- **One owner per behavior.** A behavior is pinned once, at the lowest layer that observes it. Higher layers keep one test per seam crossing, not one per rule, and higher-layer duplicates are deleted.
- **Parametric tables** keep the rows at each boundary (just below, at and just above) plus one row per branch. Rows that differ only in values on the same branch go. Rows that differ in a dimension the code branches on are distinct.
- **Rendered output** is pinned by one compile-time snapshot per distinct layout or state. Tests that re-assert wording, positions or render counts a snapshot already pins are deleted.
- **Removal pull requests** each cover one class of test.
- **Order** follows measured savings in wall time, peak memory and build time. Speed fixes and deletions compete on one list. There is no campaign against tests under 10 ms.
- **Shared slow fixtures** become committed compile-time fixtures, regenerated and checked by one test. There is no disk cache ([ADR 0062](0062-checks-need-no-race-detector-or-test-result-cache.md), [ADR 0073](0073-tests-and-simulations-run-in-memory.md)), and separately reported cases are not folded into loops ([#915](https://github.com/shgew/togi/issues/915)).
- **Process tests:** a new test starts real processes once per distinct process behavior, in one `real_process_*` test. Every existing process contract keeps its test.
- **Regrowth:** a new or changed test that takes over 1 s in CI needs a stated reason in its pull request. Nextest's `slow-timeout` stays as it is, since shortening its period also shortens the kill timeout.

### Retired invariants

- A pull request or fix that deletes, replaces, shortens or narrows a test, a sweep or a table maps every behavior and assertion no longer exercised to the retained or replacement test that pins it. A test made to stop earlier maps every assertion it no longer reaches the same way ([#496](https://github.com/shgew/togi/issues/496)). A behavior that is genuinely retired cites the owner-approved spec change or decision that retires it.
- The map names assertions, not counts: a matching number of cases, cuts or seeds is not a map.
- The map goes in the pull request body, or in the fix's reply for a review fix, not in a permanent inventory. The first independent review checks every pairing. `just cover` is evidence only ([ADR 0030](0030-coverage-is-a-review-signal.md)).
- Removal pull requests and cutovers follow the same rule: replacing a ruleset does not retire the safety invariants its old tests pinned.

### Merge units

- Before code, a design spanning several crates and every `Ruleset N` change list their merge units in the issue's Pull requests section, in landing order. Each entry states what the unit is worth on its own, how it reverts, what it depends on, and which unit is the atomic behavior or session switch.
- Behavior-preserving seams, additive types and readers, and removals after the switch are the candidates for their own units. A module nobody calls yet is not a merge unit. A ruleset change needs one atomic switch with its bench gate, not one pull request holding all its preparation.
- **Size signal:** each entry estimates its hand-written changed lines, added plus removed, with generated output (snapshots, goldens, generated corpora and input fixtures) counted separately. A unit estimated at over 10,000 hand-written changed lines says so in the plan. Its review coordinator decides whether the pull request is reviewable as planned, for example with review partitioned by invariant and module, and escalates to the owner only when it recommends rejecting the pull request.
- Size alone still does not justify splitting. The signal triggers a decision, never an automatic split, a CI check or a file-count limit.

## Considered Options

- **Share slow fixtures across tests through `LazyLock`:** rejected. Nextest runs each test in its own process, so an in-memory `LazyLock` shares nothing between tests.
- **Select simulator sessions by coverage:** rejected by #1265, which kept every session on every pull request. ADR 0087 then removed the simulator suite from the checks, so no check selects sessions at all.
- **A hard per-test time limit:** rejected. Nextest's `slow-timeout` period also sets the kill timeout, and some slow tests are worth their time; a stated reason in the pull request is the check.
- **A campaign by test count:** rejected. Tests under 10 ms are three quarters of the tests and 4% of the time.
- **A size warning when the pull request is opened, or a 100-file trigger:** rejected. The code is already written by then, and recent merges often change more than 100 files, so a file count would ask for decisions every week. Generated output inflates both counts.
- **Split oversized pull requests automatically:** rejected. #1277 found most of #1261's size was coupled retirement and replacement, not separable layers; an enforced split would create unwired scaffolding or reconstruct intermediate implementations.
- **A permanent inventory of retired invariants:** rejected. The map is needed when a test stops pinning something, and belongs to that pull request's review.

## Consequences

- Removal pull requests, cutovers and fixes that shorten a test carry an assertion map, and their first review is where a missing survivor is found.
- A large design costs a merge-unit plan before code; a ruleset change can land its preparation ahead of the switch.
- A plan over the size signal gets a reviewability decision from its review coordinator, and the owner sees only a recommendation to reject.
- Correctness is proven by ordinary nextest tests (ADR 0087), so test cost is where check time is won or lost.
