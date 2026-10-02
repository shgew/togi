# Reviewing a run

`tools/stats` is a read-only development program, not part of the installed togi binary. Run it from a source checkout. It reads only `events.jsonl`, not `state.json`, trial output or hardware; current journals and archives from older shipped schemas are accepted. A live journal's unfinished final line is ignored.

```sh
just stats                                                # target machine: /var/lib/togi/events.jsonl
just stats --state-dir <copied-state-dir>                  # another machine, no hardware or root needed
just stats --journal <state-dir>/archive/<session>.jsonl   # one archived session
just stats --since 2026-09-30T00:00:00Z
```

`--since` filters trials and hunts by their start time, and warnings, failures and rotation events by event time. Runs overlapping the cutoff retain their full totals. Session metadata and earlier evidence remain available, so a cutoff does not erase the passes used to evaluate a hunt. Output is plain text, sorted and deterministic for a fixed journal.

- **Session** identifies the session, rulesets, binary stamps and time range. **Runs** groups crash-reboot continuations until shutdown; each new `config.loaded` after shutdown begins another run.
- **Time** separates trial exposure by phase, condition, regime and outcome. Crash downtime is the interval from the last actual trial evidence timestamp to the next boot start, rather than a wall-clock start plus a monotonic duration. Boot start is the first event's timestamp minus `mono_ms`; older journals without that stamp can only approximate it.
- **Failures** separates regime, workload, signal and attribution, with workload exposure in starts and hours, loaded CCDs for unattributed crashes, last-evidence timing bins, reset reasons and recovery gap min/median/max. Recovery gap extends from the last trial evidence to the next boot's first journal event, including startup before togi resumed. Interrupted trials count toward crash timing, downtime and hunt crash totals even when a stronger signal determines their failure classification. A crash's actual instant is not recorded: a zero-duration crash may have happened before the first progress report, not immediately.
- **Hunts** shows the anchor, masks, trials, crashes, elapsed hours, result and commitment backoff. A backoff belongs to a hunt only when its recorded cause cites that hunt's end or joint mark; an unrelated later backoff is not a commitment. Open hunts are reported through the journal's last event.
- **Guard** shows rotations and reached resident steps, their outcomes, and reruns that failed on their first start. Reruns are grouped by their recorded obligation cause: the short starts and final original-duration start belong to one rerun.
- **Evidence quality** counts warnings and checks every executed hunt mask against evidence from before its hunt. A pass must have the same regime, workload, sorted loaded cores and intended duration, at an elementwise equal-or-deeper profile; a later failure of that class at an equal-or-shallower profile invalidates earlier passes. Idle crashes also invalidate all-core R6 passes at equal-or-deeper profiles, regardless of workload or duration. The tables show prior passes, whether they meet the hunt's required count, repeated outcomes and trial exposure cost, with a hunt/stage summary. This is a counterfactual evidence-reuse check, not a claim that skipping those masks would reproduce the same hunt.
  The `decisions resting on a single carried failure` line counts inferred-failure `hunt.mask` events and `tuner.decision` backoffs whose direct causes cite exactly one distinct failure observation, a failed `trial.carried` or idle `failure.carried`. Context causes such as a hunt start do not count as evidence; passing inferences, context-only failure citations, live-only failures and decisions supported by multiple failures do not count. Repeated references to the same sequence count once. The cutoff selects decision event time while retaining earlier carried evidence. This reports dependence on a lone historical failure, not the number of carried failures or a confidence estimate.
- **Failure rate by depth** groups starts and failures for multi-core classes that failed, using the shallowest loaded-core offset from the applied profile. Any loaded core at zero makes the group `0`. Older journals without `trial.intent.profile` use the recorded applied profile. Rates are per start, not probabilities for a different machine or workload.
- **Inconclusive trials** lists trials excluded from pass/failure evidence. **Tctl** gives the distribution of maximum temperatures recorded by passing trials only; crashes do not provide a comparable maximum.

Golden output is generated from a seeded joint-failure simulation. Regenerate it with `go test ./tools/stats -update` in the dev shell.

## Retros

After an unattended run on the target machine, write a retro as a new comment on the pinned [Target-machine runs](https://github.com/shgew/togi/issues/260) issue. Build it from `just stats` and `togi events`, not from memory:

- **Session:** session ID, togi version and ruleset, machine and BIOS context, wall time, trials, crashes and how they recovered.
- **What happened:** the phases in order with their hours, each hunt with masks run and inferred, and the resident profile at the end.
- **What went well** and **what went badly**, each with the numbers that show it.
- **Actions:** one line each, linking the issue or pull request that carries it; file the issue first if none exists.

A finding that holds beyond one run moves to where the next change reads it: how the machine fails goes into its bench machine files under `tools/bench/machines/`, and what simulated runs should show goes into `docs/simulating.md`. The retro links to the pull request that moved it.
