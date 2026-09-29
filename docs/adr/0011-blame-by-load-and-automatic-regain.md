Superseded by [ADR 0020](0020-hunt-and-refine.md).

Confirmation order superseded by [ADR 0015](0015-avx-512-first-in-confirmation.md); the nine workloads and backoff rule remain.

# Blame unattributed failures by load and regain depth automatically

Resident crashes often lack evidence naming a core. Under the escalation window, a second such failure before a clean rotation backed off every core, even when only one core was loaded. Confirmation could miss an R1 or R2 workload that guard later ran, while R7 loaded both CCDs before it could narrow a failure. Manual regain then repeated isolated confirmation at previously tested offsets. These observations motivate wider confirmation and narrower blame; they do not establish that confirmation would have prevented those crashes.

## Decision

At a candidate edge, each core passes every R1 and R2 workload, then one trial each of R3, R4 and R5: nine isolated trials. A failure backs off and restarts confirmation from the first R1 workload.

R7 is one guard step comprising separate resident trials on CCD0, CCD1 and then all cores, sharing one workload. A one-CCD machine runs one all-core trial. A single-CCD trial narrows which loaded cores can be blamed, but the other CCD retains its resident offsets; the CCDs are not electrically isolated. Each inconclusive retry uses the same cores and workload.

An unattributed resident failure backs off each nonzero core loaded by its trial by one count, in scheduling order. An idle crash, R6 or an all-core R7 trial implicates every core; if all cores in a narrower scope are at zero, every nonzero core backs off instead. With no nonzero core, tuning stops at `failure_at_zero`. An attributed failure remains a proven backoff of the named core, including when that core was outside the loaded set. There is no escalation window: each failure is judged by its own load.

At a clean rotation end, unless `--rotations` or a stop signal ends the run first, shycler regains one count on each core with regainable depth, in scheduling order, then changes the profile and guards it in a new rotation. A step is one core at one numeric offset. Its automatic-regain retry is spent when the regain decision is recorded, before SMU writes; replay does not spend it twice. A later suspect backoff from that step settles it: automatic regain does not return that core to the settled step or deeper until `reset --core`. A proven failure cancels unproven depth below the failed mark but does not restore spent retries. One clean rotation end can regain each core at most once, even across interruptions. Bronze requires all cores confirmed, no regainable depth and a clean rotation since the last profile change; settled depth alone does not block it.

The command `shycler regain`, its isolated re-confirmation and the escalation window are removed. The tuning ruleset and journal schema are both 2; an earlier session must be archived with `reset --all` before tuning under these rules.

## Considered Options

- **Keep the escalation window:** rejected because subsequent failures punish unrelated cores even when a trial names a smaller loaded set.
- **Keep manual regain:** rejected because it repeats isolated confirmation already passed at those offsets, while resident guard evidence is what matters.
- **Automatic crash hunts or bisection:** deferred because a passing half-profile does not clear that half, and splitting the load changes the failure condition. Revisit if repeated CCD- or all-core-scope failures settle many cores.
- **Blame by change (last regained cores) or two strikes on the loaded core:** rejected because an unchanged core may fail later, and two crashes with one loaded core may share a package or idle-core cause.
- **Weight blame by distance from each core's failed mark:** rejected as a guess without evidence.
- **Test every workload in R3–R5 at confirmation:** deferred; those regimes retain one trial each.

## Consequences

Confirmation takes 45 minutes per core at the default duration, rather than 25. R7's separate trials narrow blame when only one CCD is loaded, without changing the total default 20-minute step on a two-CCD machine. Lost suspect depth is tested under the resident profile automatically, but a step that fails again is not retried indefinitely. Regain changes the profile and resets clean hours and tier progress; `run --rotations 1` can stop before regain and without Bronze. Schema-1/ruleset-1 sessions cannot be resumed with this build; archiving them starts a new session, not a migration of their evidence.
