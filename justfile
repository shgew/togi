set shell := ["bash", "-euo", "pipefail", "-c"]
set positional-arguments

system := arch() + "-" + replace(os(), "macos", "darwin")

# List recipes by group in file order
_default:
    @{{ just_executable() }} --justfile '{{ justfile() }}' --list --unsorted

_dev-shell:
    @[[ "${TOGI_DEV_SHELL:-}" == 1 ]] || { echo 'this recipe needs the dev shell: run `nix develop`, or `direnv allow` once per checkout' >&2; exit 1; }

# Run the Go test suite with optional test flags
[group('test')]
test *args: _dev-shell
    go test ./... "$@"

# Run one package's tests with optional test flags (`just focus ./internal/tuner`, `just focus ./... -run TestChecking/crash`)
[group('test')]
focus package *args: _dev-shell
    go test "$@"

# Run the hardware tests on the target machine (Linux only), entering the dev shell itself because sudo drops it
[group('test')]
hardware *args:
    @[[ "{{ os() }}" == linux ]] || { echo 'just hardware needs Linux: run it on the target machine' >&2; exit 1; }
    nix develop --command env TMPDIR=/tmp go test -tags hardware -p 1 ./... "$@"

# Fuzz the journal parser for the given time (`just fuzz 5m`); failures land in internal/journal/testdata/fuzz
[group('test')]
fuzz time="1m": _dev-shell
    go test -run '^$' -fuzz '^FuzzParse$' -fuzztime "$1" ./internal/journal

# Print code no test reaches: per function for the whole repo, or changed lines since a base (`just cover origin/main`)
[group('test')]
cover base="": _dev-shell
    #!/usr/bin/env bash
    set -euo pipefail
    profile=$(mktemp)
    trap 'rm -f "$profile"' EXIT
    go test -shuffle=on -tags integration -coverprofile="$profile" ./... >&2
    if [[ -z "$1" ]]; then
        go tool cover -func="$profile"
        exit
    fi
    go run ./tools/cover --profile "$profile" --base "$1" --tags integration

# Lint all Go packages as built for Linux and for macOS, with optional lint flags
[group('quality')]
lint *args: _dev-shell
    for target in linux/amd64 darwin/arm64; do env GOOS="${target%/*}" GOARCH="${target#*/}" CGO_ENABLED=0 golangci-lint run ./... "$@"; done

# Format Go, Nix and this justfile in place
[group('quality')]
fmt:
    nix fmt

# Run every non-VM flake check except the sim-verify shards, which only CI and `just check` run, cheapest first, using warm dev-shell Go caches
[group('quality')]
gate: _dev-shell
    #!/usr/bin/env bash
    set -euo pipefail
    just check-one fmt
    just lint
    just check-one module changes
    vendored=$(nix build --no-link --print-out-paths '.#checks.{{ system }}.package.goModules')
    fresh=$(mktemp -d)
    trap 'rm -rf "$fresh"' EXIT
    go mod vendor -o "$fresh/vendor"
    diff -rq "$fresh/vendor" "$vendored" >&2 || { echo 'gate: the Go modules vendored for vendorHash in flake.nix differ from go.mod and go.sum; update vendorHash' >&2; exit 1; }
    go test -shuffle=on -tags integration ./...
    go test -race -shuffle=on -tags integration ./internal/trial ./internal/journal ./internal/watch ./internal/smu ./cmd/togi
    if [[ "{{ os() }}" == linux ]]; then
        go test -c -tags hardware -o /dev/null ./internal/trial
    fi

# Run every flake check this host builds: package, race, lint, fmt, changes, module and, on Linux, trial-scope-tests, the sim-verify shards and the VM tests
[group('nix')]
check *args:
    nix flake check "$@"

# Build named flake checks: package, race, lint, fmt, changes, module or, on Linux, trial-scope-tests, sim-verify-0 to sim-verify-3, vm and vm-restart-limit (`just check-one race`)
[group('nix')]
check-one +names:
    nix build --no-link $(printf '.#checks.{{ system }}.%s ' "$@")

# Run a simulated session through the search and its first clean cycle
[group('run')]
sim seed="1" *args: _dev-shell
    go run ./tools/sim --seed "$1" "${@:2}"

# Play a recorded journal through the dashboard on a fast-forward clock
[group('run')]
replay *args: _dev-shell
    go run ./tools/replay "$@"

# Summarize a current or archived journal for review
[group('run')]
stats *args: _dev-shell
    go run ./tools/stats "$@"

# Benchmark simulated session conclusions and compare against a baseline
[group('run')]
bench *args: _dev-shell
    go run ./tools/bench "$@"

# Sweep every simulated scenario and audit its retained journals, or audit given state directories
[group('run')]
audit *args: _dev-shell
    go run ./tools/audit "$@"

