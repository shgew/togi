# Reviewing a run

`togi-tool-stats` (`just stats`) is a read-only development program, not part of the installed togi binary. Run it from a source checkout. It reads `events.jsonl` and archived journals only; it never reads `state.json`, trial samples, other trial output or hardware. Current journals and archives from older shipped schemas are accepted. A live journal's unfinished final line is ignored.

```sh
just stats                                                # target machine: /var/lib/togi/events.jsonl
just stats --state-dir <copied-state-dir>                  # another machine, no hardware or root needed
just stats --journal <state-dir>/archive/<session>.jsonl   # one archived session
just stats --since 2026-09-30T00:00:00Z
```

`--since` filters trials by their start time and failures by event time. Runs overlapping the cutoff retain their full totals. Output is plain text, sorted and deterministic for a fixed journal.

- **Session** identifies the session, rulesets, binary stamps and time range. **Runs** groups crash-reboot continuations until shutdown; each new `config.loaded` after shutdown begins another run.
- **Time** separates trial exposure by phase, condition, regime and outcome. Crash downtime is the interval from the last actual trial evidence timestamp to the next boot start, rather than a wall-clock start plus a monotonic duration. Boot start is the first event's timestamp minus `mono_ms`; older journals without that stamp can only approximate it.
- **Failures** separates regime, workload, signal and attribution, with workload exposure in trials and hours, loaded CCDs for unattributed crashes, last-evidence timing bins, reset reasons and recovery gap min/median/max. Recovery gap extends from the last trial evidence to the next boot's first journal event, including startup before togi resumed. Interrupted trials count toward crash timing and downtime even when a stronger signal determines their failure classification. A crash's actual instant is not recorded: a zero-duration crash may have happened before the first progress report, not immediately.
- **Failure rate by depth** groups trials and failures for multi-core classes that failed, using the shallowest loaded-core offset from the applied profile. Any loaded core at zero makes the group `0`. Older journals without `trial.intent.profile` use the recorded applied profile. A class's trials count every started trial, inconclusive ones included, so the rate is failures per started trial; rates are not probabilities for a different machine or workload.
- **R7 voltage requests** groups started, completed R7 trials by trial class (regime, workload, sorted loaded cores and intended duration), with trials and failures in 4 mV bins of the highest `voltage_requests_v` and by each recorded top requester. Ties and multiple loaded CCDs count a trial in every recorded requester's row, not once across the whole table. A trial's `trial.end` supplies its request fields; a trial whose end lacks them has missing telemetry, with failures counted under `failures without request telemetry`. A `trial.carried` fact with both request fields counts as one more trial of its class, once per source session and sequence and never for a trial of this journal; carried facts without them are left out. Only trials with both request fields count; this journal's missing-telemetry failures are listed separately, and an empty table says `none`. Inconclusive started trials with recorded telemetry count as trials, not failures. These are measured requests, not rail voltages or probabilities for a different class.
- **Inconclusive trials** lists trials excluded from pass/failure evidence. **Tctl** gives the distribution of maximum temperatures recorded by passing trials only; crashes do not provide a comparable maximum.

