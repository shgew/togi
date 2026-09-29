# Workloads

Normative rules for what a trial runs, how it is contained, and how its outcome is judged. The research behind the regime choice is summarised in `../prior-art.md`.

## Backends

| Backend | nixpkgs attribute | Result check |
|---|---|---|
| mprime | `mprime` (unfree) | Torture test residue and rounding checks, reported in `results.txt` |
| y-cruncher | `y-cruncher` (unfree) | Built-in verification in stress mode, reported on stdout |

Every workload must check its own results: silent computation errors are a known failure mode ([ADR 0008](../adr/0008-self-checking-workloads.md)).

A backend integration:
- writes its configuration files into the trial's work directory, naming the exact logical CPUs and thread count, because both programs set their own affinity and would otherwise start workers on every CPU;
- produces the argument vector to launch;
- parses output as it arrives into progress, computation error and fatal setup error;
- reports setup problems as inconclusive, never as failures.

Known quirks the integrations handle:
- **mprime:** needs `NumCPUs`, `CoresPerTest`, the `CpuSupports*` switches for the instruction set and the FFT range in `local.txt`/`prime.txt`. `TortureTime=1` keeps each FFT size to one minute, so a trial covers several sizes. Errors land in `results.txt`, which the runner tails once per second, and mprime can keep running after one.
- **y-cruncher:** command-line thread options do not confine it; only a config file names the CPUs. It prints `Failed to set core affinity` when confinement and config disagree, which counts as a containment violation. Its startup is slow and needs the stall grace period.

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
| R7 all-core | Package power, thermals, cross-CCD interaction | One confined 1-thread R2 instance on each loaded core. A guard step has three loaded parts on two CCDs, CCD0 alone, CCD1 alone and every core, and one all-core part on one CCD; each part runs as separate trials (Durations and guard schedule) |

Within a regime, search cycles listed workloads; candidate-edge checks freeze one R1 and one R2 workload. Guard's repeated R1, R2 and R7 steps cycle their catalogs, so three occurrences cover each workload. All parts of one R7 step share its selected R2 workload; inconclusive starts retry the same class.

R6 and R7 run on the resident profile and in masked hunt trials; R7 also checks refinement rounds. R6 targets every core; an R7 trial targets one CCD or every core. `trial.intent.cores` lists exactly the loaded cores; the applied profile also includes idle cores. Each loaded core runs one instance on its first logical CPU, in its own scope and work directory, so a computation error or stall names that instance's core.

### Load-step schedules

An R3 or R4 workload starts running, then alternates on and off periods; each on period ends with SIGSTOP and each off period with SIGCONT. R3 draws each period independently and uniformly from {10 ms, 50 ms, 200 ms, 1 s, 5 s} with a PCG seeded by the recorded seed, so the seed reproduces the schedule. R4 runs for its duty share of every 100 ms period. The trial end records how many of each signal were sent.

## Durations and guard schedule

Defaults, all configurable:

| Use | Duration |
|---|---|
| Search trial (R1, then R2) | 90 s each; a candidate-edge class needs `n` passing starts |
| Short start for R7, hunt masks, refinement checks and backoff reruns | 120 s (`durations.start_s`) |
| Guard per-core trial (R1 to R5) | 2 min each |
| Guard R6 | 15 min |
| Guard R7 long starts | 20 min total on two CCDs: 5 min CCD0, 5 min CCD1, 10 min all cores; 20 min on one CCD |

For `D = durations.guard_all_core_s` and `n` CCDs, an R7 long start runs for `D` on one CCD; on multiple CCDs, each single-CCD part runs `floor(D / 4)` seconds and the all-core part runs `D - n*floor(D / 4)` seconds. Each part also runs three short `start_s` starts and one long start; when short and long durations match their required pass counts add. `guard_all_core_s` must be in [4, 86400]. A trial is torn down before the next start. Inconclusive starts repeat their part's loaded cores and workload; passing starts survive interruption. A hunt mask or refinement check instead needs `n` passing starts from `evidence.*`.

Default guard rotation, about 7.2 h on 16 cores:
1. R7, R7, R7: every R2 workload, each on CCD0, CCD1 and all cores, with three short starts and one long start per part.
2. R2, R2, R2: every R2 workload on every core.
3. R6.
4. R5 on every core.
5. R1, R1, R1: every R1 workload on every core.
6. R3 on every core.
7. R4 on every core.
8. R6.

A clean rotation qualifies only with at least three R1, three R2 and three R7 steps, and one each of R3, R4, R5 and R6. Custom schedules can end clean without qualifying; the end event lists missing coverage. Per-core steps follow `tuner.md`'s scheduling order. Search time depends on edge distance and failed steps; candidate checks take five starts of each frozen R1 and R2 class by default. Qualification is breadth coverage, not a guarantee against rare hourly failures.

## Containment

Each backend instance runs as a child of togi inside a transient scope confined to its logical CPUs:

```
systemd-run --scope --quiet --collect -p AllowedCPUs=<cpus> -p DefaultDependencies=no -- <argv>
```

`DefaultDependencies=no` keeps a system shutdown from stopping the scope on its own. Otherwise systemd stops the scope and the unit running togi at the same moment, the backend can exit before togi sees its signal, and the trial would end as an unexpected exit on the target instead of interrupted. Without the default dependencies, togi's teardown ends the scope after the signal reaches it.

The kernel enforces the cpuset whatever the backend does. togi also samples the processor field of every backend thread in `/proc/<pid>/task/*/stat` once per second, from the moment the process is inside its scope. A thread seen outside its allowed logical CPUs is a dead end. Stopping a trial terminates the whole scope.

