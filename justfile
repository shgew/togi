set shell := ["bash", "-euo", "pipefail", "-c"]
set positional-arguments

dev := if env("TOGI_DEV_SHELL", "") == "1" { "" } else { "nix develop --command" }
system := arch() + "-" + replace(os(), "macos", "darwin")

# List recipes by group in file order
_default:
    @{{ just_executable() }} --justfile '{{ justfile() }}' --list --unsorted

# Run the Go test suite with optional test flags
[group('test')]
test *args:
    {{ dev }} go test -shuffle=on ./... "$@"

# Run one package (`just focus ./internal/tuner`) or test pattern (`just focus TestGuard/crash`)
[group('test')]
focus +args:
    if [[ "$1" == ./* || "$1" == ../* || "$1" == *... || -d "$1" ]]; then {{ dev }} go test "$@"; else pattern="$1"; shift; {{ dev }} go test -run "$pattern" ./... "$@"; fi

# Run the hardware tests on the target machine (Linux only)
[group('test')]
hardware *args:
    {{ dev }} go test -tags hardware -p 1 ./... "$@"

# Fuzz the journal parser for the given time (`just fuzz 5m`); failures land in internal/journal/testdata/fuzz
[group('test')]
fuzz time="1m":
    {{ dev }} go test -run '^$' -fuzz '^FuzzParse$' -fuzztime "$1" ./internal/journal

# Print code no test reaches: per function for the whole repo, or changed lines since a base (`just cover origin/main`)
[group('test')]
cover base="":
    #!/usr/bin/env bash
    set -euo pipefail
    profile=$(mktemp)
    trap 'rm -f "$profile"' EXIT
    {{ dev }} go test -shuffle=on {{ if os() == "linux" { "-tags integration" } else { "" } }} -coverprofile="$profile" ./... >&2
    if [[ -z "$1" ]]; then
        {{ dev }} go tool cover -func="$profile"
        exit
    fi
    {{ dev }} go run ./tools/cover --profile "$profile" --base "$1"

# Lint all Go packages with optional lint flags
[group('quality')]
lint *args:
    {{ dev }} golangci-lint run ./... "$@"

# Format Go, Nix and this justfile in place
[group('quality')]
fmt:
    nix fmt

# Pre-handoff gate: lint, the fmt flake check (tracked files), then tests
[group('quality')]
gate: lint (check-one "fmt") test

# Run every flake check CI runs: package, race, lint, fmt and, on Linux, the VM tests
[group('nix')]
check *args:
    nix flake check "$@"

# Build named flake checks: package, race, lint, fmt or, on Linux, vm and vm-restart-limit (`just check-one race`)
[group('nix')]
check-one +names:
    nix build --no-link $(printf '.#checks.{{ system }}.%s ' "$@")

# Run a simulated session through the search and its first clean qualifying rotation
[group('run')]
sim seed="1":
    {{ dev }} go run ./tools/sim --seed "$1"

# Play a recorded journal through the dashboard on a fast-forward clock
[group('run')]
replay *args:
    {{ dev }} go run ./tools/replay "$@"

# Summarize a current or archived journal for review
[group('run')]
stats *args:
    {{ dev }} go run ./tools/stats "$@"

# Benchmark simulated session conclusions and compare against a baseline
[group('run')]
bench *args:
    {{ dev }} go run ./tools/bench "$@"

# Prove simulated session decisions are unchanged from a base revision
[group('run')]
same base="origin/main":
    dir=$(mktemp -d); trap 'rm -rf "$dir"' EXIT; git archive "$1" | tar -x -C "$dir"; {{ dev }} go run ./tools/bench --same "$dir"

[group('run')]
bench-baseline:
    {{ dev }} go run ./tools/bench --split all --out tools/bench/baseline.jsonl

# Regenerate privacy-safe real facts from a copied state directory
[group('run')]
facts state_dir:
    {{ dev }} go run ./tools/facts "$1" tools/bench/facts/target.jsonl.gz

# Fit the target-machine bootstrap ensemble from the committed evidence
[group('run')]
fit *args:
    {{ dev }} go run ./tools/fit "$@"

# Run only the forward-chained check of the target fit, writing no machine files (`just forward --seal 1`)
[group('run')]
forward *args:
    {{ dev }} go run ./tools/fit --forward-only "$@"

# Start the release workflow on main and follow it: once check passed on main, it commits the release, builds the package, pushes to main and publishes
[group('release')]
release:
    url=$({{ dev }} gh workflow run release.yml --ref main); echo "$url"; {{ dev }} gh run watch "${url##*/}" --exit-status

# Print the release the release workflow would make from origin/main
[group('release')]
release-preview:
    {{ dev }} go run ./tools/release

# Check the changelog fragments in changes/ (the changes flake check)
[group('release')]
changes:
    {{ dev }} go run ./tools/release -check changes

# Run GitHub commands as robotogi
[group('github')]
bot +args:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "${TOGI_DEV_SHELL:-}" != "1" ]]; then
        exec {{ dev }} just --justfile '{{ justfile() }}' bot "$@"
    fi
    token=$(gh-token generate --app-id 5162510 --key "${ROBOTOGI_KEY_FILE:?ROBOTOGI_KEY_FILE must name the robotogi private key file}" --token-only)
    GH_TOKEN=$token exec gh "$@"
