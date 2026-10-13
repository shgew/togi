# Running the Rust migration unattended

Status: see [the index](README.md).

Note (2026-10-13): the dependency-update pause ended with stage 9 ([#919](https://github.com/shgew/togi/issues/919)). `.github/dependabot.yml` updates `cargo`, the `rust-toolchain.toml` pin, `nix` and `github-actions` weekly, with minor and patch updates grouped and a 7-day cooldown, and the `open-pull-requests-limit: 0` entries are gone. The pause rule and consequence below are the record of stages 2–8.

Partly supersedes [ADR 0048](0048-rust.md) and [ADR 0046](0046-macos-checks-the-package-on-main.md). On 2026-10-11 the owner decided that stages 2–7 of the migration ([#634](https://github.com/shgew/togi/issues/634)) run without the owner present, and stages 9–12 after the cutover is accepted, and set the Rust toolchain. Before stage 2 started, a two-reviewer plan council reviewed the epic and its stage issues; its findings were applied to the issue bodies, each marked "(orchestrator, 2026-10-11, plan council)" ([summary](https://github.com/shgew/togi/issues/634#issuecomment-6108195762)). Three of these decisions change earlier ADRs. ADR 0048 had every change to a protected comparison asset approved by the owner, and counted 252 suite sessions. ADR 0046 had the macOS job build only the Go package. The full rules are in #634 (**Running unattended**) and [#637](https://github.com/shgew/togi/issues/637); [`docs/porting.md`](../porting.md) holds what porting pull requests follow.

## Decision

**Protected comparison assets change by rule, not by an owner approval for each change.** This replaces ADR 0048's rule that they "change only in pull requests the owner approves".

- **The manifest.** Stage 2 commits a hash manifest of the protected assets: the Go reference, golden files, fixture journals, the suite, machine and facts inputs, the oracles' inputs, `just same`, the CI comparison jobs, the guard's files and CI's decision path (the check discovery and the `check` aggregate in `.github/workflows/check.yml`). Entries are only ever appended.
- **The additive rule.** Comparison work goes in new files and new flake checks: a new oracle, a new comparison tool, new checks, new recipes. A pull request that adds them appends its own manifest entries. An edit, deletion or rename of a listed file, or a change to an existing manifest entry, is not additive. The reviewer confirms in a required checklist item that every existing check, golden and fixture is unchanged, still runs and is not weakened.
- **Oracles freeze in two steps.** What Go itself produces (journals, answers, frames, transcripts, vectors) is protected the day it lands. Values an oracle decomposes for the Rust design may still change under review, with the no-weakening checklist item, until their first consumer merges; then they are protected too.
- **The guard runs from the base.** A `pull_request_target` workflow checks out the base, reads the head only as data, and posts a required `guard` status on the head. It fails when a listed file or an existing manifest entry changes, when the guard's own files change, when a flake check the base has is missing from the head, or when CI's decision path changes, unless the `owner-approved` label is set. Only the owner applies that label; agents never apply or remove it. After stage 2, new CI coverage arrives only as new flake checks.
- **Bootstrap.** Stage 2 creates the comparison harness, then the manifest and the guard, under the agent merge rule, before any crate lands.
- **What stays the owner's:** a protected change that is not additive, stage 9's removals and stage 11's re-records.

**The suite is every session of `tools/bench/suite.json`:** the dev and holdout seeds of every scenario, 220 today, not 252 as ADR 0048 says. The count is read from the file. The suite is never shrunk or grown to fit a count, and a count written in prose is corrected to the file.

**On `main`, the macOS job also builds the Rust workspace and runs its tests.** This partly supersedes ADR 0046, whose macOS job builds only `checks.aarch64-darwin.package`. A failure there is fixed as a normal bug. The macOS job still runs only on pushes to `main`. Every pull request runs `cargo clippy --target aarch64-apple-darwin` on Linux.

**The unattended run** follows these rules:

- **Merging.** An agent merges a pull request when robotogi's `review` check, the `guard` status and CI pass on the head and every review thread is resolved. The cutover merge ([#643](https://github.com/shgew/togi/issues/643)) and any protected change that is not additive are the owner's. Dependent work starts from merged `main` once the Linux jobs of its `check` pass. When a Linux job of `main`'s `check` fails, merges stop until those jobs are green again. The first step is a revert of the breaking merge, reviewed and merged under the normal rules, and the cause is then fixed in a new pull request.
- **Parking.** Blocked work gets `needs-decision` and a comment with the question. The run continues on everything unblocked and stops only when nothing unblocked remains. A pull request whose same finding is still unresolved after three fix rounds is parked the same way. Parked work never counts as done.
- **The stop list.** A safety, security or journal-durability defect of the kinds REVIEW.md lists stops the stage with `needs-decision` when it is in merged Rust code or when Go has it too. Rust must then either reproduce the defect or depart from the reference, and only the owner can choose. Such a finding in an open pull request's review is fixed in that pull request, recorded in [`docs/rust-migration.md`](../rust-migration.md), and parks the pull request after three rounds.
- **Go is the reference.** Go is frozen as the reference behavior. Rust reproduces a Go bug byte for byte, a bugfix issue is filed for after the cutover, and the case is logged in `docs/rust-migration.md`.
- **The heavy-job cap.** At most 2 heavy jobs (cargo build or nextest, VM tests, bench) run at once on a host, until stage 2's measurements set the cap. `just ship`'s own lock stays.
- **CI shards.** A comparison job over the limit of [#627](https://github.com/shgew/togi/issues/627) is sharded, and the layout is measured on a GitHub runner before the job is added. The limit is the median duration of the slowest Linux job over the last ten `main` runs when stage 2 measures. If no layout fits, only the CI job is parked for the owner's budget decision. The run continues, and every pull request the comparison reaches runs it in full locally, with its output in the review snapshot. Comparisons are never sampled or dropped. The one exception is rebuilding the per-prefix Go reference of [#638](https://github.com/shgew/togi/issues/638), once per Go revision, which may exceed the limit.
- **Dependency updates pause.** Dependabot's `nix` and `github-actions` updates would change the Go toolchain under the reference mid-run. Stage 2 sets their `open-pull-requests-limit` to 0, and stage 9 restores them beside the cargo entries. No dependency-update pull request merges during stages 2–8.

## Considered Options

- **An owner approval for each protected addition,** as ADR 0048 had it: rejected. With the owner away, every new oracle, check or manifest entry would park the work that needs it, yet an addition cannot weaken an existing comparison. The guard keeps the owner's approval for the changes that can weaken one: edits, deletions, renames, changed manifest entries, a dropped flake check, a changed decision path.
- **A protection check inside the pull request's own flake or workflow:** rejected. CI's job list comes from the pull request's own flake and workflow, so a pull request could unregister or rewire the checks that judge it. The guard therefore runs from the base.
- **Parking the run on CI budget:** rejected. An earlier plan halted the run when no shard layout fit #627's limit. Parking only the CI job and running the comparison locally on every pull request it reaches keeps the comparison complete while the owner decides the budget.
- **Sampling the comparison to fit the CI budget:** rejected. A sampled comparison misses the sessions it skips, and ADR 0048's contract is byte identity on every suite session.

## Consequences

- Agents merge additive comparison work without the owner. The owner's approvals are the `owner-approved` label on non-additive protected changes, the cutover merge, stage 9's removals and stage 11's re-records.
- `guard` is a required status, and `just land` waits for it. A pull request that changes a protected asset fails it until the owner applies the label.
- A Go bug found during the port lives on in Rust until the cutover, unless it is on the stop list. Its fix is filed for after the cutover.
- The flake inputs and GitHub Actions stay at their stage 2 versions until stage 9.
- If the comparison's CI job is parked, the full comparison runs on the development host for every pull request it reaches, under the heavy-job cap.
- ADR 0048's other decisions stand. ADR 0046's other decisions stand too: the macOS job runs only on pushes to `main`, and every Linux check runs on every pull request.
