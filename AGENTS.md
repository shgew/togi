# togi

Go CLI that finds per-core Curve Optimizer offsets on Zen 5 desktop CPUs and keeps testing them. Runs on NixOS; development also works on macOS.

## Docs

- `README.md`: what togi does, what works today, and the common commands. The first page a reader sees.
- `CHANGELOG.md`: user-visible changes, in [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format.
- `CONTEXT.md`: the vocabulary. Name code, events and docs with its terms.
- `REVIEW.md`: defect criteria and recurring lessons. Read it before reviewing a pull request.
- `.omp/`: reviewer rules and `/review-pr`, the omp command for reviewing pull requests in parallel and recording their gates.
- `docs/spec/`: normative behavior. Read the relevant spec before changing behavior, and change spec and code in the same pull request or in layers of one stack merged together.
  - `tuner.md`: offsets, search, hunt, refinement, guard, qualified rotations, dead ends.
  - `workloads.md`: regimes, backends, containment, failure detection.
  - `journal.md`: events, state, logging.
  - `runtime.md`: commands, preflight, configuration, tuning boot, NixOS module.
- `docs/releasing.md`: versioning rules, the release workflow and publishing.
- `docs/simulating.md`: running a simulated session with `tools/sim`.
- `docs/reviewing.md`: reviewing an unattended run or archived journal with `just stats`, and writing its retro on the pinned "Target-machine runs" issue.
- `docs/benchmarking.md`: measuring a tuner change's time to conclusion, depth and hazard across simulated machines with `tools/bench`; run it before and after any change to how the tuner decides.
- `tools/bench/program.md`: autonomous tuner research; follow it when planning experiments, keeping bench wins and turning them into pull requests.
- `docs/adr/`: decisions and the alternatives rejected, with statuses in [the index](docs/adr/README.md). The specs describe current behavior; where they disagree with an ADR, the spec wins and the index is out of date. Reversing a decision needs a new ADR, which updates the index in the same pull request.
- `docs/prior-art.md`: before proposing a feature, check whether it was deliberately left out.
- Issues on `github.com/shgew/togi`: the plan, ideas and bugs (Issues, below).

## Workflow

Every change, docs included, lands as a pull request against `main` on `github.com/shgew/togi`, or as a layer of a stack of pull requests that ends on `main`. The owner merges; agents merge only when the owner asks. When a change is done, open its pull request without asking, unless told otherwise. The one commit that reaches `main` without a pull request is the release commit the release workflow pushes (`docs/releasing.md`).

- Branch from `main` with a short descriptive name; in a stack, each layer above the bottom branches from the layer below.
- Agents act on GitHub with the owner's account through `gh`: open pull requests, push, comment, reply, resolve threads and perform merges the owner asked for. `robotogi[bot]` only posts reviews: the review record and the `review` check, through `just bot`. Set `ROBOTOGI_KEY_FILE` to the App's PEM private key file path in the environment.
- Plan the pull requests while designing the implementation, before writing code. A pull request is one merge unit: a change the owner would accept or revert whole, usually one issue or one entry in a design's Pull requests list. Review scales to a large diff by splitting it among reviewers, so size alone is no reason to split a merge unit. Commits carry the structure inside it: a behavior-preserving refactor in its own commit apart from the change it enables, a new seam apart from the behavior built on it.
- Stack only when a lower layer is a merge unit of its own, worth merging even if the layers above never land; every fix to a lower layer restacks and re-reviews the layers above it. Stacked pull requests always use [`gh stack`](https://github.com/github/gh-stack) (`gh extension install github/gh-stack`): each layer is a branch with its own pull request based on the layer below, a lower layer holds what the ones above depend on, and every layer passes `just check` on its own. Open the stack with `gh stack submit`, keep it current with `gh stack sync`, and merge it with `gh stack merge`, never layer by layer by hand.
- Commit with short imperative messages.
- The pull request body follows `.github/pull_request_template.md`: a short summary, and the demo in a collapsed block.
- Opening a pull request starts its independent agent review before its author reports done. Follow `.omp/commands/review-pr.md` (the omp command is `/review-pr [PR numbers or URLs… | all]`; no argument means all open non-draft pull requests). omp's `.omp/rules/review-after-open.md` sends a reminder after each PR-opening command. PR coordinators run in parallel and split each diff among parallel reviewers; one coordinator owns each stack's fixes and restack. Fixes, later pushes and restacks are reviewed only where they change the pull request, by the agents that reviewed it when they are still available. Record every finding and its outcome in one App comment for the reviewed commit, covering the whole diff the first time and only the changes afterwards, then post the `review` check on that head. The merge requires that check from robotogi and resolved review threads, together with CI.
  If a legacy version-1 record's historical base cannot be recovered, review the whole current layer conservatively and explain the exception in the linked record.
- Address every finding on the same branch. Fix real defects: behavior, safety, security, journal integrity, concurrency, unmet Acceptance, tests that fail to pin behavior, and docs that contradict behavior or other docs. Reply with a reason to the rest and change nothing; style, wording, optional tests, naming taste, optional refactors and findings another layer handles are not reasons to change this layer.
  Push one review's fixes together, and reply to and resolve each thread.
- A pull request that finishes an issue says `Closes #N` in its body; one that only makes progress says `Refs #N`.
- A pull request that bumps `journal.Schema` or `tuner.Ruleset` is breaking: its title starts with `[BREAKING]`, it carries the `breaking` label, and its changelog line starts with `**BREAKING**`.
- A pull request that fixes a bug that changed decisions adds a defect entry, with a test replaying a fixture journal from before the fix, when the affected decisions can be matched. Otherwise its changelog line tells the operator which `togi reset --core` to run.

## Issues

Planning lives in issues, filed from the templates in `.github/ISSUE_TEMPLATE/`.

- Labels name the kind: `idea` (a thought, not yet discussed), `design` (decided, waiting to be scheduled), `feature` (ready to build), `bugfix`, and `breaking` on issues and pull requests alike.
- The `1.0` milestone holds what ships in 1.0.
- Lifecycle: an idea is discussed until decided, then its issue becomes a design with Why, Decided, Acceptance, Open, Pull requests and Links; Pull requests lists the planned pull requests in landing order. When a discussion settles decisions, file or update the issue before it ends. Each pull request that implements part of a design moves its decisions into the spec or an ADR; the last one closes the issue. The spec and ADRs stay the lasting record.

## Keeping docs current

A pull request updates everything that describes the old state, in the same pull request:
- `--help` text for every command or flag it adds or changes;
- the README's Status when what works changes, and its Usage when the common commands change;
- `CHANGELOG.md` under `## [Unreleased]`, for every change a user of togi would notice: commands, flags, behavior, output, configuration. One line per change under `Added`, `Changed`, `Fixed` or `Removed`, stating the effect and linking the pull request. A `**BREAKING**` line starts with what the operator must do or will see, then the mechanism. Refactors, tests and doc edits that leave the tool unchanged get no entry;
- the specs, and any comment the change makes wrong.

The first pull request that makes something runnable on real hardware adds `docs/howto.md` with the operator's steps. Later pull requests that change those steps update it.

## Writing

- Write every file as if the repository were public: no personal hostnames, home paths, or setup specific to one machine or tool. Where there are several ways to get somewhere, name them, then continue as if the reader got there.
- Write commits, pull requests and docs for readers who have not seen the conversation that produced them.
- Never overstate: claim only what the demo or the checks showed.
- Describe workflow steps by the action (open a pull request, set the milestone), so they hold whatever program performs them. Name the repository's own commands.

## Commands

Enter the dev shell with `nix develop`, or with `direnv allow` once per checkout if you use direnv. Recipes also work outside the dev shell: they enter it with `nix develop` when needed.

| Command | Use |
|---|---|
| `just` | List the recipes |
| `just bot <gh args>` | Post review records and `review` checks as robotogi, using the private key file named by `ROBOTOGI_KEY_FILE` |
| `just test` | The tight loop |
| `just gate` | Lint, the `fmt` flake check over tracked files, then tests: the quick check before handing off |
| `just check` | Every flake check, what CI runs on every pull request and push to `main`: package (its tests run shuffled, with the integration tests on Linux), race (trial, session, journal and watch under the race detector), lint, fmt and, on Linux, the VM tests `vm` (the tuning boot) and `vm-restart-limit`. Must pass before a pull request |
| `just fmt` | Format Go, Nix and the justfile in place |
| `just sim [seed]` | A simulated session through its first clean guard rotation in a temporary state directory (`go run ./tools/sim`, `docs/simulating.md`) |
| `just bench [flags]` | The bench suite of simulated sessions, optionally compared against a baseline run (`go run ./tools/bench`, `docs/benchmarking.md`) |
| `just facts STATE-DIR` | Regenerate the committed privacy-safe target evidence from a copied state directory (`docs/benchmarking.md`) |
| `just fit [flags]` | Regenerate the target-machine fit and eight bootstrap refits from the committed extract (`docs/benchmarking.md`) |
| `just release` | Start the release workflow on `main`: it checks that `check` passed on `main`, commits the release, builds the package, pushes to `main` and publishes. `just release-preview` shows what it would release. See `docs/releasing.md` |
| `just hardware` | Hardware tests, on the target machine only: as root, or as a user with an explicitly delegated host lock ([provisioning](docs/howto.md#host-lock-and-delegated-hardware-tests)), read-write access to `/sys/kernel/ryzen_smu_drv/{rsmu_cmd,smu_args,smn}` and a delegated cpuset controller. Backend package paths come from `TOGI_MPRIME` and `TOGI_YCRUNCHER`, else from `/etc/togi/config.toml` |
| `just fuzz [time]` | Fuzz the journal parser |
| `just cover [base]` | Changed lines since `base` that no test reaches, as `path:first-last` ranges, for review; without a base, coverage per function for the whole repository. Evidence for reviewers, never a target |

CI (`.github/workflows/check.yml`) runs the Linux flake checks on GitHub-hosted `ubuntu-latest` runners: an `eval` job lists the checks, one job per check builds it, with `/dev/kvm` opened to the Nix build users for the VM tests. A native `macos-15` arm64 job builds `checks.aarch64-darwin.package`, running the Darwin tests without VM, integration or hardware checks. The aggregating `check` job requires both the Linux checks and the macOS job to pass. Each VM test is its own check so the jobs boot their machines in parallel. The build jobs substitute from the public Cachix cache `togi` and push each check's output when the `CACHIX_AUTH_TOKEN` secret is available, so a check whose inputs are unchanged passes without running; the checks build the package with the revision `dev` so that a new commit alone changes none of them (ADR 0021). The release workflow (`.github/workflows/release.yml`) only builds the package on the release commit.

The Linux `module` check evaluates GRUB mirror constraints, the grubenv mount dependency and package overrides. `trial-scope-tests` builds the trial package's `hardware`-tagged test binary; `vm` runs its scope tests as root under real systemd, without stress backends or SMU access.

On macOS (aarch64-darwin) the dev shell, `just test`, `just gate`, `just sim` and the read-only commands work; `just check` builds `package`, `race`, `lint` and `fmt` and skips the VM tests, `just hardware` and the `integration` tests are Linux-only. Linux-only code follows the Go convention: OS-suffixed files (`_linux.go`, `_darwin.go`) for real implementations, and a `//go:build !linux` fallback returning a wrapped `errors.ErrUnsupported`.

A command needed twice gets a recipe, in the same pull request.

## Layout

| Package | Owns |
|---|---|
| `cmd/togi` | Command dispatch |
| `internal/config` | Configuration |
| `internal/machine` | Shared vocabulary and the seam interfaces the run loop consumes |
| `internal/defect` | Known decision-changing bugs and pure matching against the journal |
| `internal/journal` | Journal, replay, state file, log lines |
| `internal/facts` | Decisive trial and idle-failure evidence with journal provenance |
| `internal/carry` | Transitions: archiving an older session and deriving the edges and failed marks it carries |
| `internal/tuner` | Pure decision engine: search, hunt, refinement, guard |
| `internal/sim` | Simulator implementing every hardware seam, and resuming it after a journal |
| `internal/session` | The run loop: session start, resume, crash attribution, trials, dead ends |
| `internal/simrun` | A session on the simulator, across its crash reboots |
| `internal/smu` | `ryzen_smu`: the only package that writes offsets |
| `internal/trial` | Containment, sampling, load-step signaling |
| `internal/watch` | The read-only dashboard: journal projection, frame rendering and the redraw loop |
| `internal/backend/*` | mprime and y-cruncher integrations |
| `internal/hardware` | Assembles the real machine: host, preflight, GRUB |
| `internal/detect` | Kernel log, MCE, crash detection |
| `nix/` | NixOS module and VM tests |
| `tools/*` | Development programs, never shipped: `bench`, `carry-facts`, `cover`, `facts`, `fit`, `release`, `sim`, `stats`; shared evaluation packages `modelcheck` and `trialfacts`. Development and debugging behavior lives here, never in `cmd/togi` |

A package owns one responsibility, and its exported API is the seam. Split a package when it holds two responsibilities that change for different reasons.

## Code

- **Traceable:** every action and decision is a journal event with a `msg` a stranger can follow (`journal.md`). Verbose is the goal: someone reading only the journal can reconstruct what togi did and why.
- **Safe writes:** offsets reach hardware only through `internal/smu`, clamped to [-50, 0], with an intent event before and a readback after.
- Comments only where names cannot carry the reason.
- Wrap errors with the operation that failed.

## Testing

- **Fast and deterministic:** a unit test exercises logic, never the world around it. It does not wait on real time, reach the network, start processes or depend on the machine it runs on: time comes from an injected clock or a `testing/synctest` bubble, everything else from fakes. Keep each test as quick as the behavior it proves allows.
- **Simulator first:** behavior is proven on `internal/sim` with fixed seeds, never by waiting for hardware.
- Tests pin spec behavior: rules, boundaries, invariants, crash-resume. Table tests for rules, property tests for invariants, golden files for rendered output (`go test ./cmd/togi -update` rewrites them), a fuzz target for the journal parser (`just fuzz`). Compare values with `cmp.Diff`.
- Concurrent code is tested on real goroutines. The `race` flake check runs trial, session, journal and watch under the race detector on every pull request and push to `main`; `just test -race` runs the whole suite locally.
- Tests that need the real world carry a build tag and stay out of `go test ./...`: `integration` for real processes (a helper program built by the test, never mprime or y-cruncher), `hardware` for the target machine, restoring every offset they change.
- **Coverage is evidence, not a target** (ADRs 0030, 0032): no threshold, ratchet or tracked percentage. `just cover BASE` shows reviewers changed lines no test reaches. A test exists to pin behavior, never only to reach a line.
