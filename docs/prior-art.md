# Prior art

What shycler takes from existing tools and research, and what it deliberately leaves out. Read before proposing a feature: if it is listed under Rejected, it needs a new ADR.

## linux-corecycler

[shgew/linux-corecycler](https://github.com/shgew/linux-corecycler), a fork of [Daaboulex/linux-corecycler](https://github.com/Daaboulex/linux-corecycler): Python and PySide6, a GUI plus a headless CLI.

### Taken: facts

- **SMU commands for Granite Ridge** (family `0x1A`, model `0x40`-`0x4F`), from `src/corecycler/smu/commands.py`:
  - `0x06` sets one core, `0x07` sets all cores, `0xD5` reads one core.
  - All go through the RSMU mailbox of `ryzen_smu` (`/sys/kernel/ryzen_smu_drv/rsmu_cmd` and `smu_args`).
  - `0x6E` reads the boost limit, which is used for the BIOS context.
  - The fork allows -50..+10; shycler uses -50..0.
- **Argument encoding:** `(ccd << 28) | (slot << 20) | (offset & 0xFFFF)`, where `slot` is the physical core position within its CCD. Readback returns the offset as a 16-bit two's complement value in the low bits.
- **Core-to-slot mapping:**
  - The SMU addresses physical slots, counting fused-off ones.
  - On a full 8-core CCD, the kernel `core_id` modulo 8 is the slot. Harvested parts can renumber `core_id`, so the mapping is verified against the CCD's core-disable fuse, read over SMN at `0x304A03DC + (ccd << 25)`, where a set bit means a disabled slot.
  - If the mapping cannot be verified, per-core writes are refused.
  - On the 9950X3D2 both CCDs are full: logical CPUs N and N+16 are the two threads of core N.
- **Volatility:** runtime CO values reset on reboot and are never written to BIOS or NVRAM.
- **Backend quirks:** listed under Backends in `spec/workloads.md`.
- **Failure detection:** machine checks come from the kernel log, not from rasdaemon or EDAC counters. A failure to read the kernel log is an environment fault, not a pass.
- **Unattended recovery:** a GRUB specialisation with a saved default entry, panic-on-oops/lockup sysctls, a systemd runtime watchdog, and a resume service with a restart limit. The owner's NixOS config, not the fork, implements this today.

### Taken: ideas

- Write the intended offset durably before the SMU write, then read it back.
- Confine backends with a cgroup cpuset (`AllowedCPUs`) rather than trusting their own affinity settings.
- Tell a same-boot process restart apart from a real reboot by boot ID.

### Rejected

| Feature | Why |
|---|---|
| Qt GUI, desktop entry, notifications | CLI first; a TUI later reads the journal |
| Monitoring subsystem (hwmon, SPD, RAPL, APERF/MPERF, Super I/O) | Not needed to judge a trial; shycler reads Tctl only |
| SQLite history | [ADR 0003](adr/0003-journal-is-source-of-truth.md) |
| Seven validation stages, endurance banks, annealing | Replaced by confirmation plus an endless guard with tiers |
| Multi-boot crash hunt and bisection | [ADR 0007](adr/0007-suspect-backoffs.md) |
| Zen 1-5 command table, APU dialects | Zen 5 desktop only; other generations are a roadmap item |
| stress-ng and stressapptest backends | [ADR 0008](adr/0008-self-checking-workloads.md) |
| PBO limit, scalar and frequency control | shycler tunes CO only; the rest is BIOS context |

## sp00n/corecycler

[sp00n/corecycler](https://github.com/sp00n/corecycler): Windows PowerShell, the original per-core cycler.

### Taken

- Test one core at a time with one thread. All-core load lowers boost and hides single-core instability; the README's example is a 5900X that passed all-core Prime95 at -30 but needed -9 on one core.
- Prime95 SSE is the lightest load and reaches the highest boost; heavier AVX modes cover other units. Both are needed.
- Suspending and resuming the backend (`suspendPeriodically`) produces load transitions. shycler adds configurable periods and duty cycles (R3, R4).
- Apply the offset only to the tested core (`setVoltageOnlyForTestedCore`); this is shycler's isolated condition.
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

Why stability can only be bounded, never proven, and what that implies for the method:

- **The factory has more tools.** AMD bins dies with test equipment, structural scan tests and voltage/frequency/temperature sweeps, then fuses a V/F curve that includes a guardband for aging, heat, droop and worst-case workloads. Curve Optimizer spends that guardband. Software sees only its own workloads on one machine ([AMD CO FAQ](https://www.amd.com/content/dam/amd/en/documents/products/software-tools/faq-curve-optimizer.pdf); [Zen, ISSCC 2017](https://ieeexplore.ieee.org/document/7870348)).
- **Zero failures only bounds the rate.** After `T` failure-free hours, the rate is below `3/T` per hour at 95% confidence ([Hanley and Lippman-Hand 1983](https://pubmed.ncbi.nlm.nih.gov/6827763/); [NIST](https://www.itl.nist.gov/div898/handbook/prc/section2/prc241.htm)). This defines the tiers.
- **Targeted short tests beat long generic ones.** Micro-viruses aimed at individual units matched or exceeded SPEC-derived Vmin in 19 of 24 cases while cutting characterization from months to days ([Papadimitriou et al., ISPASS 2018](https://ieeexplore.ieee.org/document/8366935); [MICRO 2017](https://dl.acm.org/doi/10.1145/3123939.3124537)). Hence several distinct regimes instead of one long burn.
- **Current transients are their own failure mode.** Generated di/dt stressmarks failed 62 mV higher than a hand-written maximum-power test ([AUDIT, MICRO 2012](https://ieeexplore.ieee.org/document/6493621); [GeST](https://github.com/toolsForUarch/GeST)). Hence R3 and R4.
- **Corrected errors come before crashes** ([Bacha and Teodorescu, ISCA 2013](https://www.cse.ohio-state.edu/~teodorescu.1/download/papers/bacha-isca13.pdf)). Hence corrected MCEs count as failures.
- **Wrong results can be silent** ([Cores that don't count, HotOS 2021](https://sigops.org/s/conferences/hotos/2021/papers/hotos21-s01-hochschild.pdf); [Meta, 2022](https://arxiv.org/abs/2203.08989)). Hence [ADR 0008](adr/0008-self-checking-workloads.md).
- **Idle cannot be simulated.** A busy loop never enters deep C-states. R6 covers what synthetic idle can; real idle evidence comes only from the future `observe` service, which is why Platinum requires field hours.
