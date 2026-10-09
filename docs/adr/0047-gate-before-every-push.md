# Gate before every push

Status: Accepted.

Agents ran `just gate`, then `just check` from a cold cache before every push, so the suite ran about five times per push between the local ladder and CI. See [#492](https://github.com/shgew/togi/issues/492) (A5) and [#498](https://github.com/shgew/togi/issues/498).

## Decision

`just gate` runs before every push. `just check` is required only for changes to `flake.nix` or `nix/`, and, on a Mac, for changes to `_darwin.go` or `!linux` files ([ADR 0046](0046-macos-checks-the-package-on-main.md)). CI stays the definition of green: it runs every Linux flake check on the exact head of every pull request ([ADR 0016](0016-every-pull-request-runs-every-check.md), with the macOS package check on `main` only since ADR 0046), and `just check` stays the full set ([ADR 0036](0036-flake-checks-stay-hermetic.md)).

This removes the premise of [ADR 0042](0042-opt-in-to-the-check-cache-locally.md) that local `just check` runs before every push; the opt-in cache remains useful for the pushes that need it. It amends [ADR 0045](0045-the-tight-loop-reuses-cached-test-results.md): `just gate` runs before every push rather than every commit; the unshuffled tight loop stays.

## Considered Options

- **Keep `just check` before every push:** rejected. CI runs the same checks on the same head; the hermetic checks matter locally only when the flake or the Nix sources change.

## Consequences

A defect that only a VM test would catch can reach CI before a local run; CI blocks the merge. A darwin-only defect still first shows on `main` (ADR 0046).
