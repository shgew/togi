# Recorded agent review

Passing checks prove that a pull request builds and passes its tests, not that anyone read its diff. Reviews ran locally without a lasting record on many pull requests. Agent actions also used the owner's account. [#257](https://github.com/shgew/togi/issues/257) separates those identities and makes review a merge gate.

## Decision

The owner merges. Agents merge only when the owner asks. The GitHub App `robotogi` (App ID 5162510), installed only on this repository, is the agents' GitHub identity. It has write access to checks, contents, issues and pull requests. `just bot <gh args>` mints an installation token from the PEM private key in `ROBOTOGI_KEY` and runs `gh` with it. The App ID is public; keys and key locations are not repository content.

Every pull request gets independent agent review after it opens. Its author follows `.omp/commands/review-pr.md` (`/review-pr [PR numbers or URLs… | all]` in omp; no argument means all open non-draft pull requests). One parallel coordinator owns each PR, or a whole stack to review layers in parallel, fix bottom-up and restack and push once. Each coordinator freezes the head and diff, lists included and excluded files, and splits included hunks among parallel bundled `reviewer` agents by directory with tests beside their code. Fix diffs get the same split. `.omp/rules/review.md` always applies to that agent alone. It loads the tool-neutral criteria and recurring Lessons in `REVIEW.md`; CodeRabbit reads the same file through its code guidelines.

Opening a pull request starts this review before its author reports done. `.omp/rules/review-after-open.md` watches PR-opening bash commands for every agent except `reviewer`. Its non-interrupting reminder arrives after the command runs, then directs the author to the review flow for the opened PRs. Repetition is a session setting, not rule metadata: `.omp/config.yml` sets TTSR to `after-gap` with a zero gap so every opening call is eligible. This repeat setting also applies to other TTSR rules in repository sessions.

Findings follow `AGENTS.md`: fix real defects and reply with reasons to the rest. Fix commits get their own independent review. While CodeRabbit is installed, handle both inline and review-body findings. [Trial mode never assigns seats to bots](https://docs.coderabbit.ai/management/seat-assignment.md). An admin assigns `robotogi[bot]` a seat in CodeRabbit's Team Management, where bots are listed separately. Once seated, CodeRabbit reviews its PRs automatically on open and each push. Only PRs opened before assignment need one owner `@coderabbitai review`, because assignment does not start review retroactively. Agents never post as the owner. Wait for the head review unless CodeRabbit explicitly skips (missing seat, rate limit or pause); the record links that nonblocking reason.

The record is one App comment per reviewed head commit. It names the commit, covered files and reviewers, lists every finding with priority and outcome, and states the verdict. `REVIEW.md` defines the human format and version-1 JSON in the final hidden `togi-review` block so reporting tools can parse it. No per-pull-request record is committed to the repository.

After recording the review, the App posts a completed check run named `review` on that exact head with a summary linking the comment. Success requires an outcome for every finding and no open P0/P1; deferring such a finding does not clear it. After this change merges, the owner configures the `main` ruleset to require `review` from robotogi and resolved review threads, alongside CI. A new push needs a new check. A pure rebase carries review forward only when `git range-diff` pairs every patch in the layer unchanged; the new head gets a check linking the earlier record, without another review comment.

When the same kind of finding occurs in two pull requests, the pull request fixing the second adds a cited lesson to `REVIEW.md`. Design and feature issues gain Acceptance criteria. The ADR index records which decisions still apply; specs describe current behavior. `just reviews` will report review coverage and findings over merged pull requests in a separate change in #257.

## Considered Options

- **Commit records to the repository:** rejected. The record commit changes the commit it certifies and adds per-pull-request churn.
- **Replace omp's bundled reviewer:** rejected. A project agent with the same name silently replaces it whole and stops receiving upstream improvements. A scoped rule adds repository criteria without replacement.
- **Run the reviewer in GitHub Actions:** rejected. The owner merges, so local review is enough. Hosted review adds API keys and per-pull-request cost.
- **Require CodeRabbit's own status:** rejected. Its rate limits would block merges. Its trial is still being evaluated; the App's record captures its review or explicit skip reason.
- **Require GitHub approvals:** rejected. Before the App, agents opened pull requests under the owner's account, and the owner could not approve those pull requests. An App-authored record and check cover findings and reviewed commits instead of relying on an approval.

## Consequences

A green gate has a public record of who reviewed which commit and what happened to each finding. It is evidence of review, not a guarantee that no defect remains. The implementing agent still triages findings and creates the check; the owner controls merges and the ruleset. Keys stay local, installation tokens expire, and no workflow needs model credentials.

CodeRabbit's future remains open. Before its trial ends, agent reviewers will replay its previously unrecorded serious findings at their pre-fix commits. Misses will inform new lessons or another reviewer model, rather than making CodeRabbit's limits a permanent merge dependency.
