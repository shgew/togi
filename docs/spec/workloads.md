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
| R7 all-core | Package power, thermals, cross-CCD interaction | One confined 1-thread R2 instance on each loaded core. A guard step runs CCD0 alone, CCD1 alone, then every core as three separate trials on two CCDs; on one CCD, one all-core trial |

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

Teardown, per instance: SIGCONT then SIGTERM to the process group, and up to 3 s for it to exit; then SIGKILL to everything in the scope (`systemctl kill --kill-whom=all`) and to the process group, and up to 10 s more. A backend still running after that is a runner error. The output left in the pipes and watched files is read to the end before the outcome is decided, so an error printed during teardown still fails the trial.

## Trial outcome

A trial passes when all of these hold:
- it ran for its full duration;
- the backend reported no computation error;
- the backend kept making progress;
- no failure signal named a core;
- every sampled thread was on an allowed logical CPU.

When evidence conflicts, containment violation wins (dead end), then a backend computation error or MCE in the trial window, then the reset reason, then inconclusive, then pass. A kernel log read failure makes a trial inconclusive after its retries, unless already-recorded higher-precedence failure evidence survives it.

A scoped instance that never entered its scope by the trial deadline is a setup error and makes the trial inconclusive unless higher-precedence evidence was found. A process exit queued before teardown begins is an unexpected exit; exits caused by teardown are not.

Failure signals:

| Signal | Detection | Attribution |
|---|---|---|
| Computation error | Backend output | The instance's core |
| Unexpected exit | Process exits before the trial ends, without a setup error | The instance's core |
| Stall | Over a 10 s window of unsuspended time, backend CPU time advances less than half of thread count times that window, after a 30 s startup grace | The instance's core |
| Corrected MCE | Kernel log, followed continuously while togi runs | The core of the reporting logical CPU if the bank is core-local, else unattributed |
| Uncorrected MCE | Kernel log of the next boot, or of the crashed boot in the persistent system journal | Same rule |
| Crash | Boot ID differs from the last journal boot with no clean shutdown; reset reason refines its classification | Unattributed unless a backend signal, MCE, or one nonzero applied offset names a core |

In isolated trials every failure belongs to the target. Resident and masked trials use the backend instance's signal, then exactly one core-local MCE, then the sole nonzero offset in the applied profile. Evidence naming more than one core without a higher-precedence backend signal remains unattributed.

Crash detection and evidence: every boot in the journal other than the current one, whose last event is not `shutdown` and that no `crash.detected` names, has crashed. Its evidence is every MCE in its own kernel log plus the uncorrected MCEs logged by the boot after it (the next boot in the journal, or the current boot), each recorded once as an `mce` event with `from_boot`. An MCE that an earlier `crash.detected` already cites is evidence of that crash only: a boot's own log starts with the MCEs its predecessor's crash left in the banks. Recovery is idempotent: detection, closing the crashed trial and its failure are each redone by the next `run` if a further crash or kill interrupts them, so a crash during recovery is detected on its own, normally as stray, without losing the first.

Kernel reset reasons are read from the preceding boot's `Previous system reset reason` line. Watchdog expiry, sync flood and CPU shutdown are crash failures. Power-button reset during a trial is a crash failure; outside a trial it is inconclusive. Thermal trip is a dead end unless earlier backend or MCE evidence established a failure. No reason line on a kernel supporting this log (6.16+) is treated as power loss, and is inconclusive only after reason reporting has been confirmed in another boot. Unknown or unsupported reasons keep ordinary crash handling. A recorded backend signal or relevant MCE always takes precedence over reset reason.

A trial still open when `run` starts, in a boot that did not crash (the same boot, or one that ended in `shutdown`), ends as `interrupted`: a failure (`corrected_mce`, or `uncorrected_mce` when none is corrected) when `mce` events were already recorded for it, else inconclusive. Interrupted trials do not count toward the inconclusive dead end.

A trial closed this way, after a crash or an interruption, records as its `duration_s` the time from its `trial.start` to its last `trial.progress`, `trial.signal` or `trial.sample` in the journal, or 0 without a `trial.start`. Nothing is recorded between those events, so after a crash this is a lower bound.

The `trial.end` message for a trial closed on resume says `last evidence Ns after start` instead of presenting `duration_s` as elapsed time. A trial that ends while togi watches it, including an orderly stop by signal, reports its measured duration with `after Ns`.

MCE attribution rules:
- Attribution uses the SMCA bank type decoded by the kernel (`edac_mce_amd`), never a hard-coded bank number.
- A decoded bank continuation is assigned only when exactly one MCE could own it. If multiple status records arrive before their bank lines, those records keep an unknown bank type and are unattributed; journal message order alone cannot identify the owner.
- Core-local types (load-store, instruction fetch, L2, decode, execution, floating point) name a core.
- Shared types (L3, memory controller, data fabric and others) name none.
- In resident trials, a core-local MCE on a core whose offset is 0 is an attributed failure at 0, which is a dead end.

MCA bank contents survive a warm reset and the kernel logs them early in the next boot. The tuning boot keeps the system journal persistent, so the crashed boot's last kernel messages stay readable.

## Temperature

Tctl comes from the first hwmon whose `name` is `k10temp` or `zenpower`, using the `temp*_input` whose `temp*_label` is `Tctl`. It is sampled once per second during each trial. Without such a sensor the trial end has no Tctl. The maximum goes into the trial's end event. Temperature never decides an outcome.
