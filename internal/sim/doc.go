// Package sim is a seeded fake Zen 5 machine behind every hardware seam in package machine.
//
// # Edges
//
// Each core has a hidden edge per regime and condition: the deepest offset at which it is stable. Isolated edges
// cover R1-R5; resident edges cover R1-R7 and are never deeper than the isolated ones, so some instability only shows
// when every core carries its offset. Unless Config.Edges gives them, edges are drawn from the seed: a base B uniform
// in [-40, -5] per core, isolated R1-R5 at B+u with u uniform in {0, 1, 2}, resident R1-R5 at isolated+v, and
// resident R6 and R7 at the deepest isolated edge plus v, where v is 0 with probability 0.75 and otherwise uniform in
// {1, 2, 3}. Edges are clamped to [-50, 0]; an explicit edge of 1 makes even offset 0 fail.
//
// # Failures
//
// A target core at offset o, whose edge for the trial's regime and condition is E, is d = E - o counts past the
// edge. It fails at rate PastEdgeRate * Growth^(d-1) per second when d >= 1, and at NearEdgeRate otherwise. With the
// default model a 90 s trial one count past the edge fails 90% of the time, each further count quadruples the rate,
// and nothing fails at or shallower than the edge. The failure time is exponential; a trial passes when no target
// fails within its duration. R3 and R4 fail by their edges alone: the simulator follows the load-step schedule of
// package machine only to report SIGSTOP and SIGCONT counts, not to change failure rates.
//
// Resident trials fail by each target's resident edge at the offset in its register, so every other core's offset
// only matters when it is a target too. R6 and R7 trials target every core for their whole duration: each core fails
// by its own R6 or R7 edge, and the first to fail produces the signal. R6's idle half and R7's per-CCD phases are not
// modelled.
//
// A failing core produces one signal, drawn by the Model.Signals weights:
//   - computation_error, stall, unexpected_exit: the trial ends at the failure time with that signal;
//   - corrected_mce: a corrected MCE on the core's first logical CPU enters the current boot's kernel log and the
//     trial runs to its end, so only the kernel log shows the failure;
//   - crash: the machine dies at the failure time. With probability CrashMCE an uncorrected MCE is left in the bank
//     and logged at the start of the next boot, as the kernel does after a warm reset.
//
// An MCE bank is core-local with probability CoreLocalBank, else shared.
//
// # Determinism
//
// Every draw comes from a PCG seeded by Config.Seed and an FNV-1a hash of what is being drawn. A trial's draws are
// keyed by core, regime, condition, offset and TrialSpec.Index, so the same seed and the same history always yield
// the same observations, and a retried trial sees the same outcome.
//
// # Machine lifecycle
//
// The clock starts at 2026-01-01T00:00:00Z, or at Config.Start, and advances only by trial time and 90 s per reboot.
// Config.Boots continues boot numbering; Resume sets both from a state directory's journal and archives, so a machine
// built to resume a journal gets boot IDs the journal has not seen. Crash stops the machine: every seam call returns
// machine.ErrCrashed until Reboot starts the next boot with a new boot ID and the BIOS offsets in every register.
// Faults (failed SMU writes, corrupt readbacks, setup failures, escaped threads, failed preflight checks, crashes
// before the first write of a boot) are injected by the methods on Machine.
package sim
