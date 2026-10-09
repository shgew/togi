# Every pull request runs every flake check; the release trusts `main`

Status: the race-detector decision and its consequences are superseded by [ADR 0022](0022-run-the-race-detector-in-ci.md); the rejection of slow checks on push to `main` is superseded for the macOS job by [ADR 0046](0046-macos-checks-the-package-on-main.md); the other decisions remain in force.

Pull requests ran only `just ci`: lint, the formatting check and the Go tests. The Nix builds, the NixOS VM test and the race detector first ran in the release workflow ([ADR 0014](0014-release-from-a-workflow.md)), so a stale `vendorHash`, a flake or module regression, or a data race surfaced mid-release, after the change had merged.

## Decision

- The flake's `checks` are every gate for their system: `package` (the tests, with the integration tests on Linux), `lint`, `fmt` (treefmt with `--ci` over the tracked Go, Nix and justfile sources) and, on Linux, `vm`. The VM test uses a copy of the package with `doCheck = false`, so it does not wait on the tests `package` already runs.
- The race detector leaves the automated checks; `just test -race` runs it locally.
- The `check` workflow runs on every pull request and push to `main`. An `eval` job evaluates the flake and lists `checks.x86_64-linux`; one job per check builds it in parallel; a `check` job passes only when all of them passed. A newer push to the same ref cancels the older run.
- The release workflow refuses unless the `check` run for `main` HEAD succeeded, then builds only `packages.x86_64-linux.default` on the release commit before pushing it.
- `govulncheck` and `just vuln` are removed. Dependabot version updates for the flake inputs and the GitHub Actions run weekly; Go modules stay on Dependabot security updates.

## Considered Options

- **VM test only at release:** rejected because breakage surfaced mid-release, after it had merged.
- **The race detector as a flake check on every pull request:** rejected because its job took 8m00s on the first run, against 3m41s for the VM test, the next slowest, and so doubled every run.
- **Fast checks on pull requests, slow ones on push to `main`:** rejected because breakage could still land on `main`.
- **A single `nix flake check` job:** rejected because the checks share the runner's 2–4 vCPUs, where separate jobs each get their own.
- **A Nix store cache per check (`nix-community/cache-nix-action`):** rejected because restoring it took about a minute and the `vm` job was no faster: 4m07s with it on a changed source, against 3m41s and 4m05s without.
- **`govulncheck` in CI:** rejected because it fails on advisories unrelated to the change. Dependabot alerts cover the modules, and standard library fixes arrive with the Go toolchain through `flake.lock`.
- **A scheduled tier:** rejected because the inputs are pinned; nothing changes between pushes.
- **Go build caching:** rejected because the Nix sandbox keeps no build cache between builds.
- **Running every check again on the release commit:** rejected because the release commit changes only `version.txt` and `CHANGELOG.md` on top of a commit that passed.

## Consequences

- Every pull request spends Actions minutes on the VM test, billed while the repository is private.
- A data race is caught only when someone runs `just test -race`.
- Merging only on a green `check` is a convention until the repository is public and `check` becomes a required status.
- A Dependabot Go-module pull request needs a follow-up commit updating `vendorHash`.
- The release commit gets no `check` run of its own, since the workflow pushes it with the automatic token; its package build runs the tests on it.
- Dependabot does not support the `nix` ecosystem in private repositories, so until the repository is public `flake.lock`, and with it the Go toolchain, is updated by hand.
