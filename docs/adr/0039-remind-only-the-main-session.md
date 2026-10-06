# Remind only the main session

[ADR 0025](0025-recorded-agent-review.md) made `.omp/rules/review-after-open.md` watch PR-opening bash commands for every agent except `reviewer`. Review coordinators pushing fix stacks with `gh stack submit` and task subagents opening pull requests were then told to start another review. [#389](https://github.com/shgew/togi/issues/389) limits the reminder to the main session.

## Decision

`.omp/rules/review-after-open.md` applies only to the main, top-level omp session: its frontmatter is `agents: main`. Subagents, review coordinators and task agents included, get no reminder after `gh pr create` or `gh stack submit`. The main session that delegated the work starts the review of pull requests its subagents open.

Repetition stays as in ADR 0025: `.omp/config.yml` sets TTSR to `after-gap` with a zero gap.

## Considered Options

- **Keep `agents: "!reviewer"`:** rejected. Coordinators pushing fix stacks with `gh stack submit` and task agents opening pull requests were told to start another review ([#389](https://github.com/shgew/togi/issues/389)).
- **Exclude each subagent definition by name:** rejected. omp matches `agents` against the agent definition name, and a subagent with no definition name falls back to `sub`, so every new agent definition and every unnamed subagent would still get the reminder until the list was extended. `main` is reserved for the top-level session and no agent definition can take it, so `agents: main` cannot match a subagent.

## Consequences

A subagent that opens a pull request relies on the main session that delegated the work to start its review. Review coordinators no longer get a reminder while pushing fixes to the pull requests they review.
