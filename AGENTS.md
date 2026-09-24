# shycler

Go CLI that finds per-core Curve Optimizer offsets on Zen 5 desktop CPUs and keeps testing them. NixOS only. Private for now.

## Docs

- `CONTEXT.md`: the vocabulary. Name code, events and docs with its terms.
- `docs/spec/`: normative behavior. Read the relevant spec before changing behavior, and change spec and code in the same pull request.
  - `tuner.md`: offsets, phases, backoffs, regain, tiers, dead ends.
  - `workloads.md`: regimes, backends, containment, failure detection.
  - `journal.md`: events, state, logging.
  - `runtime.md`: commands, preflight, configuration, tuning boot, NixOS module.
- `docs/adr/`: decisions and the alternatives rejected. Reversing one needs a new ADR.
- `docs/prior-art.md`: before proposing a feature, check whether it was deliberately left out.
- `docs/ROADMAP.md`: tasks, dependencies and status.

## Workflow

Every change lands through a pull request on `code.marleb.org/shgew/shycler`. The owner reviews and merges.

1. Create a worktree on branch `<slug>`, starting from `main`, then run `direnv allow` in it.
2. Commit with short imperative messages.
3. Push, then open the pull request with `fj pr create --base main --head <slug> --body-file <file>`. The body holds:
   - a TL;DR;
   - the roadmap task;
   - the plan followed;
   - choices the reviewer may want to change;
   - the verification that ran;
   - a demo: the new behavior running, captured from a real invocation, such as a `--sim` session log, a journal excerpt or a command transcript. A pull request with nothing runnable says so.
4. Read review comments with `fj pr view <n> comments`, address each one, and push to the same branch.
5. Update the task's row in `docs/ROADMAP.md` in the same pull request.

## Commands

| Command | Use |
|---|---|
| `direnv allow` | Once per worktree: loads the flake's dev shell (Go, gopls, golangci-lint); `nix develop` is the manual equivalent |
| `go test ./...` | The tight loop |
| `nix flake check` | Tests, lint and module checks; must pass before a pull request |
| `nix fmt` | Format Go and Nix files |
| `go run ./cmd/shycler run --sim 1` | A simulated session through its first clean guard rotation in a temporary state directory |
| `sudo go test -tags hardware ./...` | Hardware tests, on the target machine only |

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
- Hardware tests carry `//go:build hardware`, run as root on the target machine, and restore every offset they change.
