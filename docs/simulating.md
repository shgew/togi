# Simulating

`tools/sim` runs a whole tuning session on the seeded simulator in `internal/sim`: a 16-core Zen 5 machine with hidden per-core edges, random failures and crashes. It needs no hardware and no root, and works on every development platform. It is a development program: the package does not ship it, so it runs from a source checkout.

```sh
just sim [seed]                                             # search, refinement and one clean qualifying rotation in a new temporary state directory
go run ./tools/sim [--seed N] [--machine FILE] [--rotations N] [--state-dir DIR]
```

- `--seed` (default 1) selects deterministic edges and failures; the same seed and history reproduce the journal.
- `--machine FILE` loads an explicit simulator machine TOML, including edges, joints, ranking, outcome scripts and reset reasons; `--seed` still sets its seed.
- `--rotations` (default 1) stops after N clean qualifying rotations once every core is done and refinement can reach no more depth.
- `--state-dir` uses an existing directory; without it, `sim` creates a temporary one and prints its path to stderr.

A crash reboots the simulated machine in-process and the next boot resumes the journal, as a real reboot would. Journal lines go to stderr as `togi run` logs them, and nothing is fsynced. A state directory that already holds a journal or archives resumes the simulated machine after them: boot numbering continues and the clock starts after the last event, so a crash in the new run is never mistaken for an old boot, and a session after `reset --all` gets a new id.

The session uses the default configuration, never `/etc/togi/config.toml`, and runs unattended: an unanswered too-aggressive defect is a dead end. `sim` exits 0 when the session stops cleanly, 1 at a dead end or on an error, and 2 on a flag error. A journal written under an older ruleset or schema is archived and seeds a new session, as `togi run` does; the resumed machine reports the BIOS context the journals recorded, so their failed marks carry. A journal written under a newer ruleset or schema is refused before another event is appended.

The read-only commands work on the result:

```sh
go run ./cmd/togi --state-dir <dir> status
go run ./cmd/togi --state-dir <dir> cert
go run ./cmd/togi --state-dir <dir> events --core 3
go run ./cmd/togi --state-dir <dir> watch
```

Fault injection, explicit edges and the failure model are a Go API for tests (`sim.Config`, `sim.Edges`, `sim.Model` and the methods on `sim.Machine`); `internal/sim/doc.go` describes the model. `internal/simrun` drives a session on the simulator across its crashes for tests that need a simulated journal.

A machine file sets the core count, BIOS context, ranking, model parameters, per-core edges, joints and scripted outcomes; unset keys keep the seeded defaults, and an unknown key is an error. If it specifies any per-core edges, it must provide a `[[core]]` table for every core. Set `[bios_context]` with `bios_version`, `board`, `cpu_model`, `microcode` and `boost_limit_mhz` to override the simulator's BIOS context. This one adds a pair that crashes only when cores 03 and 11 are both at −30 or deeper under R7:

```toml
cores = 16

[model.signals]
crash = 1

[[joint]]
members = { "3" = -30, "11" = -30 }
regimes = ["R7"]
```

Run it with `go run ./tools/sim --machine <file> --state-dir <dir>` and inspect `events --kind hunt,mark,refine` along with `status`, `cert` and `watch`. `sim.Config` also models late-onset hazards (six minutes of R6 idle or four minutes of R7 heat soak), a flat rare hazard, a failure on an idle core, a backend error just before a crash, and watchdog, power-loss and thermal-trip resets. A scripted outcome pins a particular trial and time for deterministic interruption tests. The simulator has separate wall and boot-local monotonic clocks, so a wall-clock jump need not change which MCE belongs to a trial.
