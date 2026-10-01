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
- **Time** separates trial exposure by phase, condition, regime and outcome. Crash downtime is next boot start minus trial start minus the last evidence duration. Boot start is the first event's timestamp minus `mono_ms`; older journals without that stamp can only approximate it.
- **Failures** separates regime, workload, signal and attribution, with workload exposure in starts and hours, loaded CCDs for unattributed crashes, last-evidence timing bins, reset reasons and recovery gap min/median/max. Recovery gap extends from the last trial evidence to the next boot's first journal event, including startup before togi resumed. A crash's actual instant is not recorded: a zero-duration crash may have happened before the first progress report, not immediately.
- **Hunts** shows the anchor, masks, trials, crashes, elapsed hours, result and following backoff. Open hunts are reported through the journal's last event.
- **Guard** shows rotations and reached resident steps, their outcomes, and reruns that failed on their first start.
- **Evidence quality** counts warnings and checks every executed hunt mask against evidence from before its hunt. A pass must have the same regime, workload, sorted loaded cores and intended duration, at an elementwise equal-or-deeper profile; a later failure of that class at an equal-or-shallower profile invalidates earlier passes. The tables show prior passes, whether they meet the hunt's required count, repeated outcomes and trial exposure cost, with a hunt/stage summary. This is a counterfactual evidence-reuse check, not a claim that skipping those masks would reproduce the same hunt.
- **Failure rate by depth** groups starts and failures for multi-core classes that failed, using the shallowest loaded-core offset from the applied profile. Any loaded core at zero makes the group `0`. Older journals without `trial.intent.profile` use the recorded applied profile. Rates are per start, not probabilities for a different machine or workload.
- **Inconclusive trials** lists trials excluded from pass/failure evidence. **Tctl** gives the distribution of maximum temperatures recorded by passing trials only; crashes do not provide a comparable maximum.

Golden output is generated from a seeded joint-failure simulation. Regenerate it with `go test ./tools/stats -update` in the dev shell.