Normal trial ends and partial-start rollbacks use the same teardown, with one absolute deadline shared across all instances:
1. Send SIGCONT, then SIGTERM to each process group, and allow up to 3 s of grace shared by all groups.
2. Send SIGKILL to every known scope with `systemctl kill --kill-whom=all`, including scopes whose launcher has exited and scopes attempted before a launch failed. The calls share a slot of at most 2 s and each gets only the remaining time.
3. Send SIGKILL to every launched process group.
4. Drain output for up to 10 s, without passing the absolute deadline. Close the output readers at the deadline and join their goroutines, releasing the log files even when a detached descendant holds the pipes open.

Teardown takes at most 15 s regardless of instance count. Cleanup succeeds only when every launched process has exited, output has drained, and every scope kill succeeded or confirmed that its scope no longer exists. A failed or timed-out scope kill, an unconfirmed process-group kill, or an undrained backend is a containment dead end: no next trial or tuning profile write follows. Offset restoration belongs to run-level cleanup, not partial-start rollback or trial outcome adjudication. Final output from the pipes and watched files is classified before the outcome, including unterminated computation errors printed during teardown.

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
| Crash | Boot ID differs from the last journal boot with no clean shutdown; reset reason refines its classification | Unattributed unless a backend signal, MCE, or one nonzero applied offset names a core |

In isolated trials every failure belongs to the target. Resident and masked trials use the backend instance's signal, then exactly one core-local MCE, then the sole nonzero offset in the applied profile. Evidence naming more than one core without a higher-precedence backend signal remains unattributed.

Crash detection and evidence: every boot in the journal other than the current one, whose last event is not `shutdown` and that no `crash.detected` names, has crashed. Its evidence is the MCEs in its own kernel log that are not between trials, plus the uncorrected MCEs logged by the boot after it (the next boot in the journal, or the current boot), each recorded once as an `mce` event with `from_boot`. Recovery also records the previous boot's between-trial MCEs with `between_trials: true`, without using them as crash evidence or decision causes. An MCE that an earlier `crash.detected` already cites is evidence of that crash only: a boot's own log starts with the MCEs its predecessor's crash left in the banks. Recovery is idempotent: detection, closing the crashed trial and its failure are each redone by the next `run` if a further crash or kill interrupts them, so a crash during recovery is detected on its own, normally as stray, without losing the first.

Recovery compares an old boot's MCE `monotonic_ns` with the profile-applied boundary of the open trial in that same boot. A crashed trial has no teardown timestamp: its unread kernel-log tail remains eligible after the start boundary, rather than being cut off at the last progress event. Uncorrected MCA evidence retained into the following boot is eligible for the preceding crash without comparing its new-boot clock to the old trial's clock. Corrected next-boot MCEs are not retained crash evidence. Events lacking `monotonic_ns` preserve the earlier replay behavior; an explicit zero is a timestamp, not a missing field.

Kernel reset reasons are read from the preceding boot's `Previous system reset reason` line. Watchdog expiry, sync flood and CPU shutdown are crash failures. Power-button reset during a trial is a crash failure; outside a trial it is inconclusive. Thermal trip is a dead end unless earlier backend or MCE evidence established a failure. No reason line on a kernel supporting this log (6.16+) is treated as power loss, and is inconclusive only after reason reporting has been confirmed in another boot. A boot whose kernel log holds neither its `Linux version` line nor a reason line, because its start was rotated out, reads as unsupported. An older boot the system journal no longer holds confirms nothing and is skipped without a retry; only the boot after the crash must be readable. Unknown or unsupported reasons keep ordinary crash handling. A recorded backend signal or relevant MCE always takes precedence over reset reason.

A trial still open when `run` starts, in a boot that did not crash (the same boot, or one that ended in `shutdown`), ends as `interrupted`: a failure (`corrected_mce`, or `uncorrected_mce` when none is corrected) when `mce` events were already recorded for it, else inconclusive. Interrupted trials do not count toward the inconclusive dead end.

A trial closed this way, after a crash or an interruption, records as its `duration_s` the time from its `trial.start` to its last `trial.progress`, `trial.signal` or `trial.sample` in the journal, or 0 without a `trial.start`. Nothing is recorded between those events, so after a crash this is a lower bound.

The `trial.end` message for a trial closed on resume says `last evidence Ns after start` instead of presenting `duration_s` as elapsed time. A trial that ends while togi watches it, including an orderly stop by signal, reports its measured duration with `after Ns`.

MCE attribution rules:
- Attribution uses the SMCA bank type decoded by the kernel (`edac_mce_amd`), never a hard-coded bank number.
- A decoded bank continuation is assigned only when exactly one MCE could own it. If multiple status records arrive before their bank lines, those records keep an unknown bank type and are unattributed; journal message order alone cannot identify the owner.
- A new error-description boundary closes orphaned continuation state, so a truncated record does not prevent attribution of the next independent record. A surplus bank continuation establishes interleaving and clears tentative attribution back to the earliest unresolved status; this can also clear independent records whose ownership the log cannot distinguish.
- Core-local types (load-store, instruction fetch, L2, decode, execution, floating point) name a core.
- Shared types (L3, memory controller, data fabric and others) name none.
- In resident trials, a core-local MCE on a core whose offset is 0 is an attributed failure at 0, which is a dead end.

MCA bank contents survive a warm reset and the kernel logs them early in the next boot. The tuning boot keeps the system journal persistent, so the crashed boot's last kernel messages stay readable.

## Temperature

Tctl comes from the first hwmon whose `name` is `k10temp` or `zenpower`, using the `temp*_input` whose `temp*_label` is `Tctl`. It is sampled once per second during each trial. Without such a sensor the trial end has no Tctl. The maximum goes into the trial's end event. Temperature never decides an outcome.
