# Publish as togi from a fresh repository

The project goes public before 1.0. The name shycler said nothing about what the tool does, and the early history named private hosts and infrastructure. Both are cheapest to change once, before anyone depends on the repository.

## Decision

- The name is **togi** (研ぎ), Japanese for polishing a blade. A sword polisher brings out what is already in the steel, coarse stones first, then fine ones; togi finds each core's edge the same way, in steps of 5, then 1.
- The name changes everywhere at once: module path, command, flake output names, NixOS options (`services.togi`), systemd units, the state and configuration paths, trial scopes, the GRUB entry and the `TOGI_*` variables. Earlier ADRs and changelog entries keep the old name, as records of their time.
- `journal.Schema` and `tuner.Ruleset` stay: no recorded event changes shape or meaning, so a session continues after the operator moves `/var/lib/shycler` to `/var/lib/togi`. The change is still breaking, because every existing configuration must rename its options; the changelog and `docs/howto.md` give the steps.
- The history was rewritten once: private host and infrastructure names in early docs became generic terms, and every committer became the author, except the release bot. The rewritten history lives in a new repository, `github.com/shgew/togi`. Issues kept their numbers, and each earlier pull request's number is held by a closed archive issue with its description and merge commit. The previous repository stays private as an archive.

## Considered Options

- **Force-push the rewritten history and rename the repository:** rejected because GitHub keeps `refs/pull/*` read-only, so the pull requests would keep the old commits reachable once the repository is public. GitHub Support removes such commits only for credentials and similar sensitive data.
- **No rewrite:** rejected because the early docs named a private machine and private infrastructure.
- **Keep `services.shycler` as an alias of `services.togi`:** rejected because there is one known installation, and a one-time rename in the changelog is simpler than an alias to carry and later remove.
- **Rename only the repository, module path and command:** rejected because installed names saying shycler would outlive the name everywhere else.

## Consequences

- Commit hashes changed: builds and pins from before the move name commits that exist only in the private archive. The release tags point at the rewritten commits.
- Links to `github.com/shgew/shycler` in old commits and old issue text lead to the private archive.
- Pull requests from before the move survive only as archive issues, without review threads or per-commit history.