# Prove simulated session decisions are unchanged from a base revision; stops at the first difference (`--keep-going` counts them all)
[group('run')]
same base="origin/main" *args: _dev-shell
    dir=$(mktemp -d); trap 'rm -rf "$dir"' EXIT; git archive "$1" | tar -x -C "$dir"; go run ./tools/bench --same "$dir" "${@:2}"

# Prove a fixed selection of simulated sessions, one per scenario, is unchanged from a base revision; same cache and fail-fast as `same`
[group('run')]
smoke base="origin/main" *args: _dev-shell
    dir=$(mktemp -d); trap 'rm -rf "$dir"' EXIT; git archive "$1" | tar -x -C "$dir"; go run ./tools/bench --same "$dir" --smoke "${@:2}"

# Forecast a real run from a copy of its state directory with the target ensemble (`--out tools/bench/forecasts/<session>-<seq>.json`)
[group('run')]
forecast state_dir *args: _dev-shell
    go run ./tools/bench --forecast "$1" "${@:2}"

# Re-record the committed bench baseline over all seeds, without model checks, wall times or retired fields
[group('run')]
bench-baseline: _dev-shell
    #!/usr/bin/env bash
    set -euo pipefail
    full=$(mktemp)
    slim=$(mktemp)
    trap 'rm -f "$full" "$slim"' EXIT
    go run ./tools/bench --split all --out "$full"
    nu --stdin -c '$in | lines | each { from json | reject -o model_check wall_s partial_seconds | to json --raw } | to text' <"$full" >"$slim"
    mv "$slim" tools/bench/baseline.jsonl

# Regenerate privacy-safe real facts from a copied state directory
[group('run')]
facts state_dir: _dev-shell
    go run ./tools/facts "$1" tools/bench/facts/target.jsonl.gz

# Fit the target-machine bootstrap ensemble from the committed evidence
[group('run')]
fit *args: _dev-shell
    go run ./tools/fit "$@"

# Fit the all-facts IN-SAMPLE shared-voltage anchor; not forward-validated
[group('run')]
fit-shared-voltage *args: _dev-shell
    go run ./tools/fit --shared-voltage-in-sample "$@"

# Run only the forward-chained check of the target fit, writing no machine files (`just forward --seal 1`)
[group('run')]
forward *args: _dev-shell
    go run ./tools/fit --forward-only "$@"

# Start the release workflow on main and follow it: unless an issue, open or closed, is labeled needs-hardware, once check passes on main, it commits the release, builds the package, pushes to main and publishes
[group('release')]
release: _dev-shell
    url=$(gh workflow run release.yml --ref main); echo "$url"; gh run watch "${url##*/}" --exit-status

# Print the release the release workflow would make from origin/main
[group('release')]
release-preview: _dev-shell
    go run ./tools/release

# Check the changelog fragments in changes/ (the changes flake check)
[group('release')]
changes: _dev-shell
    go run ./tools/release -check changes

# Print who may post on the repository, what waits on the owner and on the target machine, untriaged issues, work in progress, ready work, overlaps and the open Ruleset issue
[group('github')]
board: _dev-shell
    go run ./tools/board

# Limit issues, comments, reactions and pull requests to collaborators for six months, GitHub's longest interaction limit
[group('github')]
lock-interactions: _dev-shell
    gh api -X PUT 'repos/{owner}/{repo}/interaction-limits' -f limit=collaborators_only -f expiry=six_months

# Claim issue N for BRANCH from BASE (default main): refuse if it is assigned; else assign yourself and post a start comment naming the branch, its base and the one-line PLAN; then create BRANCH from BASE in a worktree yourself
[group('github')]
claim number branch plan base="main": _dev-shell
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -z "$3" || "$3" == *$'\n'* ]]; then
        echo "claim: PLAN must be one non-empty line" >&2
        exit 1
    fi
    holders=$(gh issue view "$1" --json assignees --jq '[.assignees[].login] | join(", ")')
    if [[ -n "$holders" ]]; then
        echo "claim: #$1 is already assigned to $holders" >&2
        exit 1
    fi
    gh issue edit "$1" --add-assignee @me
    body="Started on branch \`$2\` from \`$4\`."$'\n'"Plan: $3"
    if ! gh issue comment "$1" --body "$body"; then
        echo "claim: gh issue comment failed for #$1; removing your assignment" >&2
        gh issue edit "$1" --remove-assignee @me || echo "claim: could not unassign #$1; remove the assignment by hand" >&2
        exit 1
    fi

