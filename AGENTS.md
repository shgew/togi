# shycler

Go CLI that finds per-core Curve Optimizer offsets on Zen 5 desktop CPUs and keeps testing them. NixOS only.

## Docs

- `README.md`: what shycler does, what works today, and the common commands. The first page a reader sees.
- `CHANGELOG.md`: user-visible changes, in [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format.
- `CONTEXT.md`: the vocabulary. Name code, events and docs with its terms.
- `docs/spec/`: normative behavior. Read the relevant spec before changing behavior, and change spec and code in the same pull request.
  - `tuner.md`: offsets, phases, backoffs, regain, tiers, dead ends.
  - `workloads.md`: regimes, backends, containment, failure detection.
  - `journal.md`: events, state, logging.
  - `runtime.md`: commands, preflight, configuration, tuning boot, NixOS module.
- `docs/adr/`: decisions and the alternatives rejected. Reversing one needs a new ADR.
- `docs/prior-art.md`: before proposing a feature, check whether it was deliberately left out.
- `docs/ROADMAP.md`: the plan to 1.0: tasks, dependencies and status.

## Workflow

Every change, docs included, lands as a pull request against `main` on `code.marleb.org/shgew/shycler`. The owner reviews and merges. When a change is done, open its pull request without asking, unless told otherwise.

- Branch from `main` with a short descriptive name.
- One concern per pull request.
- Commit with short imperative messages.
- The pull request body follows `.github/pull_request_template.md`: a short summary, and the demo in a collapsed block.
- Address every review comment on the same branch.
- A pull request that starts or finishes a task in `docs/ROADMAP.md` updates its row.

## Keeping docs current

A pull request updates everything that describes the old state, in the same pull request:
- `--help` text for every command or flag it adds or changes;
- the README's Status when what works changes, and its Usage when the common commands change;
- `CHANGELOG.md` under `## [Unreleased]`, for every change a user of shycler would notice: commands, flags, behavior, output, configuration. One line per change under `Added`, `Changed`, `Fixed` or `Removed`, stating the effect and linking the pull request. Refactors, tests and doc edits that leave the tool unchanged get no entry;
- the specs, and any comment the change makes wrong.

The first pull request that makes something runnable on real hardware adds `docs/howto.md` with the operator's steps. Later pull requests that change those steps update it.

## Writing

- Write every file as if the repository were public: no personal hostnames, home paths, or setup specific to one machine or tool. Where there are several ways to get somewhere, name them, then continue as if the reader got there.
- Write commits, pull requests and docs for readers who have not seen the conversation that produced them.
- Never overstate: claim only what the demo or the checks showed.

## Commands

Enter the dev shell (Go, gopls, golangci-lint) with `nix develop`, or with `direnv allow` once per checkout if you use direnv. The commands below run inside it.

| Command | Use |
|---|---|
| `go test ./...` | The tight loop |
| `nix flake check` | Tests and lint; must pass before a pull request |
| `nix fmt` | Format Go and Nix files |
| `go run ./cmd/shycler run --sim 1` | A simulated session through its first clean guard rotation, to Bronze, in a temporary state directory |
| `go test -tags hardware ./...` | Hardware tests, on the target machine only: as root, or as a user with read-write access to `/sys/kernel/ryzen_smu_drv/{rsmu_cmd,smu_args,smn}` and a delegated cpuset controller. Backend package paths come from `SHYCLER_MPRIME` and `SHYCLER_YCRUNCHER`, else from `/etc/shycler/config.toml` |

## Layout

| Package | Owns |
|---|---|
| `cmd/shycler` | Command dispatch |
| `internal/config` | Configuration |
| `internal/machine` | Shared vocabulary and the seam interfaces the run loop consumes |
| `internal/journal` | Journal, replay, state file, log lines |
| `internal/tuner` | Pure decision engine: search, confirmation, guard, regain, tiers |
| `internal/sim` | Simulator implementing every hardware seam |
| `internal/session` | The run loop: session start, resume, crash attribution, trials, dead ends |
| `internal/smu` | `ryzen_smu`: the only package that writes offsets |
| `internal/trial` | Containment, sampling, load-step signaling |
| `internal/backend/*` | mprime and y-cruncher integrations |
| `internal/hardware` | Assembles the real machine: host, preflight, GRUB |
| `internal/detect` | Kernel log, MCE, crash detection |
| `nix/` | NixOS module and VM tests |

Keep packages near 1000 lines; split by responsibility when one grows past that.

## Code

- **Traceable:** every action and decision is a journal event with a `msg` a stranger can follow (`journal.md`). Verbose is the goal: someone reading only the journal can reconstruct what shycler did and why.
- **Safe writes:** offsets reach hardware only through `internal/smu`, clamped to [-50, 0], with an intent event before and a readback after.
- Comments only where names cannot carry the reason.
- Wrap errors with the operation that failed.

## Testing

- **Tight:** `go test ./...` finishes in under 10 seconds from a cold cache. Time comes from an injected clock, so tests never sleep.
- **Simulator first:** behavior is proven on `internal/sim` with fixed seeds, never by waiting for hardware.
- Tests pin spec behavior: rules, boundaries, invariants, crash-resume. Table tests for rules, property tests for invariants.
- Real-process tests use a helper program built by the test, not mprime or y-cruncher.
- Hardware tests carry `//go:build hardware`, run on the target machine, and restore every offset they change.
