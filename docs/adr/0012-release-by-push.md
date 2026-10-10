Status: see [the index](README.md).

# Releases open with a push and publish from CI

A release changes `version.txt` and `CHANGELOG.md`, so it lands as a pull request like any change. The first release tool opened that pull request through the Forgejo API and published only when run a second time after the merge. Every run needed a personal token, and the second run was easy to forget.

## Decision

`just release` opens the release pull request with a plain `git push` to Forgejo's `refs/for/main` ref (its [AGit workflow](https://forgejo.org/docs/latest/user/git-cli/agit-support/)), using the pusher's git credentials. The push carries the fixed topic `release`, so while a release pull request is open Forgejo rejects the next push, and the tool refuses instead of updating or replacing it. A person pushes the commit, so `check` runs on the pull request.

Merging publishes. The final step of the CI `check` job, run only on a push to `main` and only once the checks before it pass, creates the release with the workflow's automatic token when `version.txt` names a version with a dated changelog section and no tag. It is a step rather than a separate job so pull requests do not carry a skipped `publish` status. It targets the first-parent commit that last changed `version.txt`, so a publish retried on a later push still tags the release pull request's merge commit.

`version.txt` stays the version source, because flake builds cannot see tags. Bumps stay computed from the changelog.

## Considered Options

- **GoReleaser:** rejected because togi ships no binaries (flake users build from a tag), and GoReleaser neither creates tags, bumps versions nor opens pull requests; it would only replace the single call that creates the release.
- **Opening the pull request from a workflow started by hand:** rejected because changes made with the automatic token trigger no workflows, so the release pull request would get no `check` run.
- **Opening the pull request with a Forgejo client (`fj`) or the API and a personal token:** rejected because a push needs neither a token nor another tool in the dev shell, and a client from a fork would be built from source on every CI run.
- **A standing release pull request refreshed on every push to `main`:** rejected because the owner decides when to release.
- **Rebuilding an open release pull request on the next run:** rejected as handling for a rare case; a re-push cannot change the pull request's title when the version changes, and closing the open one by hand is cheap.

## Consequences

Releasing needs only push access. The release pull request has no branch behind it; its head lives at `refs/pull/<n>/head`. A release pull request opened by mistake is closed by hand. Publishing depends on CI: if it fails, re-running the job or the next push to `main` publishes.
