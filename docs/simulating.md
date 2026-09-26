# Simulating

`tools/sim` runs a whole tuning session on the seeded simulator in `internal/sim`: a 16-core Zen 5 machine with hidden per-core edges, random failures and crashes. It needs no hardware and no root, and works on every development platform. It is a development program: the package does not ship it, so it runs from a source checkout.

```sh
just sim [seed]                                             # a session through its first clean guard rotation in a new temporary state directory
go run ./tools/sim [--seed N] [--rotations N] [--state-dir DIR]
```

- `--seed` (default 1) draws the machine's edges and failures. The same seed and the same history always give the same journal.
- `--rotations` (default 1) stops the session, recording `shutdown`, once the current profile has survived N clean guard rotations, before any automatic regain. Every regain changes the profile and restarts the count. One clean rotation earns Bronze only when nothing is left to regain.
- `--state-dir` uses an existing state directory. Without it, `sim` creates a new temporary directory and prints its path to stderr.

A crash reboots the simulated machine in-process and the next boot resumes the journal, as a real reboot would. Journal lines go to stderr as `shycler run` logs them, and nothing is fsynced. A state directory that already holds a journal or archives resumes the simulated machine after them: boot numbering continues and the clock starts after the last event, so a crash in the new run is never mistaken for an old boot, and a session after `reset --all` gets a new id.

The session uses the default configuration, never `/etc/shycler/config.toml`, and runs unattended: an unanswered too-aggressive defect is a dead end. `sim` exits 0 when the session stops cleanly, 1 at a dead end or on an error, and 2 on a flag error. A journal written under a different ruleset or schema is refused before another event is appended.

The read-only commands work on the result:

```sh
go run ./cmd/shycler --state-dir <dir> status
go run ./cmd/shycler --state-dir <dir> cert
go run ./cmd/shycler --state-dir <dir> events --core 3
go run ./cmd/shycler --state-dir <dir> watch
```

Fault injection, explicit edges and the failure model are a Go API for tests (`sim.Config`, `sim.Edges`, `sim.Model` and the methods on `sim.Machine`); `internal/sim/doc.go` describes the model. `internal/simrun` drives a session on the simulator across its crashes for tests that need a simulated journal.
