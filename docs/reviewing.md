# Reviewing a run

`tools/stats` is a read-only development program, not part of the installed togi binary. Run it from a source checkout. It reads only `events.jsonl`, not `state.json`, trial output or hardware; current journals and archives from older shipped schemas are accepted. A live journal's unfinished final line is ignored.

```sh
just stats                                                # target machine: /var/lib/togi/events.jsonl
just stats --state-dir <copied-state-dir>                  # another machine, no hardware or root needed
just stats --journal <state-dir>/archive/<session>.jsonl   # one archived session
just stats --since 2026-09-30T00:00:00Z
```

`--since` filters trials and hunts by their start time, and warnings, failures and cycle events by event time. Runs overlapping the cutoff retain their full totals. Session metadata and earlier evidence remain available, so a cutoff does not erase the passes used to evaluate a hunt. Output is plain text, sorted and deterministic for a fixed journal.

- **Session** identifies the session, rulesets, binary stamps and time range. **Runs** groups crash-reboot continuations until shutdown; each new `config.loaded` after shutdown begins another run.
- **Time** separates trial exposure by phase, condition, regime and outcome. Crash downtime is the interval from the last actual trial evidence timestamp to the next boot start, rather than a wall-clock start plus a monotonic duration. Boot start is the first event's timestamp minus `mono_ms`; older journals without that stamp can only approximate it.
- **Failures** separates regime, workload, signal and attribution, with workload exposure in trials and hours, loaded CCDs for unattributed crashes, last-evidence timing bins, reset reasons and recovery gap min/median/max. Recovery gap extends from the last trial evidence to the next boot's first journal event, including startup before togi resumed. Interrupted trials count toward crash timing, downtime and hunt crash totals even when a stronger signal determines their failure classification. A crash's actual instant is not recorded: a zero-duration crash may have happened before the first progress report, not immediately.
- **Hunts** shows parked offsets, groups, trials, crashes, elapsed hours, result and commitment backoff. A backoff belongs to a hunt only when its recorded cause cites that hunt's end or combination; an unrelated later backoff is not a commitment. Open hunts are reported through the journal's last event.
- **Checking** counts cycles started, cycles ended and full cycles, and reruns that failed on their first trial. **Checking steps and together outcomes** groups reached steps by cycle, regime, loaded cores and outcome. Reruns are grouped by their recorded obligation cause: the short trials and final original-duration trial belong to one rerun.
- **Evidence quality** counts warnings and checks every executed hunt group against evidence from before its hunt. A pass must have the same regime, workload, sorted loaded cores and intended duration, at an elementwise equal-or-deeper profile; a later failure of that class at an equal-or-shallower profile invalidates earlier passes. Idle crashes also invalidate all-core R6 passes at equal-or-deeper profiles, regardless of workload or duration. The tables show prior passes, whether they meet the hunt's required count, repeated outcomes and trial exposure cost, with a hunt/stage summary. This is a counterfactual evidence-reuse check, not a claim that skipping those groups would reproduce the same hunt.
  The `decisions resting on a single carried failure` line counts inferred-failure `hunt.group` events and `tuner.decision` backoffs whose cause chains reach exactly one distinct failure observation, a failed `trial.carried` or idle `failure.carried`. It follows causes transitively through `hunt.start`, `hunt.end`, `combination` and other context events. Observations (`trial.carried`, `failure.carried`, `trial.end` and `failure`) end a branch: passing or inconclusive observations contribute no failure and their causes are not followed. Context events are not themselves failure evidence. Passing inferences, non-backoff decisions, chains without failures, and chains containing multiple failures or any live failure do not count. Repeated paths to the same failure sequence count once. The cutoff selects decision event time while retaining earlier evidence and context. This reports dependence on a lone historical failure, not the number of carried failures or a confidence estimate.
