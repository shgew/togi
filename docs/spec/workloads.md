# Workloads

Normative rules for what a trial runs, how it is contained, and how its outcome is judged. The research behind the regime choice is summarised in `../prior-art.md`.

## Evidence compatibility

The evidence epoch (`tuner.EvidenceEpoch`; `tuner.md`, Ruleset) versions whether recorded passes remain comparable with current trials. Changes to workload content, the backend configuration togi generates, intended trial durations, or pass/failure detection must bump it when they change the evidence contract. An epoch bump drops older carried passes while retaining eligible failures; it does not change the journal schema or the strategy ruleset. A new backend build needs no bump: passes are keyed by the backend's package store path (`tuner.md`, Evidence).

## Backends

| Backend | nixpkgs attribute | Result check |
|---|---|---|
| mprime | `mprime` (unfree) | Torture test residue and rounding checks, reported in `results.txt` |
| y-cruncher | `y-cruncher` (unfree) | Built-in verification in stress mode, reported on stdout |

Every workload must check its own results: silent computation errors are a known failure mode ([ADR 0008](../adr/0008-self-checking-workloads.md)).

A backend integration:
- writes its configuration files into the trial's work directory with the thread count, and with the exact logical CPUs where the program takes them: y-cruncher sets its own affinity from its config and would otherwise start workers on every CPU, while mprime names no CPUs (`EnableSetAffinity=0`) and the trial's scope confines it;
- produces the argument vector to launch;
- parses output as it arrives into progress, computation error and fatal setup error;
- reports setup problems as inconclusive, never as failures.

Togi creates a root-owned `trials/<trial>/` container. Each backend instance has its own writable directory: `work/` for a single instance, or `cNN/` for an all-core trial. Before launching, togi gives the configured backend account ownership of that directory and every generated input file, so mprime can update `prime.txt` and `local.txt`, create `results.txt`, and y-cruncher can access `stress.cfg`. The surrounding trial and state directories, retention markers, journal and control files remain root-owned and nonwritable by that account. Backend binaries remain read-only in the Nix store; no package file changes ownership. This is a privilege drop, not a filesystem sandbox: ordinary system permissions still apply outside togi's state.

The runner recreates a reused trial container before preparing inputs, without following old instance or file symlinks. It exclusively creates and retains both log descriptors before transferring instance ownership, so another workload sharing the backend UID cannot redirect privileged log opens. Relative trial-directory paths are resolved to absolute paths before preparation and passed unchanged to both the launcher and the scope's working-directory option.

The state directory, trials root and trial containers have mode `0711` independent of the process umask; instance directories have mode `0755` before ownership transfer. Other ancestors are never chmodded: before launching, the runner checks each resolved ancestor's search permission for the backend UID and primary GID and refuses an inaccessible path by name. A directory that only an ACL makes traversable must also grant the applicable mode-bit search permission. Watched outputs are opened without following symlinks and without blocking on special files, and only regular files are read; a rejection makes the trial inconclusive unless stronger failure or containment evidence survives.

Known quirks the integrations handle:
- **mprime:** needs `NumCPUs`, `CoresPerTest`, the `CpuSupports*` switches for the instruction set and the FFT range in `local.txt`/`prime.txt`. `TortureTime=1` keeps each FFT size to one minute, so a trial covers several sizes. Errors land in `results.txt`, which the runner tails once per second, and mprime can keep running after one.
- **y-cruncher:** command-line thread options do not confine it; only a config file names the CPUs. It prints `Failed to set core affinity` when confinement and config disagree, which counts as a containment violation. Its startup is slow and needs the stall grace period.

Backend output lines on stdout, stderr and watched files may contain at most 64 KiB before a newline or carriage return; an unterminated line has the same byte limit. Exactly 64 KiB is permitted. A longer line is a runner error and uses the shared bounded teardown: the trial is inconclusive unless a containment violation or backend failure signal collected before or during teardown takes precedence. Other output I/O errors and unconfirmed cleanup remain runner errors regardless of captured backend evidence. Reading and retaining output is bounded even for an unfinished line or a large watched-file append. The runner keeps the first 64 KiB of the offending line for diagnosis (in the stream log or watched-file buffer), stops reading that stream or file, and never classifies its truncated prefix as a complete line. It does not silently truncate output and continue the trial.

