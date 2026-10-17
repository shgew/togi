# No Go in the repository

Status: see [the index](README.md).

After the Rust cutover ([ADR 0048](0048-rust.md)), four development programs stayed in Go under `tools/`: `release`, `reviews`, `cover` and `board`. [ADR 0048](0048-rust.md) expected them to be rewritten in nushell, and Go stayed in the dev shell, the flake's `go-tools` and `lint` checks, `go.mod` and Dependabot only for them. The owner decided on 2026-10-17 to remove Go from the repository entirely ([#630](https://github.com/shgew/togi/issues/630), [#631](https://github.com/shgew/togi/issues/631), [#632](https://github.com/shgew/togi/issues/632), [#633](https://github.com/shgew/togi/issues/633)). Rewriting every tool in nushell does not fit them all: `release` parses fragments and the changelog, edits a versioned file format, talks to an API and has tests that pin each refusal, which nushell's tooling handles poorly.

## Decision

Go leaves the repository. Each Go development program is rewritten by one rule:

- **A program with a parser, a versioned format or tests that pin refusals is a Rust `togi-tool-*` crate.** It follows the workspace's lints, error convention and test rules, and its checks run in `nextest` and `clippy` with the rest of the workspace.
- **A script that turns `gh` or `git` output into text is nushell**, as `tools/board` is.

By that rule `release` ([#630](https://github.com/shgew/togi/issues/630)), `reviews` ([#632](https://github.com/shgew/togi/issues/632)) and `cover` ([#633](https://github.com/shgew/togi/issues/633)) become Rust crates and `board` ([#631](https://github.com/shgew/togi/issues/631)) is nushell. A rewrite keeps the program's flags, output, exit codes and refusals, so its recipes and workflows change only the command that runs it. Once the last Go program is gone, `go.mod`, `go.sum`, the Go toolchain in the dev shell, the `go-tools` and Go `lint` checks, golangci-lint's configuration and the Go entries of Dependabot and treefmt go with it ([#1038](https://github.com/shgew/togi/issues/1038)).

## Considered Options

- **Nushell for every tool,** as [ADR 0048](0048-rust.md) expected: rejected for the programs the rule assigns to Rust. Nushell has no type checker for a changelog parser, and a refusal pinned only by an end-to-end script is tested more slowly and less precisely than a unit test against an in-memory fake.
- **Rust for every tool, `board` included:** rejected. `board` only reshapes `gh` JSON into a report; a crate would add a build and a test harness for what a pipeline expresses directly.
- **Keep the Go tools:** rejected. They keep a second toolchain, linter, formatter configuration and dependency feed in the repository for four small programs, and agents working on them follow a second set of conventions.

## Consequences

The development programs follow the same conventions as togi itself, and the dev shell, checks and dependency updates shrink to Rust, Nix and nushell once [#1038](https://github.com/shgew/togi/issues/1038) lands. A Rust tool's dependencies enter `Cargo.lock` and pass `cargo deny` like togi's own; a tool that talks to GitHub runs `gh api`, as `togi-tool-release` does, rather than linking an HTTP client and TLS library. Until the last rewrite lands, Go remains in the dev shell for the programs still in it.
