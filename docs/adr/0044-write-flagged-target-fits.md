# Write flagged target fits

## Context

[ADR 0029](0029-bench-verdicts-rest-on-fitted-machines.md) Decision 4 has `just fit` restart each bootstrap refit that the model check flags from the passing all-facts fit, constrained to every original group's unchanged 99% interval. It assumed the all-facts fit passes. Against the refreshed extract it does not: on one R7 group, mprime AVX2 with CCD0 (cores 0–7) loaded, 300 s, depth −24, it predicts an interval of [0, 6] failures against 7 in 24 trials, a two-sided p of about 0.002. The legacy R7 joints memorize failing profiles and cannot give that group's neighbours no failures and its threshold profile about half at once. With nothing to restart from, the first flagged refit stopped the run, and `just fit` wrote no machine file ([#479](https://github.com/shgew/togi/issues/479)).

The committed `target-fit-*` files therefore predated the refresh. Under the same check against the refreshed extract, each of them flagged four to seven groups, eight distinct groups across the nine files. The model pull requests that regenerate the fits ([#306](https://github.com/shgew/togi/issues/306)'s pull requests 4, 6 and 7) could not land.

## Decision

1. When the all-facts fit passes the model check, nothing changes: a flagged bootstrap refit restarts from it under the constraint of ADR 0029 Decision 4.
2. When the all-facts fit fails, `just fit` writes every bootstrap refit as fitted, without constraints, and its output says so for each flagged refit.
3. Every written member's header records its model check against the original extract: `ok` with no flagged groups, or `flagged` with each flagged group's class, loaded cores, intended duration, depth, observed counts and interval. The fit report lists the same groups.
4. The model check is unchanged, and so is ADR 0029 Decision 3: a flagged member cannot support a target-machine claim.

This amends ADR 0029 Decision 4 for the case where the all-facts fit fails. A model pull request is then held to #306's merge bar: it flags no group that was not flagged before.

## Considered Options

- **A Holm correction across the 52 eligible groups:** rejected. It makes the per-group check about 52 times more lenient. The failing group would pass at p ≈ 0.002, and so would the CCD1 120 s depth −39 group, both real misfits of the legacy joints. The check would stop seeing what the model gets wrong.
- **Leave `just fit` blocked:** rejected. The committed fits would stay older than the evidence and flag more groups than a refit does, and every model change that regenerates them would wait.
- **Fit the shared-voltage model into the target ensemble ([#480](https://github.com/shgew/togi/issues/480)):** the real fix, and the only option that keeps both the check and ADR 0029 unchanged. The in-sample shared-voltage anchor fits the failing group, but the forward-fitted candidate predicted 2.12 times the observed R7 failures, missing [#105](https://github.com/shgew/togi/issues/105)'s bar, so it is not ready to replace the legacy fit. It stays the next model item; this decision does not wait for it.

## Consequences

- `just fit` completes on the refreshed extract and writes all nine members. On the committed extract every member is flagged, on the CCD0 300 s depth −24 group, the CCD1 120 s depth −39 group, or both, and no member supports a target-machine claim until a model passes the check.
- While the all-facts fit fails, the refits are an unconstrained bootstrap ensemble: nothing truncates their spread to the original intervals. Once an all-facts fit passes, the constrained restart returns without a further decision.
- The flags travel with the files. A reader of a machine file, the fit report or `just bench`'s model check sees the same flagged groups, and a model change can be compared by the groups it flags.