- **Failure rate by depth** groups trials and failures for multi-core classes that failed, using the shallowest loaded-core offset from the applied profile. Any loaded core at zero makes the group `0`. Older journals without `trial.intent.profile` use the recorded applied profile. Rates are per trial, not probabilities for a different machine or workload.
- **R7 voltage requests** groups started, completed R7 trials by trial class (regime, workload, sorted loaded cores and intended duration), with trials and failures in 4 mV bins of the highest `voltage_requests_v` and by each recorded top requester. Ties and multiple loaded CCDs count a trial in every recorded requester's row, not once across the whole table. Only trials with both request fields count; missing-telemetry failures are listed separately, and an empty table says `none`. Inconclusive started trials with telemetry count as trials, not failures. These are measured requests, not rail voltages or probabilities for a different class.
- **Inconclusive trials** lists trials excluded from pass/failure evidence. **Tctl** gives the distribution of maximum temperatures recorded by passing trials only; crashes do not provide a comparable maximum.

Golden output is rendered from the committed `tools/stats/testdata/combination.jsonl.gz` fixture, originally generated by a seeded combination-failure simulation, and a small synthetic telemetry journal in `TestR7RequestReport`. `go test ./tools/stats -update` in the dev shell re-renders `combination.golden` and `requests.golden`; it does not re-run the simulation or regenerate the committed journal.

## Retros

After an unattended run on the target machine, write a retro as a new comment on the pinned [Target-machine runs](https://github.com/shgew/togi/issues/260) issue. Build it from `just stats` and `togi events`, not from memory:

- **Session:** session ID, togi version and ruleset, machine and BIOS context, wall time, trials, crashes and how they recovered.
- **What happened:** the phases in order with their hours, each hunt with groups run and inferred, and the profile at the end.
- **What went well** and **what went badly**, each with the numbers that show it.
- **Actions:** one line each, linking the issue or pull request that carries it; file the issue first if none exists.

A finding that holds beyond one run moves to where the next change reads it: how the machine fails goes into its bench machine files under `tools/bench/machines/`, and what simulated runs should show goes into `docs/simulating.md`. The retro links to the pull request that moved it.

## Inspecting a transition on recorded state

`tools/carry-facts` is a development-only transition inspector and simulator, never a hardware runner. Its required `--state-dir` must name a copy beneath the system temporary directory; it rejects the real state path and symlinked archive, live journal or lock paths. Without `--simulate`, it locks the copy through the existing journal API, reads the live journal's recorded BIOS context with `internal/facts`, and prepares carry at the current evidence epoch with the current schema and ruleset + 1 to force a transition, printing carried pass/failure counts by original source session. With `--simulate`, it resumes the copy on the simulator under the current build using that recorded BIOS context; `--seed` selects simulated outcomes (default 1). It runs until the first passed full cycle and reports carried counts, candidate-solo-limit answers, live solo limit-check starts and live cycle work. Both modes mutate only the copy. No `cmd/togi` flag is added.

From a checkout on the target machine, copy only the journals (the sources are read-only):

```sh
copy=$(mktemp -d)
trap 'rm -rf "$copy"' EXIT
mkdir "$copy/archive"
sudo sh -c 'cp /var/lib/togi/archive/*.jsonl "$1/archive/"; for marker in /var/lib/togi/archive/*-reset-all; do [ ! -f "$marker" ] || cp "$marker" "$1/archive/"; done; cp /var/lib/togi/events.jsonl "$1/events.jsonl"' sh "$copy"
sudo chown -R "$(id -u):$(id -g)" "$copy"
go run ./tools/carry-facts --state-dir "$copy"
```

For the target history recorded through session `20261002T004254Z`, expect passes only from that latest session (epoch 1). Eligible failures can come from that session and earlier same-BIOS sessions after `reset --all` in `20260926T151414Z`; facts in that reset session must follow its reset, and no earlier session contributes. Older epochs contribute failures but zero passes. Counts reflect any core-reset and defect exclusions. Numeric counts must come from running the command, not from this recipe. Simulation reuses the recorded BIOS context but draws outcomes from the simulator, not from hardware measurements. Use a fresh copy for each mode because preparation archives its live journal.

To inspect request telemetry from older trial ends, also copy their `archive/<session>-trials/` directories and the live `trials/` directory, or copy the whole state directory. Journal-only copies retain fields already recorded in trial ends but cannot recover measurements from absent samples.

