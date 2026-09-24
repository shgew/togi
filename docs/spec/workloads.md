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
- **mprime:** needs `NumCPUs`, `CoresPerTest`, the `CpuSupports*` switches for the instruction set and the FFT range in `local.txt`/`prime.txt`. Errors land in `results.txt`, and mprime can keep running after one.
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
| R6 idle | Normal power management with the profile applied | First half: no load at all. Second half: short R1 bursts (100 ms every 2 s) on one core at a time in scheduling order |
| R7 all-core | Package power, thermals, cross-CCD interaction | One confined 1-thread R2 instance per core. First both CCDs, then CCD0 only, then CCD1 only |

Within a regime, each core cycles through the listed workloads trial by trial, so backends alternate on the same core.

R6 and R7 exist only in guard. Their trial targets every core: `trial.intent` lists every core in `cores`, and the trial runs on each core's first logical CPU. R7 runs one instance per core, so a computation error or stall stays attributed to that instance's core.

### Load-step schedules

An R3 or R4 workload starts running, then alternates on and off periods; each on period ends with SIGSTOP and each off period with SIGCONT. R3 draws each period independently and uniformly from {10 ms, 50 ms, 200 ms, 1 s, 5 s} with a PCG seeded by the recorded seed, so the seed reproduces the schedule. R4 runs for its duty share of every 100 ms period. The trial end records how many of each signal were sent.

## Durations and guard schedule

Defaults, all configurable:

| Use | Duration |
|---|---|
| Search trial (R1, then R2) | 90 s each |
| Confirmation trial (R1 to R5) | 5 min each |
| Guard per-core trial (R1 to R5) | 2 min each |
| Guard R6 | 15 min |
| Guard R7 | 20 min: 10 min both CCDs, 5 min per CCD |

Default guard rotation, about 3.5 h:
1. R1 on every core
2. R2 on every core
3. R6
4. R3 on every core
5. R4 on every core
6. R7
7. R5 on every core
8. R6

Per-core steps follow the scheduling order in `tuner.md`.

Rough times to Bronze on 16 cores: search 6-9 h depending on how far edges lie from the baseline, confirmation about 7 h, first rotation about 3.5 h.

## Containment

Each backend instance runs as a child of shycler inside a transient scope confined to its logical CPUs:

```
systemd-run --scope --quiet --collect -p AllowedCPUs=<cpus> -- <argv>
```

The kernel enforces the cpuset whatever the backend does. shycler also samples the processor field of every backend thread in `/proc/<pid>/task/*/stat` once per second. A thread seen outside its allowed logical CPUs is a dead end. Stopping a trial terminates the whole scope.

## Trial outcome

A trial passes when all of these hold:
- it ran for its full duration;
- the backend reported no computation error;
- the backend kept making progress;
- no failure signal named a core;
- every sampled thread was on an allowed logical CPU.

When evidence conflicts, it is weighed in this order: a containment violation first (a dead end), then a failure signal from the backend or any MCE in the trial window, then inconclusive, then pass. A kernel log that cannot be read makes the trial inconclusive.

Failure signals:

| Signal | Detection | Attribution |
|---|---|---|
| Computation error | Backend output | The instance's core |
| Unexpected exit | Process exits before the trial ends, without a setup error | The instance's core |
| Stall | Over a 10 s window of unsuspended time, backend CPU time advances less than half of thread count times that window, after a 30 s startup grace | The instance's core |
| Corrected MCE | Kernel log, followed continuously while shycler runs | The core of the reporting logical CPU if the bank is core-local, else unattributed |
| Uncorrected MCE | Kernel log of the next boot, or of the crashed boot in the persistent system journal | Same rule |
| Crash | Boot ID differs from the last one in the journal, with no clean-shutdown event | Unattributed, unless an MCE above names a core |

In isolated trials every failure is attributed to the target, including an MCE that names another core: only the target carries an offset (`tuner.md`). In resident trials, evidence naming more than one core is unattributed.

Crash detection and evidence: every boot in the journal other than the current one, whose last event is not `shutdown` and that no `crash.detected` names, has crashed. Its evidence is every MCE in its own kernel log plus the uncorrected MCEs logged by the boot after it (the next boot in the journal, or the current boot), each recorded once as an `mce` event with `from_boot`. An MCE that an earlier `crash.detected` already cites is evidence of that crash only: a boot's own log starts with the MCEs its predecessor's crash left in the banks. Recovery is idempotent: detection, closing the crashed trial and its failure are each redone by the next `run` if a further crash or kill interrupts them, so a crash during recovery is detected on its own, normally as stray, without losing the first.

A trial still open when `run` starts, in a boot that did not crash (the same boot, or one that ended in `shutdown`), ends as `interrupted`: a failure (`corrected_mce`, or `uncorrected_mce` when none is corrected) when `mce` events were already recorded for it, else inconclusive. Interrupted trials do not count toward the inconclusive dead end.

MCE attribution rules:
- Attribution uses the SMCA bank type decoded by the kernel (`edac_mce_amd`), never a hard-coded bank number.
- Core-local types (load-store, instruction fetch, L2, decode, execution, floating point) name a core.
- Shared types (L3, memory controller, data fabric and others) name none.
- In resident trials, a core-local MCE on a core whose offset is 0 is an attributed failure at 0, which is a dead end.

MCA bank contents survive a warm reset and the kernel logs them early in the next boot. The tuning boot keeps the system journal persistent, so the crashed boot's last kernel messages stay readable.

## Temperature

Tctl from the `k10temp` hwmon (`temp1_input`) is sampled once per second during each trial. The maximum goes into the trial's end event. Temperature never decides an outcome.