# Format, run `just gate` under a lock shared by every worktree of this clone, then push the current branch to the branch of the same name on origin, replacing it after a rebase unless the remote branch has commits this one never contained; stack layers use `gh stack push`
[group('github')]
ship: _dev-shell
    #!/usr/bin/env bash
    set -euo pipefail
    branch=$(git branch --show-current)
    if [[ -z "$branch" || "$branch" == main ]]; then
        echo 'ship: check out a pull request branch, not main or a detached HEAD' >&2
        exit 1
    fi
    rc=0
    gh stack view --json >/dev/null 2>&1 || rc=$?
    case "$rc" in
        0 | 6)
            echo "ship: $branch is a gh stack layer; gate it with just gate and push the stack with gh stack push" >&2
            exit 1
            ;;
        2) ;;
        *)
            extensions=$(gh extension list)
            if [[ "$extensions" == *github/gh-stack* ]]; then
                echo "ship: could not tell whether $branch is a gh stack layer (gh stack view exited $rc); see gh stack view" >&2
                exit 1
            fi
            ;;
    esac
    if [[ -n "$(git status --porcelain)" ]]; then
        echo 'ship: commit or stash every change first, untracked files included, so the gate checks what is pushed' >&2
        exit 1
    fi
    just fmt
    if [[ -n "$(git status --porcelain)" ]]; then
        echo 'ship: just fmt changed files; review and commit them, then run just ship again' >&2
        exit 1
    fi
    lock="$(git rev-parse --path-format=absolute --git-common-dir)/togi-gate.lock"
    exec 9>"$lock"
    if ! flock -n 9; then
        echo "ship: another just ship holds $lock; waiting for it to exit" >&2
        flock 9
    fi
    just gate 9>&-
    git push --force-with-lease --force-if-includes --set-upstream origin "refs/heads/$branch:refs/heads/$branch" 9>&-

# Wait until pull request N has check and review passing on its head and no unresolved review thread, then report it ready for the owner; never merges
[group('github')]
land number: _dev-shell
    #!/usr/bin/env bash
    set -euo pipefail
    last=
    while true; do
        while true; do
            read -r state head < <(gh pr view "$1" --json state,headRefOid --jq '"\(.state) \(.headRefOid)"')
            if [[ "$state" != OPEN ]]; then
                echo "land: #$1 is $state" >&2
                exit 1
            fi
            waiting=()
            for name in check review; do
                run=$(gh api "repos/{owner}/{repo}/commits/$head/check-runs?check_name=$name" --jq '.check_runs[0] // {} | "\(.status // "missing") \(.conclusion // "")"')
                read -r status conclusion <<<"$run"
                if [[ "$status" != completed ]]; then
                    waiting+=("$name $status")
                elif [[ "$conclusion" != success ]]; then
                    echo "land: #$1 $name concluded $conclusion on $head" >&2
                    exit 1
                fi
            done
            if ((${#waiting[@]} == 0)); then
                break
            fi
            now="$head: ${waiting[*]}"
            if [[ "$now" != "$last" ]]; then
                echo "land: #$1 waiting on $now"
                last=$now
            fi
            sleep 30
        done
        unresolved=$(gh api graphql --paginate -F number="$1" -f query='query($owner: String!, $repo: String!, $number: Int!, $endCursor: String) { repository(owner: $owner, name: $repo) { pullRequest(number: $number) { reviewThreads(first: 100, after: $endCursor) { nodes { isResolved comments(first: 1) { nodes { url } } } pageInfo { hasNextPage endCursor } } } } }' -F owner='{owner}' -F repo='{repo}' --jq '.data.repository.pullRequest.reviewThreads.nodes[] | select(.isResolved | not) | .comments.nodes[0].url')
        if [[ -n "$unresolved" ]]; then
            echo "land: #$1 has unresolved review threads:" >&2
            echo "$unresolved" >&2
            exit 1
        fi
        read -r state current < <(gh pr view "$1" --json state,headRefOid --jq '"\(.state) \(.headRefOid)"')
        if [[ "$state" != OPEN ]]; then
            echo "land: #$1 is $state" >&2
            exit 1
        fi
        if [[ "$current" == "$head" ]]; then
            break
        fi
        echo "land: #$1 head moved from $head to $current"
    done
    echo "land: #$1 is ready for the owner to merge: check and review passed on $head and every review thread is resolved"

# Run GitHub commands as robotogi
[group('github')]
bot +args: _dev-shell
    #!/usr/bin/env bash
    set -euo pipefail
    token=$(gh-token generate --app-id 5162510 --key "${ROBOTOGI_KEY_FILE:?ROBOTOGI_KEY_FILE must name the robotogi private key file}" --token-only)
    GH_TOKEN=$token exec gh "$@"

# Print the review track record of merged pull requests: robotogi's review check, findings by priority and outcome, and time to the first record
[group('github')]
reviews: _dev-shell
    go run ./tools/reviews
