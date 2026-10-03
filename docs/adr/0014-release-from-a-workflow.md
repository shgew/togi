Checks at release superseded by [ADR 0016](0016-every-pull-request-runs-every-check.md); reading entries from `[Unreleased]` superseded by [ADR 0033](0033-changelog-fragments.md), which assembles them from `changes/`. The release commit, push and publish remain.

# Releases are made by a workflow started by hand

The repository moved from Forgejo to GitHub. [ADR 0012](0012-release-by-push.md) opened the release pull request by pushing to Forgejo's `refs/for/main`, and GitHub has no push that opens a pull request. On GitHub, a pull request opened by a workflow with the automatic token gets no `check` run, and opening one from the owner's machine needs a client and its login.

## Decision

`just release` starts the `release` workflow on `main` with `gh workflow run` and follows the run. The workflow:

1. commits `Release x.y.z` on top of `origin/main`, updating `version.txt` and `CHANGELOG.md` together (`go run ./tools/release -commit`), and fails when `[Unreleased]` is empty;
2. runs `just check --race` and `just vuln` on that commit;
3. pushes it to `main` as a fast-forward, so the run fails if `main` moved since it started;
4. creates the release and its `v<version>` tag on that commit, with the version's changelog section as the notes (`go run ./tools/release -publish`).

The release commit lands on `main` without a pull request: starting the workflow is the owner's approval, and `just release-preview` shows the version, the reason and the notes beforehand. If a run pushes but fails to publish, the next run finds a dated version without a tag and publishes it without a new commit.

`version.txt` stays the version source, because flake builds cannot see tags. Bumps stay computed from the changelog, and the workflow takes no inputs.

## Considered Options

- **A release pull request opened by the workflow:** rejected because a pull request opened with the automatic token gets no `check` run.
- **A release pull request opened from the owner's machine with `gh pr create`, published on merge:** rejected as two steps where one run tests exactly the commit it publishes.
- **A `version` input to override the bump:** rejected because the changelog is the single source of the bump.

## Consequences

Releasing needs permission to start workflows. Release commits are authored by `github-actions[bot]` and are the only commits pushed to `main` without a pull request. A branch rule that requires pull requests on `main` must let the workflow push. On a personal-account repository the automatic token cannot bypass a ruleset, so the workflow pushes with a write deploy key, stored as the `RELEASE_DEPLOY_KEY` secret, and the `main` ruleset lets deploy keys bypass it.
