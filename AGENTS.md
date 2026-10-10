# togi

Go CLI that finds per-core Curve Optimizer offsets on Zen 5 desktop CPUs and keeps testing them. Runs on NixOS; development also works on macOS.

## Docs

- `README.md`: what togi does, what works today, and the common commands.
- `GLOSSARY.md`: the vocabulary. Name code, events and docs with its terms.
- `docs/how-togi-tunes.md`: read first to understand the tuning sequence, phase costs and ruleset history; the specs own the rules.
- `REVIEW.md`: defect criteria, recurring lessons and the review record. Read it before reviewing a pull request.
- `.omp/`: reviewer rules, the `review-coordinator` agent and `/review-pr`, the omp command for reviewing pull requests in parallel and recording their gates.
- `docs/spec/`: normative behavior (`tuner.md`, `workloads.md`, `journal.md`, `runtime.md`). Read the relevant spec before changing behavior, and change spec and code in the same pull request or in layers of one stack merged together.
- `docs/issues.md`: labels, lifecycle, `Touches:`, claiming and the target-machine queue. Read it before filing, triaging or claiming an issue.
- `docs/adr/`: decisions and the alternatives rejected, with statuses in [the index](docs/adr/README.md). Where a spec disagrees with an ADR, the spec wins. Reversing a decision needs a new ADR, which updates the index in the same pull request.
- `docs/prior-art.md`: before proposing a feature, check whether it was deliberately left out.
- `docs/porting.md` and `docs/rust-migration.md`: the Go-to-Rust porting guide (rule, idioms, byte hazards, reviewer checklist) and the migration log; read the guide before any porting work, and append what your pull request measured to the log ([ADR 0048](docs/adr/0048-rust.md)).
- `docs/benchmarking.md`: run it before and after any change to how the tuner decides; it also covers `just forecast`.
- `docs/simulating.md`, `docs/reviewing.md`, `docs/releasing.md`, `CHANGELOG.md` and `changes/README.md`: read when you do what they name.

## Workflow

Every change, docs included, lands as a pull request against `main` on `github.com/shgew/togi`, or as a layer of a stack of pull requests that ends on `main`. The owner controls what reaches `main`: the owner merges, and an agent merges only a pull request the owner has explicitly told it to merge. When a change is done, open its pull request without asking, unless told otherwise. The one commit that reaches `main` without a pull request is the release commit the release workflow pushes (`docs/releasing.md`).

