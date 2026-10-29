# B′: a negative cap and filler experiment

## Question and outcome

Could pooling night exposure across offsets and probing deeper make Ruleset 13's
cap move without changing the daily risk target? The [proposal](https://github.com/shgew/togi/issues/1217#issuecomment-6400502490)
and [2026-10-25 reduced development read](https://github.com/shgew/togi/issues/1217#issuecomment-6403197212)
answer only an exploratory question: **B′ climbed further, overshot more, and
passed no scenario.** Neither A nor B′ met the original registered metrics.
This is not a full registered development score or a holdout confirmation;
the comment explicitly says the holdout was not inspected.

This record supports [#1214](https://github.com/shgew/togi/issues/1214) and
[#1217](https://github.com/shgew/togi/issues/1217), not a production change.
The experimental branch was not pushed as product code. Its source is retained
only as an [inert patch](bprime-results/experiment.patch), following the
repository's practice of preserving historical gate inputs and outputs
([benchmarking](../benchmarking.md#historical-gates)). No current command or
configuration selects B′.

## What changed

The patch is the two-commit experiment `b226aad7` (separate witness construction
from shared cap decisions), then `c3be02d2` (pooled cap and sustained filler),
relative to common ancestor `1f6c924bf2ae4aefb65268781107b5c6e2f02f5c`.
The latter was the retained experimental head, **not a source revision recorded
by the measurements**.

- **A's cap:** use the deepest adequately exposed mapping offset within each
  compatible class to form a Poisson upper rate, then project it shallower with
  the measured/fallback slope. Shallower exposure does not feed that witness.
  A's filler probes one count deeper than S, subject to the edge margin.
- **B′'s cap:** retain the class identity (including content, placement, intended
  duration and firmware context), choose the deepest adequate positive-time
  mapping reference x, and pool all mapping observations of that class, including
  partial observations at other offsets. With signed offsets oᵢ,
  `K = Σ kᵢ`, `E_x = Σ Tᵢ exp(beta * (x - oᵢ))`, and the upper endpoint is
  `chi_square_quantile(0.95, 2(K+1)) / (2 E_x)`. This is one likelihood, not the
  minimum of separate bounds. Classes lacking adequate positive-time mapping
  remain uncovered.
- **Onset:** subtract 600 seconds separately for each actual launch before
  pooling survival time, not once from accumulated time. The implementation uses
  `max(eligible_seconds - max(600 - warm_up_seconds, 0), 0) / 3600` hours.
  Failures count even before onset; stopped launches never receive their intended
  tail. Short mapping launches therefore cannot accumulate fictitious sustained
  exposure. Counting pre-onset failures is conservative for this calculation,
  not a literal onset-only generative model.
- **Filler:** keep CCD selection and rotation but probe the deepest admissible
  signed offset `min(U + 5, 0)` with 1,800-second isolated mapping cells marked
  sustained. Ordinary mapping and qualification matrices retain their durations.
- **Unchanged:** slope selection, CCD count, λ* = 1/3,000 machine-hours, edge
  margin, night shares, qualification and daily rules use the shared policy.
  `cap_with` supplies only the alternate witness function. The simulator enables
  a default-off `sim-bprime` feature and explicitly constructs a diagnostic
  `ruleset13-bprime` policy. Ordinary construction and deserialization select A;
  the selector is not serialized. Added tests cover pooling, onset, filler,
  policy selection, snapshot exclusion and explicit-comparator replay. This note
  does not claim those historical tests were rerun.

## Scored reduced development read

The [reduced registration](bprime-results/dev10.json) contains seeds 1–10,
22 scenarios and 730 calendar days: eight daily active hours and three eight-hour
nights per week. Each policy/slope has 220 JSON Lines records. A is identified by
`comparator: ruleset13` in `dev10-*`; B′ by `comparator: ruleset13-bprime` in
`bprime-*`. The files cross-check the issue comment as follows:

| Measure | A 1.5× | A 2.0× | B′ 1.5× | B′ 2.0× |
|---|---:|---:|---:|---:|
| Scenarios passing | 0/22 | 0/22 | 0/22 | 0/22 |
| Censored runs | 220/220 | 220/220 | 220/220 | 214/220 |
| Maximum promotion steps per CCD | 8 | 8 | 21 | 19 |
| Worst overshoot, both-CCD interaction | 8 | 22 | 18 | 26 |
| Median worst-CCD convergence | infinite | infinite | infinite | infinite |

The comment's A range 0–8 and B′ maximum 21 summarize these maxima. The fixed-cap
promotion limit is seven; moving-cap and firmware-change rows do not score that
limit. Infinite medians do not mean no individual CCD ever recorded convergence.
Censored runs remain in the denominator; their partial counts are lower bounds,
not passes. The B′ verdict tables are diagnostic comparisons, never gate passes.
For example, at 2×, B′ reaches median final mean-CCD depth 32.5 on `edge-deep`
and `no-write-limit-failure`, yet both remain censored and fail convergence.

| Policy | Slope | Rendered tables | Per-run records |
|---|---|---|---|
| A | 1.5× | [dev10-1.5.txt](bprime-results/dev10-1.5.txt) | [JSONL](bprime-results/dev10-1.5.jsonl) |
| A | 2.0× | [dev10-2.0.txt](bprime-results/dev10-2.0.txt) | [JSONL](bprime-results/dev10-2.0.jsonl) |
| B′ | 1.5× | [bprime-1.5.txt](bprime-results/bprime-1.5.txt) | [JSONL](bprime-results/bprime-1.5.jsonl) |
| B′ | 2.0× | [bprime-2.0.txt](bprime-results/bprime-2.0.txt) | [JSONL](bprime-results/bprime-2.0.jsonl) |

JSONL is preserved verbatim. A's text files retain the complete benchmark output
but omit the execution-wrapper opening line and service-status trailer, which
contained nonportable execution identifiers. No execution logs are archived.
The four JSONL files together are about 555 KB, rather than the approximate
300 KB anticipated for the archive; all four are needed for both comparisons.

## Commands and provenance limits

A's original text headers establish these argument vectors (executable and file
locations normalized to portable names):

```sh
togi-tool-bench day-night --registration dev10.json --split dev --policy ruleset13 --slope 1.5 --jobs 24 --out dev10-1.5.jsonl
togi-tool-bench day-night --registration dev10.json --split dev --policy ruleset13 --slope 2.0 --jobs 24 --out dev10-2.0.jsonl
```

They name a release executable, but not its build command or source revision.
B′'s tables establish the policy and slope, and the issue comment establishes
the reduced seed set and horizon. They do **not** retain the exact B′ invocation,
build command, job count or output arguments; no matching shell-history entry
was found. The patch establishes support for
`--policy ruleset13-bprime --slope <1.5|2.0>`, not proof of a particular command's
execution. Do not treat a reconstructed command as an original transcript.

The records name neither source revision nor slope in each JSON object, and do
not embed a registration digest. File names and text headings supply the slope.
The retained registration supplies the reduced definition, but its use by B′
cannot be independently established from its headerless outputs. The source
head `c3be02d2` and patch explain the retained implementation; they do not prove
that every measured executable was built from exactly that clean revision.

Finally, the pooled endpoint is a fixed-look model calculation, **not an
adaptive joint confidence bound**: offsets, exposure allocation and measured
slope depend on earlier observations. It does not provide a stopping-aware
joint rare-event guarantee across classes or CCDs. The 600-second onset and
1,800-second filler are hard-coded experimental choices, not learned hardware
properties. These limitations, censored outcomes and failed metrics preclude
claiming a safe production cap or repairing the original gate after scoring.
The successor design and gate require separate decisions and registration.
