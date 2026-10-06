# togi

Go CLI that finds per-core Curve Optimizer offsets on Zen 5 desktop CPUs and keeps testing them. Runs on NixOS; development also works on macOS.

## Docs

- `README.md`: what togi does, what works today, and the common commands. The first page a reader sees.
- `CHANGELOG.md`: released user-visible changes, in [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format. Pending entries wait in `changes/`, one file per pull request; `changes/README.md` has the format.
- `GLOSSARY.md`: the vocabulary. Name code, events and docs with its terms.
- `REVIEW.md`: defect criteria, recurring lessons and the review record. Read it before reviewing a pull request.
- `.omp/`: reviewer rules, the `review-coordinator` agent and `/review-pr`, the omp command for reviewing pull requests in parallel and recording their gates.
- `docs/spec/`: normative behavior. Read the relevant spec before changing behavior, and change spec and code in the same pull request or in layers of one stack merged together.
  - `tuner.md`: offsets, search, hunt, deepening, checking, clean cycles, dead ends.
  - `workloads.md`: regimes, backends, containment, failure detection.
  - `journal.md`: events, state, logging.
  - `runtime.md`: commands, preflight, configuration, tuning boot, NixOS module.
- `docs/releasing.md`: versioning rules, the release workflow and publishing.
- `docs/simulating.md`: running a simulated session with `tools/sim`.
- `docs/reviewing.md`: reviewing an unattended run or archived journal with `just stats`, and writing its retro on the pinned "Target-machine runs" issue.
- `docs/benchmarking.md`: measuring a tuner change's time to conclusion, depth and hazard across simulated machines with `tools/bench`; run it before and after any change to how the tuner decides.
- `tools/bench/adversary.md`: autonomous adversarial search for simulated machines, consistent with the real evidence, that the tuner gets wrong; follow it when hunting for new bench scenarios.
- `tools/bench/program.md`: autonomous tuner research; follow it when planning experiments, keeping bench wins and turning them into pull requests.
- `tools/fit/program.md`: autonomous simulator model research; follow it when changing how the simulator models failures or how `tools/fit` fits them.
- `docs/adr/`: decisions and the alternatives rejected, with statuses in [the index](docs/adr/README.md). The specs describe current behavior; where they disagree with an ADR, the spec wins and the index is out of date. Reversing a decision needs a new ADR, which updates the index in the same pull request.
- `docs/prior-art.md`: before proposing a feature, check whether it was deliberately left out.
- Issues on `github.com/shgew/togi`: the plan, ideas, bugs and who works on what (Issues, below).

## Workflow

Every change, docs included, lands as a pull request against `main` on `github.com/shgew/togi`, or as a layer of a stack of pull requests that ends on `main`. The owner merges; agents merge only when the owner asks. When a change is done, open its pull request without asking, unless told otherwise. The one commit that reaches `main` without a pull request is the release commit the release workflow pushes (`docs/releasing.md`).

- Claim the issue first (Issues, below): `just claim` creates the branch from `main`, with a short descriptive name. In a stack, each layer above the bottom branches from the layer below: pass that layer's branch as BASE.
- Agents act on GitHub with the owner's account through `gh`: open pull requests, push, comment, reply, resolve threads and perform merges the owner asked for. `robotogi[bot]` only posts reviews: the review record and the `review` check, through `just bot`. Set `ROBOTOGI_KEY_FILE` to the App's PEM private key file path in the environment.
- Plan the pull requests while designing the implementation, before writing code. A pull request is one merge unit: a change the owner would accept or revert whole, usually one issue or one entry in a design's Pull requests list. Review scales to a large diff by splitting it among reviewers, so size alone is no reason to split a merge unit. Commits carry the structure inside it: a behavior-preserving refactor in its own commit apart from the change it enables, a new seam apart from the behavior built on it.
- Stack only when a lower layer is a merge unit of its own, worth merging even if the layers above never land; every fix to a lower layer restacks and re-reviews the layers above it. Stacked pull requests always use [`gh stack`](https://github.com/github/gh-stack) (`gh extension install github/gh-stack`): each layer is a branch with its own pull request based on the layer below, a lower layer holds what the ones above depend on, and every layer passes `just check` on its own. Open the stack with `gh stack submit`, keep it current with `gh stack sync`, and merge it with `gh stack merge`, never layer by layer by hand.
- Follow the agent check ladder: `just test` or `just focus` while editing; `just gate` before every commit or handoff; `just check` before every push to a pull request head, review fixes included.
- Commit with short imperative messages.
- The pull request body follows `.github/pull_request_template.md`: a short summary, and the demo in a collapsed block.
- Opening a pull request starts its independent agent review before its author reports done. `.omp/commands/review-pr.md` says how the review runs (the omp command is `/review-pr [PR numbers or URLs… | all]`; no argument means all open non-draft pull requests). omp's `.omp/rules/review-after-open.md` sends the main session a reminder after each PR-opening command it runs; subagents, review coordinators included, get none. The merge requires robotogi's `review` check on the head and resolved review threads, together with CI.
- Address every finding on the same branch. Fix real defects: behavior, safety, security, journal integrity, concurrency, unmet Acceptance, tests that fail to pin behavior, and docs that contradict behavior or other docs. Reply with a reason to the rest and change nothing; style, wording, optional tests, naming taste, optional refactors and findings another layer handles are not reasons to change this layer.
- A pull request that finishes an issue says `Closes #N` in its body; one that only makes progress says `Refs #N`.
- A pull request that bumps `journal.Schema` or `tuner.Ruleset` is breaking: its title starts with `[BREAKING]`, it carries the `breaking` label, and its changelog line starts with `**BREAKING**`.
- A pull request that fixes a bug that changed decisions adds a defect entry, with a test replaying a fixture journal from before the fix, when the affected decisions can be matched. Otherwise its changelog line tells the operator which `togi reset --core` to run.

## Issues

Planning and coordination live in issues, filed from the templates in `.github/ISSUE_TEMPLATE/`. Run `just board` before choosing work. togi takes no outside contributions (`CONTRIBUTING.md`): a GitHub interaction limit keeps issues, comments, reactions and pull requests to collaborators. GitHub lifts it after six months at most; when `just board` asks, renew it with `just lock-interactions`.

- **Labels.** Kind: `idea` (a thought, not yet discussed), `design` (decided), `feature` (behavior ready to build), `bugfix`, `research` (opened by an autonomous research run), and `breaking` on issues and pull requests alike. Priority: `P0`–`P3`, as their label descriptions define them; no priority label means triaged and deliberately not scheduled. `needs-triage`: nobody has triaged it yet. `needs-decision`: waiting on the owner, with the question in a comment. The `1.0` milestone holds what ships in 1.0.
- **Lifecycle:** an idea is discussed until decided, then its issue becomes a design with Why, Decided, Acceptance, Open, Pull requests, Links and a `Touches:` line; Pull requests lists the planned pull requests in landing order. When a discussion settles decisions, file or update the issue before it ends. Each pull request that implements part of a design moves its decisions into the spec or an ADR; the last one closes the issue. The spec and ADRs stay the lasting record.
- **Triage:** every template adds `needs-triage`. A triage pass removes it and either schedules the issue (kind, priority, block, `Touches:`) or leaves it an unscheduled idea. Triage runs after each target-machine retro and whenever `just board` lists untriaged issues.
- **Blocks** are parent issues titled `Block: …` whose sub-issues one agent can take together. GitHub's "blocked by" relation orders issues; a block's body holds only that order and its reasons, since GitHub shows which sub-issues are open and who holds them.
- **No work without an issue.** File one from a template, under its block, before writing code. Work found mid-task (a deferred finding, a split, a design that does not hold) becomes a sub-issue right away. Exceptions: fixes to an open pull request, whose pull request is the record, and the release commit.
- **`Touches:`** every ready issue carries one line, `Touches: path, path`, naming the packages or files its change edits. Paths overlap when equal or when one is a directory containing the other; `just board` lists ready issues that overlap work in progress. Check it before claiming.
- **Ready** means a `design`, `feature` or `bugfix` with a priority label, no open blocker, no assignee and no open sub-issue (an issue with open sub-issues is a container; its sub-issues are the work). An `idea` inside a block is claimed to write a design comment first; code waits until the owner approves it and relabels it `design`.
- **Claiming:** `just claim N BRANCH WORKTREE PLAN [BASE]` refuses an issue that is already assigned and shows who holds it; otherwise it assigns the issue to the account `gh` acts as, links BRANCH from BASE (default `main`; the layer below for an upper stack layer) with `gh issue develop` and posts the start comment naming the branch, the worktree and the plan. PLAN is one line. If linking or commenting fails, it removes the assignment again. Then fetch BRANCH and create your worktree at WORKTREE from it. Assigned means taken. A claim with no commit, pull request or comment for 24 hours is stale; another agent may take it over after saying so in a comment.
- **Updates** are comments on state changes only: started; blocked or waiting on the owner (add `needs-decision` and ask in the comment); scope changed; stopped unfinished (where it stopped and what is left, then unassign). Opening and merging a pull request show in the timeline through `Refs`/`Closes`. The body is always the current state; the comments are the history.
- **The next ruleset:** one pinned issue titled `Ruleset N` is always open. Its sub-issues are the changes that bump `tuner.Ruleset` and the default strategy settings that change decisions, so they share one bench gate, one baseline and one session transition: decided sub-issues, and `idea` candidates that are grilled or moved out before release. A retro action or kept research result that would change tuner decisions becomes one of its sub-issues. The owner decides when it ships. The pull request that bumps `tuner.Ruleset` closes it; whoever merges that pull request opens and pins `Ruleset N+1` and moves the unfinished sub-issues to it.

## Keeping docs current

A pull request updates everything that describes the old state, in the same pull request:
- `--help` text for every command or flag it adds or changes;
- the README's Status when what works changes, and its Usage when the common commands change;
- a changelog fragment, `changes/<N>.md` named by the pull request's number and added once it is opened, for every change a user of togi would notice: commands, flags, behavior, output, configuration. One line per change under `### Added`, `### Changed`, `### Removed` or `### Fixed`, stating the effect and ending with a period; the release adds the pull request link (ADR 0033). A `**BREAKING**` line starts with what the operator must do or will see, then the mechanism. A change to something not yet released edits that change's fragment instead. Refactors, tests and doc edits that leave the tool unchanged get no fragment;
- the specs, and any comment the change makes wrong.

Pull requests that change the operator's steps update `docs/howto.md`.

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
| `just board` | Who may post on the repository, what waits on the owner, untriaged issues, work in progress with its branches, pull requests and `Touches:`, ready work by priority and block, overlaps between ready and in-progress `Touches:`, and the open `Ruleset N` issue (`go run ./tools/board`) |
| `just claim N BRANCH WORKTREE PLAN [BASE]` | Claim issue N: refuse if it is assigned, else assign it, link BRANCH from BASE (default `main`) and post the start comment naming WORKTREE and PLAN |
| `just lock-interactions` | Limit issues, comments, reactions and pull requests to collaborators for six months, GitHub's longest interaction limit; run it when `just board` asks |
| `just bot <gh args>` | Post review records and `review` checks as robotogi, using the private key file named by `ROBOTOGI_KEY_FILE` |
| `just reviews` | The review track record of the merged pull requests: per pull request, robotogi's `review` check on its head, its findings by priority and outcome from the review records, and the time from opening to the first record; then totals (`go run ./tools/reviews`) |
| `just test` | The tight loop |
| `just gate` | Every non-VM flake check, sequentially, cheapest first: fmt, lint, module and changes, the Go modules vendored for `vendorHash` against `go.mod` and `go.sum`, shuffled integration-tagged tests, race, and, on Linux, the hardware-tagged trial test compile. Uses warm dev-shell Go caches |
| `just check` | Every flake check the host builds, CI's definition of green: package (shuffled integration-tagged tests), race (trial, session, journal and watch), lint (Linux and macOS), fmt, changes (changelog fragments), module (NixOS module evaluation) and, on Linux, `trial-scope-tests` and the VM tests `vm` (tuning boot) and `vm-restart-limit`. Must pass before every push to a pull request head, review fixes included; CI also runs the Linux-only checks for macOS authors |
| `just fmt` | Format Go, Nix and the justfile in place |
| `just sim [seed]` | A simulated session through its first clean cycle in a temporary state directory (`go run ./tools/sim`, `docs/simulating.md`) |
| `just replay --state-dir DIR` | Play a recorded journal through the dashboard on a fast-forward clock, or print one frame with `--at SEQ` (`go run ./tools/replay`, `docs/simulating.md`) |
| `just bench [flags]` | The bench suite of simulated sessions, optionally compared against a baseline run (`go run ./tools/bench`, `docs/benchmarking.md`) |
| `just audit [flags] [STATE-DIR...]` | Audit journal invariants; without state directories, sweep every bench scenario over 200 seeds and retain the evidence (`docs/benchmarking.md`) |
| `just same [base]` | Prove a shape-only change leaves all simulated session journals unchanged; base defaults to `origin/main` (`docs/benchmarking.md`) |
| `just facts STATE-DIR` | Regenerate the committed privacy-safe target evidence from a copied state directory (`docs/benchmarking.md`) |
| `just fit [flags]` | Regenerate the target-machine fit and eight bootstrap refits, then report the forward-chained check on later sessions (`docs/benchmarking.md`) |
| `just fit-shared-voltage [--facts EXTRACT --out DIRECTORY]` | Write the all-facts IN-SAMPLE shared-voltage anchor only if every eligible original-evidence group checks `ok`; not forward-validated (`docs/benchmarking.md`) |
| `just forward [--seal N]` | Only the forward-chained check, writing no machine files; `--seal N` leaves the newest N sessions unscored (`docs/benchmarking.md`) |
| `just release` | Start the release workflow on `main`: it waits up to an hour for `check` to pass on `main`, commits the release, builds the package, pushes to `main` and publishes. `just release-preview` shows what it would release, and `just changes` checks the changelog fragments. See `docs/releasing.md` |
| `just hardware` | Hardware tests, on the target machine only: as root, or as a user with an explicitly delegated host lock ([provisioning](docs/howto.md#host-lock-and-delegated-hardware-tests)), read-write access to `/sys/kernel/ryzen_smu_drv/{rsmu_cmd,smu_args,smn}` and a delegated cpuset controller. Backend package paths come from `TOGI_MPRIME` and `TOGI_YCRUNCHER`, else from `/etc/togi/config.toml` |
| `just fuzz [time]` | Fuzz the journal parser |
| `just cover [base]` | Changed lines since `base` that no test reaches, as `path:first-last` ranges, then the changed files the host's platform and build tags leave out, for review; without a base, coverage per function for the whole repository. Evidence for reviewers, never a target |

CI (`.github/workflows/check.yml`) runs the Linux flake checks on GitHub-hosted `ubuntu-26.04` runners: an `eval` job lists the checks, one job per check builds it, with `/dev/kvm` opened to the Nix build users for the VM tests. A native `macos-26` arm64 job builds every `aarch64-darwin` check. The aggregating `check` job requires both the Linux checks and the macOS job to pass. Each VM test is its own check so the jobs boot their machines in parallel. The build jobs substitute from the public Cachix cache `togi` and push each check's output when the `CACHIX_AUTH_TOKEN` secret is available, so a check whose inputs are unchanged passes without running; the checks build the package with the revision `dev` so that a new commit alone changes none of them (ADR 0021). The release workflow (`.github/workflows/release.yml`) only builds the package on the release commit.

The `module` check evaluates the x86_64-linux NixOS configuration on every host: GRUB mirror constraints, the grubenv mount dependency, package overrides and the tuning specialisation's spec'd `30s` watchdog. The tuning-boot VM's package excludes tools, the simulator, test files and testdata, so edits there reuse its cached image and result. `trial-scope-tests` adds the trial package's `hardware`-tagged scope tests and their helpers; `vm` runs those tests as root under real systemd, without stress backends or SMU access. Edits to those retained tests or helpers rebuild the VM. The VM nodes disable DHCP and override only their tuning specialisation's watchdog to `10s`; `vm` still freezes PID 1 and observes the real hardware watchdog reset.

`vm-restart-limit` covers systemd's restart-limit recovery with a stub package: `run` and `restart-limit` exit 2, while other invocations sleep. It checks three failed starts, the fallback leave reason when `restart-limit` itself fails, recovery to the normal generation, GRUB's saved entry clearing and host-lock ownership without depending on the Go package, so Go-only changes reuse its cached result.

Development works on x86_64-linux and on macOS (aarch64-darwin). Only hardware runs, `just hardware` and the NixOS VM tests need Linux: on macOS, every other recipe runs, `just check` builds every check but `trial-scope-tests`, `vm` and `vm-restart-limit`, and the Linux-only tests are left out. CI runs them; open a draft pull request for Linux feedback before review. gopls on macOS skips Linux-only files: `just lint` checks them, and gopls with `GOOS=linux` in its environment shows them in an editor. Linux-only code follows the Go convention: OS-suffixed files (`_linux.go`, `_darwin.go`) for real implementations, and a `//go:build !linux` fallback returning a wrapped `errors.ErrUnsupported`.

A command needed twice gets a recipe, in the same pull request.

## Layout

| Package | Owns |
|---|---|
| `cmd/togi` | Command dispatch |
| `internal/config` | Configuration |
| `internal/machine` | Shared vocabulary and the seam interfaces the run loop consumes |
| `internal/defect` | Known decision-changing bugs and pure matching against the journal |
| `internal/journal` | Journal, replay, state file |
| `internal/render` | Human rendering of events and diagnostics: escaping, line format, colour and the journald priority prefix |
| `internal/facts` | Decisive trial and idle-failure evidence with journal provenance |
| `internal/carry` | Transitions: archiving an older session and deriving the solo limits and failure points it carries |
| `internal/tuner` | Pure decision engine: search, hunt, deepening, checking |
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
| `tools/*` | Development programs, never shipped: `audit`, `bench`, `board`, `carry-facts`, `cover`, `facts`, `fit`, `release`, `replay`, `reviews`, `sim`, `stats`; shared evaluation packages `modelcheck` and `trialfacts`. Development and debugging behavior lives here, never in `cmd/togi` |

A package owns one responsibility, and its exported API is the seam. Split a package when it holds two responsibilities that change for different reasons.

## Code

- **Traceable:** every action and decision is a journal event with a `msg` a stranger can follow (`journal.md`). Verbose is the goal: someone reading only the journal can reconstruct what togi did and why.
- **Safe writes:** offsets reach hardware only through `internal/smu`, clamped to [-50, 0], with an intent event before and a readback after.
- Comments only where names cannot carry the reason.
- Wrap errors with the operation that failed.

## Testing

- **Fast and deterministic:** a unit test exercises logic, never the world around it. It does not wait on real time, reach the network, start processes or depend on the machine it runs on: time comes from an injected clock or a `testing/synctest` bubble, everything else from fakes. Keep each test as quick as the behavior it proves allows.
- **Simulator first:** behavior is proven on `internal/sim` with fixed seeds, never by waiting for hardware.
- Tests pin spec behavior: rules, boundaries, invariants, crash-resume. Table tests for rules, property tests for invariants, golden files for rendered output (`go test ./cmd/togi ./internal/watch -update` rewrites them), a fuzz target for the journal parser (`just fuzz`). Compare values with `cmp.Diff`.
- Concurrent code is tested on real goroutines. The `race` flake check runs trial, session, journal and watch under the race detector on every pull request and push to `main`; `just test -race` runs the whole suite locally.
- Tests that need the real world carry a build tag and stay out of `go test ./...`: `integration` for real processes (a helper program built by the test, never mprime or y-cruncher), `hardware` for the target machine, restoring every offset they change.
- **Coverage is evidence, not a target** (ADRs 0030, 0032): no threshold, ratchet or tracked percentage. `just cover BASE` shows reviewers changed lines no test reaches. A test exists to pin behavior, never only to reach a line.