Each watched-file polling read processes at most 64 KiB, including short complete lines, in chunks of at most 4096 bytes. Unread bytes and partial lines remain at their exact incremental offsets for later batches; exhausting a batch is not EOF or successful draining. Polling checks cancellation and deadlines between reads and between lines. During a trial, a polling round yields by the next scheduled load step or sample interval, or the trial deadline, whichever comes first; this scheduling budget expiring leaves output pending, not an I/O error. The next round resumes with the next watched file and instance so a flooded file cannot monopolize polling. Only output classification yields to the polling budget: each sample tick still checks containment and CPU progress for every ready instance until an outcome or trial cancellation ends supervision. Supervision returns to process events and load-step scheduling between polling rounds.

## Regimes

"One thread" means the core's first logical CPU. Its SMT sibling stays idle.

| Regime | Purpose | Content |
|---|---|---|
| R1 light | Highest single-core boost with light instructions | 1 thread. mprime SSE FFT 4K-21K; y-cruncher BKT + SFTv4; y-cruncher SNT + SVT, using the lowest-ISA binary the package ships |
| R2 vector heavy | Vector units, higher current | 1 thread. mprime AVX2 FFT 36K-248K; mprime AVX-512 FFT 36K-248K; y-cruncher FFTv4 + N63 + VT3, using the Zen 5 binary |
| R3 load steps | Current transients between busy and idle | An R1 or R2 workload suspended and resumed with `SIGSTOP`/`SIGCONT`. On and off periods are drawn from {10 ms, 50 ms, 200 ms, 1 s, 5 s} with a recorded seed |
| R4 medium load | Partial duty cycles | An R1 workload at 25%, 50% or 75% duty with a 100 ms period, cycling per trial |
| R5 SMT pair | Both threads of one core | R1 and R2 workloads with 2 threads on both logical CPUs of the core |
| R6 idle | Normal power management with the profile applied | One confined 1-thread R1 instance per core, each stopped with SIGSTOP as soon as it enters its scope, before the next instance launches. Trial timing begins only after all instances are stopped. First half: no load at all. Second half: short bursts (SIGCONT, then SIGSTOP 100 ms later, every 2 s) on one core at a time in scheduling order |
| R7 all-core | Self-sufficiency, package power, thermals, cross-CCD interaction | One confined 1-thread R2 instance on each loaded core. For each CCD a checking step runs its full part then partial parts that idle successive top-request groups, ties within 1 mV together, while at least two cores remain loaded; the all-core part runs last. On one CCD the all-core part is its full part, run once before the chain |

