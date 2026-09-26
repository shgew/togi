# shycler

Go CLI that finds per-core Curve Optimizer offsets on Zen 5 desktop CPUs and keeps testing them. Runs on NixOS; development also works on macOS.

## Docs

- `README.md`: what shycler does, what works today, and the common commands. The first page a reader sees.
- `CHANGELOG.md`: user-visible changes, in [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format.
- `CONTEXT.md`: the vocabulary. Name code, events and docs with its terms.
- `docs/spec/`: normative behavior. Read the relevant spec before changing behavior, and change spec and code in the same pull request.
  - `tuner.md`: offsets, phases, backoffs, regain, tiers, dead ends.
  - `workloads.md`: regimes, backends, containment, failure detection.
  - `journal.md`: events, state, logging.
  - `runtime.md`: commands, preflight, configuration, tuning boot, NixOS module.
- `docs/releasing.md`: versioning rules and the release pull request and tagging procedure.
- `docs/simulating.md`: running a simulated session with `tools/sim`.
- `docs/adr/`: decisions and the alternatives rejected. Reversing one needs a new ADR.
- `docs/prior-art.md`: before proposing a feature, check whether it was deliberately left out.
- Issues on `code.marleb.org/shgew/shycler`: the plan, ideas and bugs (Issues, below).

## Workflow

Every change, docs included, lands as a pull request against `main` on `code.marleb.org/shgew/shycler`. The owner reviews and merges. When a change is done, open its pull request without asking, unless told otherwise.

- Branch from `main` with a short descriptive name.
- One concern per pull request.
- Commit with short imperative messages.
- The pull request body follows `.github/pull_request_template.md`: a short summary, and the demo in a collapsed block.
- Address every review comment on the same branch.
- A pull request that finishes an issue says `Closes #N` in its body; one that only makes progress says `Refs #N`.
- A pull request that bumps `journal.Schema` or `tuner.Ruleset` is breaking: its title starts with `[BREAKING]`, it carries the `breaking` label, and its changelog line starts with `**BREAKING**`.
- A breaking pull request merges only after `[Unreleased]` has been released (see `docs/releasing.md`).
- A pull request that fixes a bug that changed decisions adds a defect entry, with a test replaying a fixture journal from before the fix, when the affected decisions can be matched. Otherwise its changelog line tells the operator which `shycler reset --core` to run.

## Issues

Planning lives in issues, filed from the templates in `.github/ISSUE_TEMPLATE/`.

- Labels name the kind: `idea` (a thought, not yet discussed), `design` (decided, waiting to be scheduled), `feature` (ready to build), `bugfix`, and `breaking` on issues and pull requests alike.
- The `1.0` milestone holds what ships in 1.0.
- Lifecycle: an idea is discussed until decided, then its issue becomes a design with Why, Decided, Open and Links. When a discussion settles decisions, file or update the issue before it ends. The pull request that implements a design moves its decisions into the spec or an ADR and closes the issue; the spec and ADRs stay the lasting record.

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
- Describe workflow steps by the action (open a pull request, set the milestone), so they hold whatever program performs them. Name the repository's own commands.

## Commands

Enter the dev shell (Go, gopls, golangci-lint, govulncheck, just, nixfmt) with `nix develop`, or with `direnv allow` once per checkout if you use direnv. Recipes also work outside the dev shell: they enter it with `nix develop` when needed.

| Command | Use |
|---|---|
| `just` | List the recipes |
| `just test` | The tight loop |
| `just gate` | Lint, formatting check and tests: the quick check before handing off |
| `just check` | Every flake check: package (its tests run shuffled under the race detector, with the integration tests on Linux), lint and the VM test; must pass before a pull request; CI runs it on every pull request and push to `main` |
| `just fmt` | Format Go, Nix and the justfile |
| `just sim [seed]` | A simulated session through its first clean guard rotation in a temporary state directory (`go run ./tools/sim`, `docs/simulating.md`) |
| `just release` | Open the release pull request, or tag a merged one; see `docs/releasing.md` |
| `just hardware` | Hardware tests, on the target machine only: as root, or as a user with read-write access to `/sys/kernel/ryzen_smu_drv/{rsmu_cmd,smu_args,smn}` and a delegated cpuset controller. Backend package paths come from `SHYCLER_MPRIME` and `SHYCLER_YCRUNCHER`, else from `/etc/shycler/config.toml` |
| `just fuzz [time]` | Fuzz the journal parser |
| `just vuln` | Known vulnerabilities in called dependency and standard library code; CI runs it |

CI (`.forgejo/workflows/check.yml`) runs on a Forgejo runner offering the `nix-latest` label (the `nixos/nix` image) with `/dev/kvm` passed to job containers, which the NixOS VM test needs.

On macOS (aarch64-darwin) the dev shell, `just test`, `just gate`, `just sim` and the read-only commands work; `just check` builds `package` and `lint` and skips the VM test, `just hardware` and the `integration` tests are Linux-only. Linux-only code follows the Go convention: OS-suffixed files (`_linux.go`, `_darwin.go`) for real implementations, and a `//go:build !linux` fallback returning a wrapped `errors.ErrUnsupported`.

A command needed twice gets a recipe, in the same pull request.

## Layout

| Package | Owns |
|---|---|
| `cmd/shycler` | Command dispatch |
| `internal/config` | Configuration |
| `internal/machine` | Shared vocabulary and the seam interfaces the run loop consumes |
| `internal/defect` | Known decision-changing bugs and pure matching against the journal |
| `internal/journal` | Journal, replay, state file, log lines |
| `internal/tuner` | Pure decision engine: search, confirmation, guard, regain, tiers |
| `internal/sim` | Simulator implementing every hardware seam, and resuming it after a journal |
| `internal/session` | The run loop: session start, resume, crash attribution, trials, dead ends |
| `internal/simrun` | A session on the simulator, across its crash reboots |
| `internal/smu` | `ryzen_smu`: the only package that writes offsets |
| `internal/trial` | Containment, sampling, load-step signaling |
| `internal/backend/*` | mprime and y-cruncher integrations |
| `internal/hardware` | Assembles the real machine: host, preflight, GRUB |
| `internal/detect` | Kernel log, MCE, crash detection |
| `nix/` | NixOS module and VM tests |
| `tools/*` | Development programs, never shipped: `release`, `sim`. Development and debugging behavior lives here, never in `cmd/shycler` |

Keep packages near 1000 lines; split by responsibility when one grows past that.

## Code

- **Traceable:** every action and decision is a journal event with a `msg` a stranger can follow (`journal.md`). Verbose is the goal: someone reading only the journal can reconstruct what shycler did and why.
- **Safe writes:** offsets reach hardware only through `internal/smu`, clamped to [-50, 0], with an intent event before and a readback after.
- Comments only where names cannot carry the reason.
- Wrap errors with the operation that failed.

## Testing

- **Fast and deterministic:** a unit test exercises logic, never the world around it. It does not wait on real time, reach the network, start processes or depend on the machine it runs on: time comes from an injected clock or a `testing/synctest` bubble, everything else from fakes. Keep each test as quick as the behavior it proves allows.
- **Simulator first:** behavior is proven on `internal/sim` with fixed seeds, never by waiting for hardware.
- Tests pin spec behavior: rules, boundaries, invariants, crash-resume. Table tests for rules, property tests for invariants, golden files for rendered output (`go test ./cmd/shycler -update` rewrites them), a fuzz target for the journal parser (`just fuzz`). Compare values with `cmp.Diff`.
- Concurrent code is tested on real goroutines under the race detector: `just check` runs the suite with `-race`, and `just test -race` is the quicker local pass.
- Tests that need the real world carry a build tag and stay out of `go test ./...`: `integration` for real processes (a helper program built by the test, never mprime or y-cruncher), `hardware` for the target machine, restoring every offset they change.
