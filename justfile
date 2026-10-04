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

# Run one package (`just focus ./internal/tuner`) or test pattern (`just focus TestChecking/crash`)
[group('test')]
focus +args:
    if [[ "$1" == ./* || "$1" == ../* || "$1" == *... || -d "$1" ]]; then {{ dev }} go test "$@"; else pattern="$1"; shift; {{ dev }} go test -run "$pattern" ./... "$@"; fi

# Run the hardware tests on the target machine (Linux only)
[group('test')]
hardware *args:
    @[[ "{{ os() }}" == linux ]] || { echo 'just hardware needs Linux: run it on the target machine' >&2; exit 1; }
    {{ dev }} env TMPDIR=/tmp go test -tags hardware -p 1 ./... "$@"

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
    {{ dev }} go test -shuffle=on -tags integration -coverprofile="$profile" ./... >&2
    if [[ -z "$1" ]]; then
        {{ dev }} go tool cover -func="$profile"
        exit
    fi
    {{ dev }} go run ./tools/cover --profile "$profile" --base "$1" --tags integration

# Lint all Go packages as built for Linux and for macOS, with optional lint flags
[group('quality')]
lint *args:
    for target in linux/amd64 darwin/arm64; do {{ dev }} env GOOS="${target%/*}" GOARCH="${target#*/}" CGO_ENABLED=0 golangci-lint run ./... "$@"; done

# Format Go, Nix and this justfile in place
[group('quality')]
fmt:
    nix fmt

# Run every non-VM flake check, cheapest first, using warm dev-shell Go caches
[group('quality')]
gate:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "${TOGI_DEV_SHELL:-}" != "1" ]]; then
        exec {{ dev }} just --justfile '{{ justfile() }}' gate
    fi
    just check-one fmt
    just lint
    just check-one module changes
    vendored=$(nix build --no-link --print-out-paths '.#checks.{{ system }}.package.goModules')
    fresh=$(mktemp -d)
    trap 'rm -rf "$fresh"' EXIT
    go mod vendor -o "$fresh/vendor"
    diff -rq "$fresh/vendor" "$vendored" >&2 || { echo 'gate: the Go modules vendored for vendorHash in flake.nix differ from go.mod and go.sum; update vendorHash' >&2; exit 1; }
    go test -shuffle=on -tags integration ./...
    go test -race -shuffle=on -tags integration ./internal/trial ./internal/session ./internal/journal ./internal/watch
    if [[ "{{ os() }}" == linux ]]; then
        go test -c -tags hardware -o /dev/null ./internal/trial
    fi

# Run every flake check this host builds: package, race, lint, fmt, changes, module and, on Linux, trial-scope-tests and the VM tests
[group('nix')]
check *args:
    nix flake check "$@"

# Build named flake checks: package, race, lint, fmt, changes, module or, on Linux, trial-scope-tests, vm and vm-restart-limit (`just check-one race`)
[group('nix')]
check-one +names:
    nix build --no-link $(printf '.#checks.{{ system }}.%s ' "$@")

# Run a simulated session through the search and its first clean cycle
[group('run')]
sim seed="1" *args:
    {{ dev }} go run ./tools/sim --seed "$1" "${@:2}"

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

# Sweep every simulated scenario and audit its retained journals, or audit given state directories
[group('run')]
audit *args:
    {{ dev }} go run ./tools/audit "$@"

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