Within a regime, search cycles listed workloads; candidate-solo-limit checks freeze one R1 and one R2 workload. Checking's repeated steps cycle their regime's catalog from the start of each cycle: three R1, R2 or R7 occurrences cover each workload, and three R3 or R4 occurrences run the first three entries of their longer catalogs. All parts of one R7 step share its selected R2 workload. Inconclusive trials retry the same class. Every R7 part requires passing trials; every failed multi-core R7 trial follows the request-based attribution and [voltage-targeted backoff rules](tuner.md#r7-voltage-targeted-backoff).

R6 runs on the profile and in parked hunt trials; R7 runs on the profile, checks deepening rounds and runs located hunts: a live unattributed multi-core R7 failure with an unloaded core off CO 0 is first located among the unloaded cores. R6 targets every core; an R7 trial can target part of a CCD, a full CCD or every core. `trial.intent.cores` lists exactly the loaded cores; the applied profile also includes idle cores. Each loaded core runs one instance on its first logical CPU, in its own scope and work directory, so a backend signal names that instance's core. Partial-chain derivation and its stopping reason are recorded in `checking.chain`; no partial with fewer than two loaded cores is launched.

### Load-step schedules

An R3 or R4 workload starts running, then alternates on and off periods; each on period ends with SIGSTOP and each off period with SIGCONT. R3 draws each period independently and uniformly from {10 ms, 50 ms, 200 ms, 1 s, 5 s} with a PCG seeded by the recorded seed, so the seed reproduces the schedule. R4 runs for its duty share of every 100 ms period. The trial end records how many of each signal were sent.

## Durations and checking schedule

Defaults, all configurable:

| Use | Duration |
|---|---|
| Search trial (R1, then R2) | 90 s each; a candidate-solo-limit class needs `n` passing trials |
| Short trial for R7, hunt groups, deepening checks and backoff reruns | 120 s (`durations.short_trial_s`) |
| Checking per-core trial (R1 to R5) | 2 min each |
| Checking R6 | 15 min |
| Checking R7 long trials | Full parts total 20 min on two CCDs: 5 min each CCD, 10 min all cores; each derived partial adds 5 min. On one CCD the full part runs 20 min and each derived partial adds 20 min |

For `D = durations.checking_all_core_s` and `n` CCDs, a full R7 long trial runs for `D` on one CCD; on multiple CCDs, each full single-CCD part runs `floor(D / 4)` seconds and the all-core part runs `D - n*floor(D / 4)` seconds. A partial receives its CCD's full-part long duration in addition to these allocations. Every part runs three short `short_trial_s` trials and one long trial. When short and long durations match, their counts add: four trials. All parts require passes and supply full-cycle coverage. `checking_all_core_s` must be in [4, 86400]. A trial is torn down before the next trial. Inconclusive trials repeat their part's frozen loaded cores and workload; partial sets are derived after the predecessor passes, using request telemetry as specified in [tuner.md](tuner.md#r7-request-order-and-attribution).

Hunt-group duration selection and its carried-evidence rules are defined in [tuner.md, Hunt](tuner.md#hunt).

Default checking cycle, with elapsed time depending on the request ties and resulting partial chains:
1. R7, R7, R7: every R2 workload, each on CCD0 full and its partial chain, CCD1 full and its partial chain, then all cores, with three short trials and one long trial per part.
2. R2, R2, R2: every R2 workload on every core.
3. R6.
4. R5 on every core.
5. R1, R1, R1: every R1 workload on every core.
6. R3, R3, R3: every R1 workload with load steps on every core.
7. R4, R4, R4: every R1 workload on every core, at 25%, 50% and 75% duty respectively.
8. R6.

Full-cycle coverage, earlier-cycle credit, carried-evidence boundaries and per-core scheduling order are defined in [tuner.md, Checking](tuner.md#checking). Search time depends on solo limit distance and failed steps; candidate checks require five passes of each frozen R1 and R2 class by default, including eligible carried passes.

## Containment

Each backend instance runs as a child of togi inside a transient scope confined to its logical CPUs:

```
systemd-run --scope --quiet --collect --uid=<uid> --gid=<gid> --working-directory=<instance-dir> -p AllowedCPUs=<cpus> -p DefaultDependencies=no -- <argv>
```

`DefaultDependencies=no` keeps a system shutdown from stopping the scope on its own. Otherwise systemd stops the scope and the unit running togi at the same moment, the backend can exit before togi sees its signal, and the trial would end as an unexpected exit on the target instead of interrupted. Without the default dependencies, togi's teardown ends the scope after the signal reaches it.

The root launcher clears all supplementary groups before starting `systemd-run`, retaining UID 0 for system-bus access and scope creation. The scope launch then drops to the configured backend UID and primary GID. Its working directory is explicit: with `--uid` alone, `systemd-run --scope` defaults to the account's home instead of the prepared instance directory.

If togi dies before teardown, the scope can outlive it and keep its descendants running. The next fresh start or resume sweeps concrete `togi-trial-*.scope` units after nonwriting preflight and before any SMU profile write, whether in a tuning boot or an in-session run. The sweep sends SIGCONT and SIGTERM through bounded `systemctl kill --kill-whom=all` calls on each exact scope during the shared grace period, then uses normal teardown's bounded scope-kill slot to send SIGKILL and stop the scopes. A recovered PID and its start time are used only to verify exit, never to signal a PID or process group that may now belong to somebody else. Cleanup verifies that no matching unit or process remains and that every captured process exited, including one that moved out of its scope. A moved process that survives scope signaling is a containment dead end, not a reason to signal its recovered PID. Discovery and verification consume the same absolute 15 s budget as teardown, not a fresh budget per scope.

The safe real-process integration test kills a helper owner and sweeps its detached descendant, with systemd unit discovery and operations replaced by a deterministic seam. It proves real process cleanup without touching host scopes; deterministic fake-time tests prove unit filtering and deadlines. This does not prove systemd transient-scope collection on a real tuning boot; that requires the VM trial infrastructure.

Processes that disappear while `/proc` is scanned are no longer leftover workloads; missing-process errors do not fail the sweep, but permission and other read failures still do.

The kernel enforces the cpuset whatever the backend does. togi also samples the processor field of every backend thread in `/proc/<pid>/task/*/stat` once per second, from the moment the process is inside its scope. A thread seen outside its allowed logical CPUs is a dead end. Stopping a trial terminates the whole scope.

Every running workload exposes `Started`, `Wait` and `Stop`. `Stop` cancels the workload and returns once its teardown has joined; repeated calls return the first result. The run owner calls it before any cleanup profile write and preserves containment errors.

Normal trial ends and partial-start rollbacks use the same teardown, with one absolute deadline shared across all instances:
1. Send SIGCONT, then SIGTERM to each identity-verified process group owned by the current run, and allow up to 3 s of grace shared by all groups. Owned groups are verified against the captured launcher PID and start time before each signal; reaping and signaling are serialized so the PID cannot be reused between verification and signaling.
2. Send SIGKILL to every known scope with `systemctl kill --kill-whom=all`, including scopes whose launcher has exited and scopes attempted before a launch failed. The calls share a slot of at most 2 s and each gets only the remaining time.
3. Send SIGKILL to every still identity-verified owned process group. An exited or reused launcher PID receives no process-group signal; its scope remains the cleanup target.
4. Drain output for up to 10 s, without passing the absolute deadline. Every watched file on every instance shares this deadline: drain them in bounded polling rounds that check the remaining time between files, reads and lines, even after the launchers exit. Close the output readers at the deadline and join their goroutines, releasing the log files even when a detached descendant holds the pipes open. No unbounded final watched-file drain follows the deadline.

Teardown takes at most 15 s regardless of instance count. Cleanup succeeds only when every launched process has exited, output has drained, and every scope kill succeeded or confirmed that its scope no longer exists. A failed or timed-out scope kill, an unconfirmed process-group kill, or an undrained backend is a containment dead end: no next trial or tuning profile write follows. Offset restoration belongs to run-level cleanup, not partial-start rollback or trial outcome adjudication, and runs only after confirmed workload teardown; unconfirmed cleanup prevents restoration and a clean `shutdown` even if the journal also fails. Final output from the pipes and watched files is classified before the outcome, including unterminated computation errors printed during teardown.

An unread watched-file backlog at the cleanup deadline is unconfirmed output draining, not a successful EOF, and prevents a pass, another trial or a tuning profile write. Final unterminated watched-file lines are classified exactly once only after their bytes have been fully read; a batch boundary is not a line boundary. Backend errors beyond the first read batch remain evidence when drained within the shared deadline. The explicit oversized-line rejection above still retains its diagnostic prefix without classifying it as a complete line and keeps the same evidence precedence.

Rejecting a watched symlink or nonregular file without a known unread regular-file backlog intentionally stops that reader and does not by itself make draining unconfirmed; confirmed process and scope teardown leaves the trial inconclusive and retryable. If a previously accepted regular file still had unread bytes, replacing it with a symlink or nonregular file loses accepted output evidence and remains unconfirmed draining and a containment dead end.

A non-EOF backend-pipe read failure cannot confirm output draining and is a containment failure, even if the process exit and scope kill otherwise succeed.

Every systemd CLI invocation has a context deadline. The preflight `systemd-run` probe and `systemctl reboot` are bounded by 30 s; a trial's `systemd-run` process is bounded by its trial duration plus the startup and teardown allowances. Teardown `systemctl` calls share the bounded slot within the absolute 15 s cleanup deadline, so each kill has only the remaining time. A timed-out kill is failed cleanup even if its output says the scope is missing. Kernel-log reads with `journalctl` have a 30 s deadline; a timeout is a read failure, not an empty log, and follows the existing inconclusive handling.

## Trial outcome

A trial passes when all of these hold:
- it ran for its full duration;
- the backend reported no computation error;
- the backend kept making progress;
- no failure signal named a core;
- every sampled thread was on an allowed logical CPU.

Both successful waits and runner errors use the same evidence precedence, including recovery after a crash:

| Evidence (first applicable row wins) | Result |
|---|---|
| Containment escape | Dead end |
| Backend computation error, stall or early exit | Failure, even with cleanup errors, cancellation or a crash |
| MCE inside the trial's window | Failure |
| Reset reason | Applies only without higher-precedence evidence |
| Required sampling, current-boot kernel log or setup observation missing | Inconclusive |
| Full duration, every required observation valid and no failure | Pass |

Runner errors remain diagnostics in the trial's reason; they never replace a classified failure. A failed kernel-log boundary read or an unresumable cursor leaves its interval unobserved, making an intersecting trial inconclusive unless higher-precedence evidence survives. Later successful reads do not erase that trial's lost observation. MCEs returned with a read error remain evidence.

An explicit `No journal boot entry found` result from `journalctl` is a missing observation, not zero MCEs: the current-boot read returns a missing-boot error and the trial is inconclusive unless higher-precedence evidence decides it. Exit status 1 with empty stdout and stderr is a valid no-match result and remains clean. During crash recovery, a missing older boot still supplies no MCEs without a retry; missing current-boot recovery observations keep the existing retry policy.

The kernel log is read from a cursor at each trial's intent boundary and at teardown, also on process startup and clean shutdown. Consecutive reads cover profile writes, backend retry waits and the time between trials without a streaming follower. Each trial's evidence window starts once its profile has been applied and read back and ends at the teardown read, including the target's reset to 0. Both boundaries are inclusive. An MCE is a trial cause only when its boot-local timestamp falls inside that window; MCEs before profile readiness, after teardown or beyond the current read's monotonic time are recorded and shown by `status`, `events` and `watch`, but do not feed trial decisions.

The opaque cursor is persisted on existing boundary events, scoped to their boot ID. Same-boot resume validates and continues the last recorded cursor; crash recovery reads the previous boot's persistent log, including its unread tail, while the new boot starts its own cursor. No cursor crosses boots. An unavailable cursor is recorded as observation loss before re-anchoring on the retained log; re-anchoring permits future covered trials, not a retroactive pass for a trial with a lost observation. A journal from before cursor recording is read from the beginning of the boot, with existing MCE records deduplicated.

Required samples are the thread processor fields used to check containment and the process CPU-time fields used to check progress. A failed task-directory read, an unreadable thread or process stat, or malformed required data is a lost observation. Even a single lost sample makes the trial inconclusive unless higher-precedence evidence decides it; successful samples later in the trial do not repair the loss. Readable threads still provide containment evidence when another thread cannot be read. A process or thread disappearing before its stat can be read is normal, not sampling loss: an exited thread is skipped, and the process-exit observation decides whether a backend exited early. Temperature is optional and does not affect sampling validity.

A scoped instance that never entered its scope by the trial deadline is a setup error and makes the trial inconclusive unless higher-precedence evidence was found. A process exit queued before teardown begins is an unexpected exit; exits caused by teardown are not.

Failure signals:

| Signal | Detection | Attribution |
|---|---|---|
| Computation error | Backend output | The instance's core |
| Unexpected exit | Process exits before the trial ends, without a setup error | The instance's core |
| Stall | Over a 10 s window of unsuspended time, backend CPU time advances less than half of thread count times that window, after a 30 s startup grace | The instance's core |
| Corrected MCE | Kernel log, read from a cursor at every trial boundary | The core of the reporting logical CPU if the bank is core-local, else unattributed |
| Uncorrected MCE | Kernel log of the next boot, or of the crashed boot in the persistent system journal | Same rule |
| Crash | Boot ID differs from the last journal boot with no clean shutdown; reset reason narrows its classification | Unattributed unless a backend signal or MCE names a core; outside multi-core R7, one nonzero applied offset can also name it |

In trials alone every failure belongs to the target. Outside multi-core R7, together and parked trials use the backend instance's signal, then exactly one core-local MCE, then the sole nonzero offset in the applied profile. Evidence naming more than one core without a higher-precedence backend signal remains unattributed. Multi-core R7 counts named backend or exactly-one-core MCE failures against the named core; otherwise it uses top requesters, never the sole nonzero-offset inference ([tuner.md](tuner.md#r7-request-order-and-attribution)).

Crash detection and evidence: every boot in the journal other than the current one, whose last event is not `shutdown` and that no `crash.detected` names, has crashed. Its evidence is the MCEs in its own kernel log that are not between trials, plus the uncorrected MCEs logged by the boot after it (the next boot in the journal, or the current boot), each recorded once as an `mce` event with `from_boot`. Recovery also records the previous boot's between-trial MCEs with `between_trials: true`, without using them as crash evidence or decision causes. An MCE that an earlier `crash.detected` already cites is evidence of that crash only: a boot's own log starts with the MCEs its predecessor's crash left in the banks. Recovery is idempotent: detection, closing the crashed trial and its failure are each redone by the next `run` if a further crash or kill interrupts them, so a crash during recovery is detected on its own, normally as stray, without losing the first.

Recovery compares an old boot's MCE `monotonic_ns` with the profile-applied boundary of the open trial in that same boot. A crashed trial has no teardown timestamp: its unread kernel-log tail remains eligible after the start boundary, rather than being cut off at the last progress event. Uncorrected MCA evidence retained into the following boot is eligible for the preceding crash without comparing its new-boot clock to the old trial's clock. Corrected next-boot MCEs are not retained crash evidence. Events lacking `monotonic_ns` preserve the earlier replay behavior; an explicit zero is a timestamp, not a missing field.

Kernel reset reasons are read from the immediate system boot after the crashed boot, even when togi did not run in that boot. `journalctl --list-boots` supplies a candidate, not proof of succession: recovery requires the crashed boot's last journal entry and the candidate's first entry to have the same `__SEQNUM_ID` and consecutive `__SEQNUM` values. These boundaries read all journal entries, not only kernel messages, because journald assigns sequence numbers across its split files. A gap, a changed sequence identity, missing boundary entries or missing sequence metadata (including systemd before 254) leaves the reason unknown. If the crashed boot or its successor cannot be identified, or the successor's log has been vacuumed, recovery records no reason and keeps ordinary crash handling; it never substitutes a later boot's reason. Watchdog expiry, sync flood and CPU shutdown are crash failures. Power-button reset during a trial is a crash failure; outside a trial it is inconclusive. Thermal trip is a dead end unless earlier backend or MCE evidence established a failure. No reason line on a kernel supporting this log (6.16+) is treated as power loss, and is inconclusive only after reason reporting has been confirmed in another boot. A boot whose kernel log holds neither its `Linux version` line nor a reason line, because its start was rotated out, reads as unsupported. A boot the system journal no longer holds confirms nothing and is skipped without a retry; other kernel-log read errors keep the recovery retry policy. Unknown or unsupported reasons keep ordinary crash handling. A recorded backend signal or relevant MCE always takes precedence over reset reason.

Crash recovery also looks for the previous boot's saved kernel messages under `/var/lib/systemd/pstore/`, after `systemd-pstore.service` has archived `/sys/fs/pstore/`. The supported EFI layout is `<Unix timestamp>/<three-digit dump count suffix>/dmesg.txt`, reconstructed by systemd from `dmesg-efi-` or `dmesg-efi_pstore-` chunks. The dump timestamp must fall at or after that boot's first system-journal entry and before the next listed system boot's first entry, including boots where togi did not run. Missing or non-increasing boot timestamps leave the diagnostic absent. Among matching records, the greatest full boot-local dump count wins, then the lowest chunk part from each record's first kernel dump header: the directory count suffix wraps after 999, and EFI timestamps each chunk separately, so a dump crossing a second boundary can be split across archive directories. The group containing the lowest part retains the newest kernel messages. Malformed or incomplete headers warn and leave the diagnostic absent. Other layouts are not guessed from filesystem modification times, which date archival rather than the dump. This is a best-effort wall-clock match, not a boot ID embedded in pstore; clock changes or vacuumed intervening boots can make the interval unavailable or ambiguous. Unlike reset reasons, this optional diagnostic does not require consecutive journal sequence numbers: it can survive the loss of the crashed boot's journal tail. A matching record adds the optional bounded `crash.detected.pstore` diagnostic (`journal.md`); missing records, including hard freezes that never dumped messages, leave it absent. Read errors warn and do not retry or stop recovery, since this is optional diagnostic text, not stability evidence. Recovery neither removes nor modifies the archive.

A trial still open when `run` starts, in a boot that did not crash (the same boot, or one that ended in `shutdown`), ends as `interrupted`: a failure (`corrected_mce`, or `uncorrected_mce` when none is corrected) when `mce` events were already recorded for it, else inconclusive. Interrupted trials do not count toward the inconclusive dead end.

A trial closed this way, after a crash or an interruption, records as its `duration_s` the time from its `trial.start` to its last `trial.progress`, `trial.signal` or `trial.sample` in the journal, or 0 without a `trial.start`. The separate conditions samples do not change this lower bound or supply stability evidence. After a crash, recovery also records the last complete conditions sample's elapsed seconds, Tctl and available loaded-core frequency range in optional `trial.end` fields (`journal.md`).

For failed trials with at least two loaded cores, persisted worker CPU-time samples may name the worker that stopped advancing first. R6 omits this evidence because its workers are suspended by design: unchanged CPU time can be scheduled suspension, including a worker that has not yet carried load. A stall is a terminal run of unchanged cumulative CPU time, dated at its first unchanged sample; a later advance cancels that stall. The earliest terminal stall must be unique. No samples, fewer than two samples, any missing loaded-worker reading, decreasing counters or non-increasing sample timestamps, and tied earliest stalls leave the evidence absent. A worker that advanced through the final sample has no observed stall. `stalled_core` does not attribute a failure to that core; for an unattributed multi-core R7 failure it restricts affected top requesters to its CCD. Other regimes retain diagnostic-only use. Crash recovery derives it from completed persisted samples on the next boot; an ordinary failure derives it after the samples writer finishes.

The `trial.end` message for a trial closed on resume says `last evidence Ns after start` instead of presenting `duration_s` as elapsed time. A trial that ends while togi watches it, including an orderly stop by signal, reports its measured duration with `after Ns`.

MCE attribution rules:
- Attribution uses the SMCA bank type decoded by the kernel (`edac_mce_amd`), never a hard-coded bank number.
- A decoded bank continuation is assigned only when exactly one MCE could own it. If multiple status records arrive before their bank lines, those records keep an unknown bank type and are unattributed; journal message order alone cannot identify the owner.
- A new error-description boundary closes orphaned continuation state, so a truncated record does not prevent attribution of the next independent record. A surplus bank continuation establishes interleaving and clears tentative attribution back to the earliest unresolved status; this can also clear independent records whose ownership the log cannot distinguish.
- Core-local types (load-store, instruction fetch, L2, decode, execution, floating point) name a core.
- Shared types (L3, memory controller, data fabric and others) name none.
- In together trials outside multi-core R7, a core-local MCE on a core whose offset is 0 is an attributed failure at 0, which is a dead end. In multi-core R7 it dead-ends only if that core was in its CCD's top group in that trial; otherwise the failure routes to that CCD's top group, or to the loaded CCD's when that core's CCD had no loaded core, with the CO-0 stepdown (`tuner.md`).

MCA bank contents survive a warm reset and the kernel logs them early in the next boot. The tuning boot keeps the system journal persistent, so the crashed boot's last kernel messages stay readable.

## Conditions sampling

At each sampling tick (once per second by default), the runner hands one conditions sample to a writer without waiting for storage. The writer creates the root-owned `trials/<trial>/samples.jsonl` and syncs its containing directories, then appends each accepted sample in order as one JSON line and fsyncs it before writing the next. One sample may wait while file setup or a write is in progress; if that slot is full, the new sample is dropped instead of delaying load-step transitions or the trial deadline. File setup also does not delay scheduled load steps, cancellation or teardown. A crash can lose the unfinished write and queued sample; completed writes remain durable. Trial teardown does not wait for storage, but the runner waits for the writer and file close before returning, draining accepted samples unless setup or persistence fails. Samples use milliseconds since trial timing began, including idle and suspended periods; they are diagnostic, not observations that decide an outcome. Missing or unreadable sensors are omitted, never fatal. A failure to create, write, sync or close the samples file is a runner error, with the existing teardown and outcome precedence.

Tctl and Tccd temperatures come from the first hwmon whose `name` is `k10temp` or `zenpower`, using the `temp*_input` with labels `Tctl` or `Tccd1`, `Tccd2`, and so on. Temperatures are whole degrees Celsius. The maximum Tctl still goes into normally ended trials' end events.

For each loaded core, the runner reads its first logical CPU's `cpufreq/scaling_cur_freq` under `/sys/devices/system/cpu`, converts kHz to whole MHz, and records it by core ID. This includes loaded cores that are temporarily suspended; idle cores outside the trial's target set are not sampled.

The same tick records cumulative backend worker CPU time in whole milliseconds by loaded core ID (`worker_cpu_ms`), reusing the confined instance's process CPU-time reading used for progress checks. It includes user and system CPU time and does not advance while suspended. A reading that was not obtained (including an instance not yet ready or a tick stopped early by stronger evidence) is absent, never fabricated as zero. Non-Linux runners retain the existing unsupported process-observation fallback; this telemetry adds no new required observation or failure rule.

On Linux, the same tick takes the latest completed reading of the driver's read-only `/sys/kernel/ryzen_smu_drv/pm_table_version` and `pm_table` without waiting, then requests a background refresh if no read is in flight. Only version `0x620205`, exactly 2452 bytes (613 little-endian float32 values), and core IDs exactly 0–15 in CCD/slot order (0–7 on CCD0, 8–15 on CCD1) decode per-core lanes, for all cores including those outside the loaded set; lane i identifies core i. Zero-based float indices 301–316 are power in W, 317–332 voltage requests in V, 333–348 temperature in °C, and 381–396, 397–412 and 413–428 are C0, CC1 and CC6 residency in percent. Indices 349–380 remain undecoded because their meaning is not established. Granite Ridge feeds both CCDs from one VDDCR rail where the highest request wins: voltage lanes are requests, not separate core rails. Non-finite decoded values, any other version, size or topology, missing files, or a failed read omit `pm_table`; no zero-filled lanes replace missing data. Non-Linux hardware runners report no lanes. These readings never stop a trial or decide its outcome; their `trial.end` summaries below inform the tuner's R7 decisions.

Development simulator: a machine with `[shared_voltage]` emits synthetic voltage-request lanes for all 16 cores and clocks for loaded cores on every sample, retaining lazy generation and seeded determinism. Only multi-core R7 uses its shared-rail hazard; its highest loaded request across both CCDs sets the voltage and each loaded core's smooth hazard depends on the voltage minus its workload threshold. Such failures are crashes without fabricated MCEs unless the failing core's signal mix selects a computation error. R1–R6 and single-core failure rules are unchanged. Machines without the table retain their previous samples and journals. The parameter contract and steady-state package/thermal clock model are in [Simulating](../simulating.md#shared-voltage-r7-model).

For every R1–R7 trial, `trial.end` summarizes requested voltage over complete persisted samples: take the maximum voltage request among the loaded cores in each sample with decoded lanes, then take the median and minimum of those maxima (`journal.md`). Loaded cores include any temporarily suspended workers; idle cores outside the loaded set never count. An even sample count uses the mean of the two middle maxima. The watched end and resume after a crash or interruption use the same computation from `samples.jsonl`, skipping samples without lanes and a torn tail. If no sample has lanes, both fields are absent. This optional telemetry supplies no stability evidence.

The same persisted samples also supply each loaded core's median voltage request, each loaded CCD's top requesters (ties within 1 mV) and median clock in `trial.end`. This summary skips the first 5 s and needs at least 20 samples with decoded lanes; otherwise the fields are omitted. Each CCD's clock is the median over qualifying samples of that sample's median available loaded-core clock, rounded to whole MHz, and needs at least 20 sample medians for that CCD. Single-core trials use the same summary. These fields do not affect trial outcomes or the whole-trial voltage summary above (`journal.md`). Tuning decisions use them to order R7 partial parts, attribute multi-core R7 failures and size voltage-targeted backoffs (`tuner.md`).

A file read triggers the driver's SMU table transfer (RSMU `0x03`, about 0.3 ms measured). The reader starts one initial read and refreshes at sampling ticks, not in a separate polling loop. At most one read is in flight for the shared run reader, including the informational preflight check. A read has a 100 ms budget: while a read is overdue its lanes are omitted, and a late completion is discarded. A completed reading may be reused only within two seconds of its read's start; an older reading is omitted. A stuck driver read cannot be cancelled, so it occupies that reader instead of spawning replacement goroutines. Sampling, deadlines, cancellation, backend events and load steps never wait for it. The informational check waits only for the remaining part of the same 100 ms budget and reports pending/overdue, unsupported topology or other unavailability without failing preflight.

The driver's `pm_table_version` is a binary little-endian uint32, not hexadecimal text; a version read of any length other than four bytes is invalid. The decoder checks the actual `pm_table` payload length rather than relying on a separately advertised size.

When `/sys/class/powercap/intel-rapl:0/energy_uj` is readable, successive energy readings produce package watts as the microjoule delta divided by elapsed seconds and one million. The initial reading is taken when trial timing begins. `max_energy_range_uj` handles counter wrap; a missing reading or an unexplained counter decrease omits power for that interval. The hwmon, CPU and powercap roots are injectable runner options.
