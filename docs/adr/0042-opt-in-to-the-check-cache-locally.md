# Opt in to the check cache locally

[ADR 0021](0021-cache-check-outputs-on-cachix.md) has CI push each check's output to the public Cachix cache `togi` and decided that "the cache serves CI only: nothing configures it for users, the dev shell or local `just check`". Local `just check` therefore rebuilds every check, VM tests included on Linux, although CI already holds the output of every check whose inputs are unchanged. Nix substitutes from a binary cache only when the machine's own configuration trusts it: a flake's `nixConfig` asks for permission on each command (`accept-flake-config = ask`) and is ignored for users who are not trusted users ([#404](https://github.com/shgew/togi/issues/404)).

## Decision

- The README's Development section names the cache, `https://togi.cachix.org`, and its public key, `togi.cachix.org-1:1EZ2zQlDkNhHZmROZzR0n0/CcYGLPfAmPs+VaGL72LU=`, and the two ways to trust it: `extra-substituters` and `extra-trusted-public-keys` in the system Nix configuration, or `cachix use togi` as a trusted user. Local `just check` on a machine that opted in downloads every check whose inputs are unchanged.
- The flake still sets no `nixConfig`.

This amends ADR 0021's sentence that nothing configures the cache for local `just check`; the dev shell, users installing togi and CI's pushes are unchanged.

## Considered Options

- **Keep the cache CI-only:** rejected. ADR 0021 judged local `just check` rare enough to rebuild, but it runs before every push to a pull request head, review fixes included (`AGENTS.md`), and a rebuild repeats work CI has already done.
- **Configure the cache in the flake's `nixConfig`:** rejected for ADR 0021's reason: it prompts anyone running nix commands on the flake, and it does nothing for users who are not trusted users, so it would still need the same documented step.

## Consequences

- A machine that opted in passes an unchanged check without running it, as CI does: no new `-shuffle=on` seed and no VM boot for that check. `just test` and `just gate` still run the tests locally.
- Trusting the key trusts every path the cache serves, including any output a pull request run holding the push token wrote under a check's store path (ADR 0021). The trust is per machine and the reader's choice.
- A machine that has not opted in behaves as before and rebuilds every check.