- Claim the issue first (`docs/issues.md`): `just claim` assigns it and posts the start comment; then create the branch from `main`, with a short descriptive name, in your own worktree. In a stack, each layer above the bottom branches from the layer below: pass that layer's branch as BASE.
- Agents act on GitHub with the owner's account through `gh`: open pull requests, push, comment, reply, resolve threads, and merge when the owner explicitly permits it. `robotogi[bot]` only posts reviews: the review record and the `review` check, through `just bot`. Set `ROBOTOGI_KEY_FILE` to the App's PEM private key file path in the environment.
- Plan the pull requests while designing the implementation, before writing code. A pull request is one merge unit: a change the owner would accept or revert whole, usually one issue or one entry in a design's Pull requests list. Review scales to a large diff by splitting it among reviewers, so size alone is no reason to split a merge unit. Commits carry the structure inside it: a behavior-preserving refactor in its own commit apart from the change it enables, a new seam apart from the behavior built on it.
- Stack only when a lower layer is a merge unit of its own, worth merging even if the layers above never land; every fix to a lower layer restacks and re-reviews the layers above it. Stacked pull requests always use [`gh stack`](https://github.com/github/gh-stack) (`gh extension install github/gh-stack`): each layer is a branch with its own pull request based on the layer below, a lower layer holds what the ones above depend on, and every layer passes `just gate` on its own. Open the stack with `gh stack submit`, keep it current with `gh stack sync`, and merge it with `gh stack merge`, never layer by layer by hand.
- Commit with short imperative messages.
- The pull request body follows `.github/pull_request_template.md`: a short summary, and the demo in a collapsed block.
- Opening a pull request starts its independent agent review before its author reports done. `.omp/commands/review-pr.md` says how the review runs (the omp command is `/review-pr [PR numbers or URLs… | all]`; no argument means all open non-draft pull requests). omp's `.omp/rules/review-after-open.md` sends the main session a reminder after each PR-opening command it runs; subagents, review coordinators included, get none. The ruleset enforces the merge gate: robotogi's `review` check on the head, resolved review threads and CI.
- Address every finding on the same branch. Fix real defects: behavior, safety, security, journal integrity, concurrency, unmet Acceptance, tests that fail to pin behavior, and docs that contradict behavior or other docs. Reply with a reason to the rest and change nothing; style, wording, optional tests, naming taste, optional refactors and findings another layer handles are not reasons to change this layer.
- A pull request that finishes an issue says `Closes #N` in its body; one that only makes progress says `Refs #N`.
- A pull request whose Acceptance needs a run on the target machine says `Refs #N` and merges once everything else is green; its issue keeps `needs-hardware` (`docs/issues.md`, ADR 0043).
- A pull request that bumps `journal.Schema` or `tuner.Ruleset` is breaking: its title starts with `[BREAKING]`, it carries the `breaking` label, and its changelog line starts with `**BREAKING**`.
- A pull request that fixes a bug that changed decisions adds a defect entry, with a test replaying a fixture journal from before the fix, when the affected decisions can be matched. Otherwise its changelog line tells the operator which `togi reset --core` to run.

## Keeping docs current

A pull request updates everything that describes the old state, in the same pull request:
- `--help` text for every command or flag it adds or changes;
- the README's Status when what works changes, and its Usage when the common commands change;
- a changelog fragment, `changes/<N>.md` named by the pull request's number and added once it is opened, for every change a user of togi would notice: commands, flags, behavior, output, configuration. One line per change under `### Added`, `### Changed`, `### Removed` or `### Fixed`, stating the effect and ending with a period; the release adds the pull request link (ADR 0033). A `**BREAKING**` line starts with what the operator must do or will see, then the mechanism. A change to something not yet released edits that change's fragment instead. Refactors, tests and doc edits that leave the tool unchanged get no fragment;
- the specs, and any comment the change makes wrong.
- `docs/how-togi-tunes.md` when tuner behavior changes.

Pull requests that change the operator's steps update `docs/howto.md`.

## Writing

- **Public:** this repository and its GitHub pages are public and permanent, history and pull request refs included: files, commits, branches, issues, comments, pull requests, reviews and releases. Publish what is true of togi and the hardware it runs on, never who or where is behind it: no personal details, whereabouts or routines, no credentials, no names, addresses or paths of private machines, networks or accounts, no identifier unique to one machine, nothing from other work or private projects. Machine output stays complete as evidence; replace only what identifies a person, place or machine, the same way each time. Reread the final text before it leaves the machine; when something ruled out here seems necessary, leave it out or ask the owner.
- **Portable:** write for any reader's setup, nothing specific to one machine or tool. Where there are several ways to get somewhere, name them, then continue as if the reader got there.
- Write commits, pull requests and docs for readers who have not seen the conversation that produced them.
- Each rule has one owner, the spec. README, howto, ADRs and tool docs link to it instead of restating it.
- Never overstate: claim only what the demo or the checks showed.
- Describe workflow steps by the action (open a pull request, set the milestone), so they hold whatever program performs them. Name the repository's own commands.

## Commands

Enter the dev shell with `nix develop`, or with `direnv allow` once per checkout if you use direnv. Recipes that run Go or `gh` stop with that hint outside it; `just hardware` enters it itself, since `sudo` drops the environment. The shell sets `GOTOOLCHAIN=local`, so `go` uses the flake's pinned toolchain and fails instead of downloading another one when `go.mod` asks for a newer Go. `just --list` lists the recipes by group with their purpose; a command needed twice gets a recipe, in the same pull request.

The check ladder: `just test` or `just focus` while editing, unshuffled so unchanged packages reuse cached results (ADR 0045); `just gate` before every push, every non-VM flake check sequentially with warm Go caches; `just check`, every flake check the host builds, only for changes to `flake.nix` or `nix/`, and, on a Mac, for changes to `_darwin.go` or `!linux` files. Agents sharing one host run `just gate`, `just check` and benchmarks under `flock` on one lock file shared by the agents on that host, so parallel sessions queue instead of overloading the machine. `just ship` formats, runs `just gate` under a lock that every worktree of the clone shares and pushes the current branch, force-pushing with lease after a rebase and refusing on a `gh stack` layer; run `just check` before it when the ladder asks for one. `just land N` waits for `check` and `review` on pull request N's head and for every review thread to be resolved, then reports it ready for the owner; neither merges. CI is the definition of green (ADR 0036); its jobs are explained in `.github/workflows/check.yml`, the VM tests in `nix/vm-test.nix` and `nix/module-test.nix`. `just hardware` runs on the target machine only ([provisioning](docs/howto.md#host-lock-and-delegated-hardware-tests)).

Development works on x86_64-linux and macOS (aarch64-darwin); hardware runs and the NixOS VM tests need Linux, and CI runs what macOS cannot. Pull requests get no macOS feedback: when a change touches `_darwin.go` or `!linux` files, run `just check` on a Mac before review; otherwise a darwin breakage first shows as a failed `check` on `main`. gopls on macOS skips Linux-only files: `just lint` checks them, and `GOOS=linux` in gopls's environment shows them.

Linux-only code uses OS-suffixed files (`_linux.go`, `_darwin.go`) and a `//go:build !linux` fallback returning a wrapped `errors.ErrUnsupported`.

## Layout

Each package's purpose is its package doc comment (`go doc ./internal/<name>`). Two invariants hold across packages:

- `internal/smu` is the only package that writes offsets.
- `tools/` holds development programs and is never shipped; development and debugging behavior lives there, never in `cmd/togi`.

A package owns one responsibility, and its exported API is the seam. Split a package when it holds two responsibilities that change for different reasons.

## Code

- **Traceable:** every action and decision is a journal event with a `msg` a stranger can follow (`journal.md`). Verbose is the goal: someone reading only the journal can reconstruct what togi did and why.
- **Safe writes:** offsets reach hardware only through `internal/smu`, clamped to [-50, 0], with an intent event before and a readback after.
- Comments only where names cannot carry the reason.
- Wrap errors with the operation that failed.

## Testing

- **Fast and deterministic:** a unit test exercises logic, never the world around it. It does not wait on real time, reach the network, start processes or depend on the machine it runs on: time comes from an injected clock or a `testing/synctest` bubble, everything else from fakes. Keep each test as quick as the behavior it proves allows.
- **Simulator first:** behavior is proven on `internal/sim` with fixed seeds, never by waiting for hardware.
- Tests pin spec behavior: rules, boundaries, invariants, crash-resume. Table tests for rules, property tests for invariants, golden files for rendered output (`-update` rewrites them: `cmd/togi`, `internal/watch`, `tools/bench`, `tools/board`, `tools/fit` and `tools/stats` have them), a fuzz target for the journal parser (`just fuzz`). Compare values with `cmp.Diff`.
- Concurrent code is tested on real goroutines. The `race` flake check runs trial, journal, watch, smu and `cmd/togi` under the race detector on every pull request and push to `main`; `just test -race` runs the whole suite locally.
- Tests that need the real world carry a build tag and stay out of `go test ./...`: `integration` for real processes (a helper program built by the test, never mprime or y-cruncher), `hardware` for the target machine, restoring every offset they change.
- **Coverage is evidence, not a target** (ADRs 0030, 0032): no threshold, ratchet or tracked percentage. `just cover BASE` shows reviewers changed lines no test reaches. A test exists to pin behavior, never only to reach a line.
