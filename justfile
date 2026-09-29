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
    {{ dev }} go test -tags hardware ./... "$@"

# Fuzz the journal parser for the given time (`just fuzz 5m`); failures land in internal/journal/testdata/fuzz
[group('test')]
fuzz time="1m":
    {{ dev }} go test -run '^$' -fuzz '^FuzzParse$' -fuzztime "$1" ./internal/journal

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

# Run every flake check CI runs: package, lint, fmt and, on Linux, the VM test
[group('nix')]
check *args:
    nix flake check "$@"

# Build named flake checks: package, lint, fmt or, on Linux, vm (`just check-one vm`)
[group('nix')]
check-one +names:
    nix build --no-link $(printf '.#checks.{{ system }}.%s ' "$@")

# Run a simulated session through the search and its first clean qualifying rotation
[group('run')]
sim seed="1":
    {{ dev }} go run ./tools/sim --seed "$1"

# Start the release workflow on main and follow it: once check passed on main, it commits the release, builds the package, pushes to main and publishes
[group('release')]
release:
    url=$({{ dev }} gh workflow run release.yml --ref main); echo "$url"; {{ dev }} gh run watch "${url##*/}" --exit-status

# Print the release the release workflow would make from origin/main
[group('release')]
release-preview:
    {{ dev }} go run ./tools/release
