set shell := ["bash", "-euo", "pipefail", "-c"]
set positional-arguments

dev := if env("SHYCLER_DEV_SHELL", "") == "1" { "" } else { "nix develop --command" }
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
    {{ dev }} go test -tags hardware ./... "$@"

# Fuzz the journal parser for the given time (`just fuzz 5m`); failures land in internal/journal/testdata/fuzz
[group('test')]
fuzz time="1m":
    {{ dev }} go test -run '^$' -fuzz '^FuzzParse$' -fuzztime "$1" ./internal/journal

# Lint all Go packages with optional lint flags
[group('quality')]
lint *args:
    {{ dev }} golangci-lint run ./... "$@"

# Report known vulnerabilities in the dependencies and standard library code this module calls
[group('quality')]
vuln:
    {{ dev }} govulncheck ./...

# Format Go, Nix and this justfile in place
[group('quality')]
fmt:
    nix fmt
    {{ just_executable() }} --justfile '{{ justfile() }}' --fmt

# Check formatting of Go, Nix and this justfile without changing files
[group('quality')]
_fmt-check:
    unformatted=$({{ dev }} gofmt -l .); if [[ -n "$unformatted" ]]; then printf 'not gofmt-formatted:\n%s\n' "$unformatted"; exit 1; fi
    {{ dev }} nixfmt --check flake.nix nix/*.nix
    {{ just_executable() }} --justfile '{{ justfile() }}' --fmt --check

# Pre-handoff gate: lint, formatting check, then tests
[group('quality')]
gate: lint _fmt-check test

# The pull request checks CI runs: lint, formatting check, then the tests, with the integration tests on Linux
[group('quality')]
ci: lint _fmt-check
    {{ dev }} go test -shuffle=on {{ if os() == "linux" { "-tags integration" } else { "" } }} ./...

# Run every flake check; `--race` also runs the Go tests under the race detector, as the release workflow does
[group('nix')]
check *args:
    race=0; flags=(); for a in "$@"; do if [[ "$a" == --race ]]; then race=1; else flags+=("$a"); fi; done; nix flake check "${flags[@]}"; if (( race )); then nix build --no-link .#legacyPackages.{{ system }}.race; fi

# Build named flake checks: package, lint or, on Linux, vm (`just check-one vm`)
[group('nix')]
check-one +names:
    nix build --no-link $(printf '.#checks.{{ system }}.%s ' "$@")

# Run a simulated session through its first clean guard rotation in temporary state; Bronze needs no depth left to regain
[group('run')]
sim seed="1":
    {{ dev }} go run ./tools/sim --seed "$1"

# Start the release workflow on main and follow it: it commits the release, runs the checks, pushes to main and publishes
[group('release')]
release:
    url=$({{ dev }} gh workflow run release.yml --ref main); echo "$url"; {{ dev }} gh run watch "${url##*/}" --exit-status

# Print the release the release workflow would make from origin/main
[group('release')]
release-preview:
    {{ dev }} go run ./tools/release
