# Cap isolated controller clocks and rotate loaded hosts

## Decision

Make the controller's placement part of the managed trial contract. During mapping-shaped trials, put it on an idle CO-0 CCD and cap that CCD's cpufreq policies. During whole-machine loads, rotate its physical host core and leave it uncapped; its work is part of that core's load. R6 and partial loads prefer an unloaded CO-0 core, otherwise rotate. On a single CCD rotate capped across tested cores and withhold the host's exposure credit. [The workload specification](../spec/workloads.md#controller-placement) owns the complete selection and credit rules.

The cap is the lowest permitted policy limit, respecting the current policy minimum. Discover shared policies and refuse a cap that would also limit a different tested core. Save original limits durably before changing them, read back writes, and restore and verify after confirmed workload teardown or same-boot recovery. An incomplete cap or placement cannot produce passing credit. [The runtime specification](../spec/runtime.md#trial-confinement) owns the transaction and recovery rules.

Keep managed placement and process-fault decisions opt-in until Ruleset 13 schedules their evidence contract. The existing scheduler retains its decisions; adding executable shapes is not permission to change which trials it runs.

## Reasons

Both CCDs share a voltage rail, so a full-speed controller on a nominally idle CO-0 core can raise the rail and rescue the tested cores. Conversely, a controller on an undervolted idle core is an untracked light load that can itself fail. An isolated capped controller reduces the first effect; rotation makes the unavoidable controller work explicit when no isolated host is available. A process fault is evidence, not an interruption to discard, and its kernel-reported CPU must not silently blame another tested core.

## Alternatives

- **Always use the least-deep idle core:** can put the controller on a nonzero offset and hides the controller's own load and faults.
- **Always reserve a core:** changes the shape of all-core and all-thread trials and never tests the complete machine under their intended load.
- **Always use an uncapped CO-0 core:** can support the shared rail and conceal voltage dependence in mapping trials.
- **Always cap a loaded host:** changes the load being tested. This is unavoidable on one CCD, where the host receives no exposure credit.
- **Assume one cpufreq policy per CPU:** can silently throttle the tested load on a kernel or driver with shared policies.
- **Restore only in normal teardown:** leaves the machine capped after controller death. A boot-bound durable intent allows same-boot restoration without applying stale values after reboot.