Golden output is rendered from journals the tests build in memory ([The stats tool](#the-stats-tool-togi-tool-stats)).

## The stats tool (`togi-tool-stats`)

`just stats` runs `togi-tool-stats` ([#820](https://github.com/shgew/togi/issues/820)). It takes the flags `--state-dir`, `--journal` and `--since`, and prints the sections above. Scoring a run against a simulated forecast (`--forecast`) was retired with the forecast by Ruleset 13 ([#1217](https://github.com/shgew/togi/issues/1217)). Per-core policy analyses of historical journals (hunts, checking chains, R7 backoff attribution, counterfactual re-derivation accounting) were retired with the per-core policy by Ruleset 13 ([#1217](https://github.com/shgew/togi/issues/1217)); `togi events` reads those journals' events.

- It reads only `events.jsonl`, never trial samples. A `trial.end` already holds the request telemetry the session summarized from the trial's samples, also for a trial a crash cut short, so a trial whose end lacks the fields has none to recover. The R7 request report counts a trial's end fields and the carried `trial.carried` facts of other sessions.
- A journal that does not follow itself (a gap in the sequence, a cause that does not precede its event) is an error naming the file, with exit status 1; a usage error exits 2.
- Recorded voltage requests must fit a signed 64-bit 4 mV bin; an unrepresentable bin is a journal-input error, never a clipped endpoint.
- **Time** is grouped by the trial's activity (`search`, `checking`, `hunt`, `deepening`, or `-` for an intent without one). Every grouped table follows the order the vocabulary declares: regimes `R1`..`R7`; activities, conditions and outcomes as the journal lists them; and trial classes (failure rate by depth, the R7 request tables) by the workload's place in the catalog, then the loaded cores by number, then the intended duration in seconds, so `60s` lists before `120s`. Nothing sorts alphabetically.
- **Session** adds the event count.
- **Failures** shows `-` for the regime of a failure that names neither a regime nor a trial. **Inconclusive trials** escapes a multi-line reason (`\n`) and shows `-` for a trial that records none.
- `--journal` and `--state-dir` refuse a journal file that is a symlink, because the report reads through the journal crate's no-follow open.

The tests compare rendered output with compile-time golden fixtures in `crates/togi-tool-stats/src/tests/snapshots/`, from journals they build in memory (a combination failure on two CCDs, the review-boundary scenario, R7 requests). Run `cargo nextest run -p togi-tool-stats` to check them; an intentional rendering change updates the fixture and reviews its diff against this document. Tests over a simulated session check the report against the journal's own events and not against bytes, so a tuner change leaves the snapshots alone.

## Retros

After an unattended run on the target machine, write a retro as a new comment on the pinned [Target-machine runs](https://github.com/shgew/togi/issues/260) issue. Build it from `just stats` and `togi events`, not from memory. A question they do not answer, such as how clocks and voltage requests moved with offset, can be asked of the journals and samples with [`just sql`](#querying-journals-and-samples-just-sql); the retro still reports from `just stats` and `togi events`. In omp, `/retro` runs the whole workflow ([`.omp/commands/retro.md`](../.omp/commands/retro.md)).

A full retro has these sections:

- **Session:** session ID, togi version and ruleset, machine and BIOS context, wall time, trials, crashes and how they recovered, and any earlier run that got no retro.
- **What happened:** the nights in order with their hours and tasks, each qualification with its outcome, each mapping edge and cap with its change, the day steps, the daily events with their vetoes, and the day profile at the end of the run.
- **What went well** and **what went badly**, each with the numbers that show it.
- **Actions:** one line each, linking the issue or pull request that carries it; file the issue first if none exists. An action that would change tuner decisions becomes a sub-issue of the open pinned `Ruleset N` issue; any other action becomes an ordinary issue in its block ([`docs/issues.md`](issues.md)). Link both kinds here.

A run with nothing new gets the short form instead: no failure pattern that earlier retros have not recorded, no decision that contradicts the spec, no device trouble. Its retro is the **Session** paragraph, the numbers that changed since the previous retro, and **Actions:** none.

Retros have no dashboard section: the owner files dashboard and UI issues directly.

A finding that holds beyond one run moves to where the next change reads it: how the machine fails goes into its bench machine files under `tools/bench/machines/`, and what simulated runs should show goes into `docs/simulating.md`. The retro links to the pull request that moved it.

## Querying journals and samples (`just sql`)

`just sql [DIR]` opens [DuckDB](https://duckdb.org) on a state directory, `/var/lib/togi` by default, with views over its current and archived journals and every trial's samples, plain or gzip-compressed ([Files](spec/journal.md#files)). It answers aggregate questions across trials and sessions in one query, where `jq` would read every file. DuckDB comes with the development shell only; togi never ships or imports it. The database lives in memory and the views only read, so a live state directory is safe to query; for another machine, copy its whole state directory, `trials/` and `archive/` included. The views list the files when they are read, as DuckDB starts: trials written since are missing, and a query fails with "No files found" once a listed file moves, as a trial's samples do when they are compressed at its end and a session's files do when it is archived. `.read tools/sql/views.sql` lists them again, or restart `just sql`. Where togi has run trials, only root can list `trials/` and the trial directories ([Backends](spec/workloads.md#backends)), so `just sql` refuses such a state directory for any other user rather than show no samples: run it as root, or query a copy you own. The views skip a line that does not parse, such as most torn tails, without saying so, and read a torn tail that happens to parse, which togi drops ([Rules](spec/journal.md#rules)); this is one reason `just stats` and `togi events` stay what a review reports from. Further arguments go to `duckdb`, so `-c` runs a query and exits:

```sh
sudo nix develop --command just sql                       # target machine, interactive, as root
sudo cp -R /var/lib/togi <copy> && sudo chown -R "$(id -u):$(id -g)" <copy>
just sql <copy> -c 'from trials limit 5'                  # a copy you own
just sim --samples --machine tools/bench/machines/target-shared-voltage.json --state-dir <dir>
just sql <dir>                                            # a simulated session with PM-table lanes
```

| View | One row per | Columns |
|---|---|---|
| `events` | event of the current or an archived journal | `session` and `archived`, then the event's `seq`, `time`, `boot`, `mono_ms`, `cause`, `kind`, `msg` and `data` (JSON) |
| `trials` | trial, from its `trial.intent` and `trial.end` | `session`, `archived`, `trial`, `started`, `ended`, `regime`, `workload`, `condition`, `activity`, `cores` (the loaded cores the intent names), `profile` (core → offset), `outcome`, `duration_s`, `tctl_max_c`, and the whole payloads as `intent_data` and `end_data` |
| `samples` | conditions sample | `session`, `archived`, `trial`, then the sample's fields: `core_mhz` and `worker_cpu_ms` keyed by core ID, `tccd_c` by CCD label, and `pm_table` as a struct of 16-element lists |
| `core_samples` | sample and loaded core (one with a clock or a worker in it, filtered by the intent's loaded set) | `session`, `archived`, `trial`, `regime`, `workload`, `core`, its `offset` from the trial's profile, `elapsed_ms`, `mhz`, `worker_cpu_ms`, its own PM-table lanes (`voltage_request_v`, `power_w`, `temperature_c`, `c0_pct`, `cc1_pct`, `cc6_pct`), `tctl_c` and `package_power_w` |

Trial IDs restart in every session, so join on `session` and `trial`, never on `trial` alone. `offset` is an SQL keyword: quote it as `"offset"`. DuckDB lists start at 1, so core `c` of a PM-table lane is `pm_table.voltage_request_v[c + 1]`; `core_samples` has done that already. The views are in `tools/sql/views.sql`; `just sql-test` checks them against fixtures.

Trials and outcomes by regime and workload, across every session of the directory:

```sql
select regime, workload, outcome, count(*) as trials, round(avg(tctl_max_c), 1) as avg_tctl_max_c
from trials
where outcome is not null
group by all
order by all;
```

How each core's voltage request and clock moved with its offset in R7 trials, from the fifth second on as the request summaries of `trial.end` count them ([Event format](spec/journal.md#event-format)):

```sql
select core, "offset", count(*) as samples, round(median(voltage_request_v), 3) as median_v, median(mhz) as median_mhz
from core_samples
where regime = 'R7' and elapsed_ms >= 5000 and voltage_request_v is not null
group by all
order by core, "offset" desc;
```

## Inspecting a transition on recorded state

A copy of the target machine's journals shows what the next real run would archive and import. `togi-tool-sim --state-dir` resumes the copy on the simulator under the current build, so a journal recorded under an older ruleset or schema goes through the transition the next `togi run` would make (`docs/simulating.md`): it archives the source journal unchanged and imports only its raw day records ([Transitions](spec/journal.md#transitions)). `togi events --kind session.imported,day.imported` lists what it imported, each record with the session and sequence it came from. Run it only on a copy: preparation archives the live journal.

From a checkout on the target machine, copy the journals, `state.json` and the archive markers with [`just copy-state`](benchmarking.md#copying-a-real-state-directory) (the sources are read-only), while togi is not running a trial:

```sh
copy=$(mktemp -d)
trap 'rm -rf "$copy"' EXIT
sudo nix develop --command just copy-state /var/lib/togi "$copy"
sudo chown -R "$(id -u):$(id -g)" "$copy"
just sim --state-dir "$copy" --recorded-content
just togi --state-dir "$copy" events --kind session.imported,day.imported
just togi --state-dir "$copy" events --kind stock.attested,trial.intent
```

Without `--recorded-content` the simulator reports synthetic content
identities, so the copy's recorded observations do not match the simulated
session's workloads and the dry run cannot show whether the recorded raw
evidence stays compatible. With it, the simulated trials report the
identities the copy's journal recorded ([`--recorded-content`](simulating.md));
that assumes those executables are what the real machine runs and verifies
nothing about them. The option refuses a copy whose journal records no
identity, or two for one workload.

The first night after the transition records the simulated operator's
[stock-stability attestation](spec/tuner-ruleset14.md#operator-attested-stock-stability)
and begins isolated mapping at −5. Inspect `stock.attested` and `trial.intent`:
there must be no all-zero trial or stock-control queue. Old consent is never
imported into the new session. The simulator supplies an operator answer; it
does not establish that the real machine is stable at stock.

What the transition archives and imports is in [Fact eligibility](spec/journal.md#fact-eligibility). What the simulated session does after the transition draws outcomes from the simulator, not from hardware measurements.

The transition reads no trial samples, so a copy without `trials/` and the archived `<session>-trials/` directories is enough. To query request telemetry from older trial ends with [`just sql`](#querying-journals-and-samples-just-sql), copy the whole state directory instead.

