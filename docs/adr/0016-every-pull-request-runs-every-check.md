# Every pull request runs every flake check; the release trusts `main`

Pull requests ran only `just ci`: lint, the formatting check and the Go tests. The Nix builds, the NixOS VM test and the race detector first ran in the release workflow ([ADR 0014](0014-release-from-a-workflow.md)), so a stale `vendorHash`, a flake or module regression, or a data race surfaced mid-release, after the change had merged.

## Decision

- The flake's `checks` are every gate for their system: `package` (the tests, with the integration tests on Linux), `lint`, `fmt` (treefmt with `--ci` over the tracked Go, Nix and justfile sources), `race` (the tests under the race detector) and, on Linux, `vm`. The VM test uses a copy of the package with `doCheck = false`, so it does not wait on the tests `package` already runs.
- The `check` workflow runs on every pull request and push to `main`. An `eval` job evaluates the flake and lists `checks.x86_64-linux`; one job per check builds it in parallel; a `check` job passes only when all of them passed. A newer push to the same ref cancels the older run.
- The release workflow refuses unless the `check` run for `main` HEAD succeeded, then builds only `packages.x86_64-linux.default` on the release commit before pushing it.
- `govulncheck` and `just vuln` are removed. Dependabot version updates for the flake inputs and the GitHub Actions run weekly; Go modules stay on Dependabot security updates.

## Considered Options

- **VM test and race detector only at release:** rejected because breakage surfaced mid-release, after it had merged.
- **Fast checks on pull requests, slow ones on push to `main`:** rejected because breakage could still land on `main`.
- **A single `nix flake check` job:** rejected because the checks share the runner's 2–4 vCPUs, where separate jobs each get their own.
- **`govulncheck` in CI:** rejected because it fails on advisories unrelated to the change. Dependabot alerts cover the modules, and standard library fixes arrive with the Go toolchain through `flake.lock`.
- **A scheduled tier:** rejected because the inputs are pinned; nothing changes between pushes.
- **Go build caching:** rejected because the Nix sandbox keeps no build cache between builds.
- **Running every check again on the release commit:** rejected because the release commit changes only `version.txt` and `CHANGELOG.md` on top of a commit that passed.

## Consequences

- Every pull request spends Actions minutes on the VM test and the race build, billed while the repository is private.
- Merging only on a green `check` is a convention until the repository is public and `check` becomes a required status.
- A Dependabot Go-module pull request needs a follow-up commit updating `vendorHash`.
- The release commit gets no `check` run of its own, since the workflow pushes it with the automatic token; its package build runs the tests on it.
