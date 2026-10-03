// Package sim is a seeded fake Zen 5 machine behind every hardware seam in package machine.
//
// # Edges
//
// Each core has a hidden isolated edge for R1-R5 and resident edge for R1-R7. The isolated edge applies when
// all other cores' registers are zero; otherwise the resident edge applies, regardless of trial condition.
// Workload-specific edges override both. Unless Config.Edges gives them, edges are drawn from the seed: a base B uniform
// in [-40, -5] per core, isolated R1-R5 at B+u with u uniform in {0, 1, 2}, resident R1-R5 at isolated+v, and
// resident R6 and R7 at the deepest isolated edge plus v, where v is 0 with probability 0.75 and otherwise uniform in
// {1, 2, 3}. Edges are clamped to [-50, 0]; an explicit edge of 1 makes even offset 0 fail.
//
// # Failures
//
// A loaded core at offset o, with edge E for the workload or regime, is d = E - o counts past the edge.
// It fails at rate PastEdgeRate * Growth^(d-1) per second when d >= 1, and at NearEdgeRate otherwise. With the
// default model a 90 s trial one count past the edge fails 90% of the time, each further count quadruples the rate,
// and nothing fails at or shallower than the edge. Flat adds an independent rate at any nonzero offset.
// OnsetBoost increases the hazard for the first OnsetS seconds, and joints add hazards while all members are deep
// enough, possibly after a delay. Idle edges, and joints with no member in the loaded set, can crash a trial through a
// core outside its loaded set. Joint crashes leave no MCE unless Joint.CrashMCECore explicitly requests misleading
// core-local evidence; idle per-core crashes leave no MCE.
// R3 and R4 follow their load-step schedules only to report SIGSTOP and SIGCONT counts, not to change failure rates.
//
// Config.SingleCore opts R1/R2 singleton starts into a smooth loaded-core hazard:
// exp(LogRate + Core[c] + Workload[id] + Slope*(-offset-25)) failures/second.
// A positive MaxRate bounds this smoothly to MaxRate/(1+MaxRate/rate);
// zero leaves the exponential rate unchanged.
// It replaces that loaded core's edge and flat hazards, not unloaded-core hazards.
// Isolated and resident profiles share the same loaded-core rule. Missing workload
// effects are zero; machine files without [single_core] retain their old behavior.
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
// The clock starts at 2026-01-01T00:00:00Z, or at Config.Start. Trial time and Sleep advance boot-local monotonic
// time; JumpWall moves only wall time, and reboot advances 90 seconds before starting a fresh monotonic clock.
// Config.Boots continues boot numbering; Resume sets both from a state directory's journal and archives, so a machine
// built to resume a journal gets boot IDs the journal has not seen. Crash stops the machine: every seam call returns
// machine.ErrCrashed until Reboot starts the next boot with a new boot ID and the BIOS offsets in every register.
// Faults (partial writes, missing backends, corrupt readbacks, setup failures, escaped threads, failed preflight
// checks, crashes before the first write of a boot) are injected by the methods on Machine.
package sim
