# Prior art

What togi takes from existing tools and research, and what it deliberately leaves out. Read before proposing a feature: if it is listed under Rejected, it needs a new ADR.

## linux-corecycler

[shgew/linux-corecycler](https://github.com/shgew/linux-corecycler), a fork of [Daaboulex/linux-corecycler](https://github.com/Daaboulex/linux-corecycler): Python and PySide6, a GUI plus a headless CLI.

### Taken: facts

- **SMU commands for Granite Ridge** (family `0x1A`, model `0x40`-`0x4F`), from `src/corecycler/smu/commands.py`:
  - `0x06` sets one core, `0x07` sets all cores, `0xD5` reads one core.
  - All go through the RSMU mailbox of `ryzen_smu` (`/sys/kernel/ryzen_smu_drv/rsmu_cmd` and `smu_args`).
  - `0x6E` reads the boost limit, which is used for the BIOS context.
  - The fork allows -50..+10; togi uses -50..0.
- **Argument encoding:** `(ccd << 28) | (slot << 20) | (offset & 0xFFFF)`, where `slot` is the physical core position within its CCD. Readback returns the offset as a 16-bit two's complement value in the low bits.
- **Core-to-slot mapping:**
  - The SMU addresses physical slots, counting fused-off ones.
  - On a full 8-core CCD, the kernel `core_id` modulo 8 is the slot. The CCD's core-disable fuse is read over SMN at `0x304A03DC + (ccd << 25)`, where a set bit means a disabled slot; only full CCDs with eight cores in the OS topology support per-core access.
  - Harvested parts can renumber `core_id`. The fuse identifies enabled slots, not which kernel core occupies each slot: assigning sorted kernel cores to ascending enabled slots is unverified. Any fused-off slot refuses all per-core reads and writes until the mapping is confirmed on a harvested part.
  - An independent identity probe would require experimental SMU writes to identify which core moved. It is deferred because the available hardware has only full CCDs.
  - On the 9950X3D2 both CCDs are full: logical CPUs N and N+16 are the two threads of core N.
- **Volatility:** runtime CO values reset on reboot and are never written to BIOS or NVRAM.
- **Backend quirks:** listed under Backends in `spec/workloads.md`.
- **Failure detection:** machine checks come from the kernel log, not from rasdaemon or EDAC counters. A failure to read the kernel log is an environment fault, not a pass.
- **Unattended recovery:** a GRUB specialisation with a saved default entry, panic-on-oops/lockup sysctls, a systemd runtime watchdog, and a resume service with a restart limit. The owner's NixOS config, not the fork, originally implemented this; togi now provides it in its [NixOS module](../nix/module.nix).

### Taken: ideas

- Write the intended offset durably before the SMU write, then read it back.
- Confine backends with a cgroup cpuset (`AllowedCPUs`) rather than trusting their own affinity settings.
- Tell a same-boot process restart apart from a real reboot by boot ID.

### Rejected

| Feature | Why |
|---|---|
| Qt GUI, desktop entry, notifications | CLI first; `togi watch` reads the journal as a TUI |
| Full monitoring subsystem (hwmon, SPD, RAPL, APERF/MPERF, Super I/O) | Not needed to judge a trial; togi samples temperatures, per-core clocks, the PM table and package energy-derived power, not a general monitoring subsystem |
| SQLite history | [ADR 0003](adr/0003-journal-is-source-of-truth.md) |
| Seven validation stages, endurance banks, annealing | Replaced by alone search, parked hunts, deepening and continuing checking with clean cycles ([ADR 0020](adr/0020-hunt-and-refine.md), [ADR 0028](adr/0028-remove-tiers.md)) |
| Multi-boot crash hunt and bisection | Adopted outside multi-core R7: journaled parked delta debugging with complements, fixed loaded cores and workload, and cautious fallback combinations ([ADR 0020](adr/0020-hunt-and-refine.md)); multi-core R7 instead uses measured top requesters, partial chains and voltage-targeted backoff on every failure ([ADR 0038](adr/0038-self-sufficient-cores.md)); not simple load-splitting bisection |
| Zen 1-5 command table, APU dialects | Zen 5 desktop only; other generations are [#20](https://github.com/shgew/togi/issues/20) |
| stress-ng and stressapptest backends | [ADR 0008](adr/0008-self-checking-workloads.md) |
| PBO limit, scalar and frequency control | togi tunes CO only; the rest is BIOS context |

## sp00n/corecycler

[sp00n/corecycler](https://github.com/sp00n/corecycler): Windows PowerShell, the original per-core cycler.

### Taken

- Test one core at a time with one thread. All-core load lowers boost and hides single-core instability; the README's example is a 5900X that passed all-core Prime95 at -30 but needed -9 on one core.
- Prime95 SSE is the lightest load and reaches the highest boost; heavier AVX modes cover other units. Both are needed.
- Suspending and resuming the backend (`suspendPeriodically`) produces load transitions. togi adds configurable periods and duty cycles (R3, R4).
- Apply the offset only to the tested core (`setVoltageOnlyForTestedCore`); this is togi's alone condition.
- Alternate CCDs between cores to spread heat.

### Rejected

- **One-way search**, which only moves offsets shallower after errors and never looks for the deepest stable value.
- **Resume via a Windows logon task**, and restarting the core order after a crash (issue #106).
- A CPU-load threshold without a startup grace period, which falsely failed y-cruncher on slow starts (issue #146).

## Other tools

- **AMD Ryzen Master auto CO:** AMD warns that its values depend on the chosen test duration ([CO FAQ](https://www.amd.com/content/dam/amd/en/documents/products/software-tools/faq-curve-optimizer.pdf)).
- **1usmus HYDRA:** optimizes for performance; its method is undisclosed.
- **ClockTuner for Ryzen:** Zen 2/3 only. It accepted settings that failed Prime95 small FFTs within seconds ([Chips and Cheese](https://chipsandcheese.com/p/ctr-safety-revisited)).
- **OCCT:** has a Linux build but is proprietary and not in nixpkgs.

None of these has a real method for idle stability.

## Research

Why finite testing cannot prove stability, and what that implies for the method:

- **The factory has more tools.** AMD bins dies with test equipment, structural scan tests and voltage/frequency/temperature sweeps, then fuses a V/F curve that includes a guardband for aging, heat, droop and worst-case workloads. Curve Optimizer spends that guardband. Software sees only its own workloads on one machine ([AMD CO FAQ](https://www.amd.com/content/dam/amd/en/documents/products/software-tools/faq-curve-optimizer.pdf); [Zen, ISSCC 2017](https://ieeexplore.ieee.org/document/7870348)).
- **Finite testing misses rare failures.** Failure-free trials cover the tested workloads and profiles, not every future workload or real-use condition. Repeatedly selecting evidence after failures does not make it a guarantee. Choose additional checking cycles with `run --cycles N` rather than treating a label or elapsed testing time as proof ([ADR 0028](adr/0028-remove-tiers.md)).
- **Targeted short tests beat long generic ones.** Micro-viruses aimed at individual units matched or exceeded SPEC-derived Vmin in 19 of 24 cases while cutting characterization from months to days ([Papadimitriou et al., ISPASS 2018](https://ieeexplore.ieee.org/document/8366935); [MICRO 2017](https://dl.acm.org/doi/10.1145/3123939.3124537)). Hence several distinct regimes instead of one long burn.
- **Current transients are their own failure mode.** Generated di/dt stressmarks failed 62 mV higher than a hand-written maximum-power test ([AUDIT, MICRO 2012](https://ieeexplore.ieee.org/document/6493621); [GeST](https://github.com/toolsForUarch/GeST)). Hence R3 and R4.
- **Corrected errors come before crashes** ([Bacha and Teodorescu, ISCA 2013](https://www.cse.ohio-state.edu/~teodorescu.1/download/papers/bacha-isca13.pdf)). Hence corrected MCEs count as failures.
- **Wrong results can be silent** ([Cores that don't count, HotOS 2021](https://sigops.org/s/conferences/hotos/2021/papers/hotos21-s01-hochschild.pdf); [Meta, 2022](https://arxiv.org/abs/2203.08989)). Hence [ADR 0008](adr/0008-self-checking-workloads.md).
- **Idle cannot be simulated.** A busy loop never enters deep C-states. R6 covers what synthetic idle can; real idle evidence would need observation during real use, which the planned `observe` service does not yet provide.
