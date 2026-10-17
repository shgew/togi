set shell := ["bash", "-euo", "pipefail", "-c"]
set positional-arguments

system := arch() + "-" + replace(os(), "macos", "darwin")

# List recipes by group in file order
_default:
    @{{ just_executable() }} --justfile '{{ justfile() }}' --list --unsorted

_dev-shell:
    @[[ "${TOGI_DEV_SHELL:-}" == 1 ]] || { echo 'this recipe needs the dev shell: run `nix develop`, or `direnv allow` once per checkout' >&2; exit 1; }

# Run the test suite with optional nextest flags (`just test -p togi-journal`)
[group('test')]
test *args: _dev-shell
    cargo nextest run "$@"

# Test the Nushell coordination board against fixed GitHub fixtures, without network access
[group('test')]
board-test: _dev-shell
    nu --no-config-file tools/board/tests.nu

# Check the DuckDB views of `just sql` against fixed state directories: plain and gzip samples, an archived session, and a journal without archives or samples
[group('test')]
sql-test: _dev-shell
    tools/sql/test.sh

# Run one crate's tests with optional nextest flags (`just focus togi-journal`)
[group('test')]
focus crate *args: _dev-shell
    cargo nextest run -p "$@"

# Type-check one crate without building it (`just typecheck togi-journal`)
[group('test')]
typecheck crate: _dev-shell
    cargo check --all-targets -p "$1"

# Run the hardware tests of smu, detect, trial and hardware one binary and one test at a time, every binary even after one fails, on the target machine (Linux only), entering the dev shell itself because sudo drops it; as root, as the SMU needs, it builds under root's home and skips trial's `cached_errors`, which refuses uid 0 (`just hardware --no-run` only builds them)
[group('test')]
hardware *args:
    @[[ "{{ os() }}" == linux ]] || { echo 'just hardware needs Linux: run it on the target machine' >&2; exit 1; }
    tests=(--test hardware); [[ "$(id -u)" == 0 ]] || tests+=(--test cached_errors); [[ "$(id -u)" != 0 ]] || export CARGO_TARGET_DIR=~root/.cache/togi/hardware-target; nix develop --command env TMPDIR=/tmp RUST_TEST_THREADS=1 cargo test --locked --no-fail-fast -p togi-smu -p togi-detect -p togi-trial -p togi-hardware --features hardware "${tests[@]}" "$@"

# Fuzz the journal reader for the given seconds (`just fuzz 300`), seeded from fuzz/seeds
[group('test')]
fuzz seconds="60": _dev-shell
    cd crates/togi-journal && mkdir -p fuzz/corpus/reader && cargo fuzz run -s none reader fuzz/corpus/reader fuzz/seeds/reader -- -max_total_time="$1"

# Print changed lines no test reached since a base, and the changed files the run does not build (`just cover origin/main`)
[group('test')]
cover base: _dev-shell
    #!/usr/bin/env bash
    set -euo pipefail
    trace=$(mktemp)
    trap 'rm -f "$trace"' EXIT
    cargo llvm-cov nextest --locked --lcov --output-path "$trace" >&2
    target=${CARGO_LLVM_COV_TARGET_DIR:-${CARGO_TARGET_DIR:-target}/llvm-cov-target}
    go run ./tools/cover --lcov "$trace" --build-dir "$target/debug" --base "$1"

# Lint the retained Go tool for Linux and macOS, check the Nushell board's syntax, and run workspace clippy for the host and aarch64 macOS, with optional Go lint flags
[group('quality')]
lint *args: _dev-shell
    for target in linux/amd64 darwin/arm64; do env GOOS="${target%/*}" GOARCH="${target#*/}" CGO_ENABLED=0 golangci-lint run ./... "$@"; done
    nu --no-config-file -c 'nu-check --debug ("tools/board/main.nu" | path expand); nu-check --debug --as-module ("tools/board/mod.nu" | path expand)'
    cargo clippy --locked --all-targets -- --deny warnings
    cargo clippy --locked --all-targets --target aarch64-apple-darwin -- --deny warnings

# Format Go, Nix and this justfile in place
[group('quality')]
fmt:
    nix fmt

# Run every non-VM flake check but the sim-suite-* shards, which CI and `just check` on Linux run (`just sim-suite` or `just check-one sim-suite-<i>` reproduces them), and cargo-deny's advisory audit, which only the deny flake check runs, cheapest first, using warm dev-shell Go and cargo caches
[group('quality')]
gate: _dev-shell
    #!/usr/bin/env bash
    set -euo pipefail
    just check-one fmt
    just lint
    just check-one module changes
    just board-test
    just sql-test
    vendored=$(nix build --no-link --print-out-paths '.#checks.{{ system }}.go-tools.goModules')
    fresh=$(mktemp -d)
    trap 'rm -rf "$fresh"' EXIT
    go mod vendor -o "$fresh/vendor"
    diff -rq "$fresh/vendor" "$vendored" >&2 || { echo 'gate: the Go modules vendored for vendorHash in flake.nix differ from go.mod and go.sum; update vendorHash' >&2; exit 1; }
    go test -shuffle=on -tags integration ./...
    cargo nextest run --locked --profile ci
    cargo test --locked --doc
    cargo deny --locked check bans licenses sources
    cargo shear

# Run every flake check this host builds, streaming their build logs: board-test, sql-test, go-tools, lint, fmt, changes, module, clippy, nextest, doc, cargo-deps, cargo-fmt, deny, shear, package and, on Linux, trial-scope-tests, the sim-suite-* shards and the VM tests
[group('nix')]
check *args:
    nix flake check --print-build-logs "$@"

# Build named flake checks, streaming their build logs: board-test, sql-test, go-tools, lint, fmt, changes, module, clippy, nextest, doc, cargo-deps, cargo-fmt, deny, shear, package or, on Linux, trial-scope-tests, vm-recovery-tests, sim-suite-<i>, vm, vm-host-lock, vm-trial-scope, vm-restart-limit and vm-emergency-stop (`just check-one lint`)
[group('nix')]
check-one +names:
    nix build --no-link --print-build-logs $(printf '.#checks.{{ system }}.%s ' "$@")

# Run a simulated session, flags as `togi-tool-sim` takes them (`just sim --seed 7 --max-boots 20`)
[group('run')]
sim *args: _dev-shell
    cargo run --release --locked -p togi-tool-sim -- "$@"

# Run the togi binary built from this tree, flags as `togi` takes them (`just togi completion zsh`); a local build reports the revision `dev`
[group('run')]
togi *args: _dev-shell
    cargo run --release --locked -p togi -- "$@"

# Run every session of the simulator suite and check that each ended clean
[group('run')]
sim-suite *args: _dev-shell
    out=$(mktemp -d); cargo run --release --locked -p togi-tool-sim -- suite --suite tools/bench/suite.json --out "$out" "$@" && cargo run --release --locked -p togi-tool-sim -- check --suite tools/bench/suite.json --shards 1 "$out"

# Play a recorded journal through the dashboard on a fast-forward clock
[group('run')]
replay *args: _dev-shell
    cargo run --release --locked -p togi-tool-replay -- "$@"

# Summarize a current or archived journal for review
[group('run')]
stats *args: _dev-shell
    cargo run --release --locked -p togi-tool-stats -- "$@"

# Query a state directory's current and archived events, trials and per-trial samples with DuckDB (default /var/lib/togi; read only); further arguments go to duckdb (`just sql runs/x -c 'from trials limit 5'`); docs/reviewing.md has the views and example queries
[group('run')]
sql dir="/var/lib/togi" *args: _dev-shell
    tools/sql/sql.sh "$@"

# Benchmark simulated session conclusions, building and running the simulator (`just bench --split dev --baseline tools/bench/baseline.jsonl`, or `just bench sanity --records records.jsonl` against the recorded baseline)
[group('run')]
bench *args: _dev-shell
    cargo run --release --locked -p togi-tool-bench -- "$@"

# Sweep every simulated scenario with the bench and simulator and audit the retained journals, or audit given state directories (`just audit --seeds 50 --jobs 8 --keep runs`)
[group('run')]
audit *args: _dev-shell
    cargo build --release --locked -p togi-tool-bench
    cargo run --release --locked -p togi-tool-audit -- "$@"

# Prove simulated session decisions are unchanged from a base revision, building both trees' simulators; stops at the first difference (`--keep-going` counts them all)
[group('run')]
same base="origin/main" *args: _dev-shell
    dir=$(mktemp -d); trap 'rm -rf "$dir"' EXIT; git archive "$1" | tar -x -C "$dir"; cargo run --release --locked -p togi-tool-bench -- same "$dir" "${@:2}"

# Prove a fixed selection of simulated sessions, one per scenario, is unchanged from a base revision; same cache and fail-fast as `same`
[group('run')]
smoke base="origin/main" *args: _dev-shell
    dir=$(mktemp -d); trap 'rm -rf "$dir"' EXIT; git archive "$1" | tar -x -C "$dir"; cargo run --release --locked -p togi-tool-bench -- same "$dir" --smoke "${@:2}"

# Forecast a real run from a copy of its state directory with the target ensemble (`--out FILE` writes the forecast record)
[group('run')]
forecast state_dir *args: _dev-shell
    cargo run --release --locked -p togi-tool-bench -- forecast "$1" "${@:2}"

# Compute the counterfactual pass/cost of the R7 part requirements at a fixed profile, never tuner evidence (`just pass-cost` over the #490 hazard grid, `just pass-cost --machine FILE --profile OFFSETS`)
[group('run')]
pass-cost *args: _dev-shell
    cargo run --release --locked -p togi-tool-bench -- pass-cost "$@"

# Re-record the committed bench baseline over all seeds, without model checks or wall times (`just bench-baseline` writes tools/bench/baseline.jsonl, `just bench-baseline OUT` another file)
[group('run')]
bench-baseline out="tools/bench/baseline.jsonl": _dev-shell
    cargo run --release --locked -p togi-tool-bench -- --split all --baseline-out "$1"

# Regenerate privacy-safe real facts from a copied state directory
[group('run')]
facts state_dir: _dev-shell
    cargo run --release --locked -p togi-tool-facts -- "$1" tools/bench/facts/target.jsonl.gz

# Fit the target-machine bootstrap ensemble from named facts into a named directory (`just fit --facts FILE --out DIR`; add --current-extract to read sessions after the research artifact); the result does not depend on --jobs
[group('run')]
fit *args: _dev-shell
    cargo run --release --locked -p togi-tool-fit -- "$@"

# Fit the all-facts IN-SAMPLE shared-voltage anchor from named facts into a named directory; not forward-validated
[group('run')]
fit-shared-voltage *args: _dev-shell
    cargo run --release --locked -p togi-tool-fit -- --shared-voltage-in-sample "$@"

# Write the original seven-session research artifact from the committed extract to the new file OUT, checking its hash (tools/fit/program.md#the-data)
[group('run')]
fit-artifact out: _dev-shell
    cargo run --release --locked -p togi-tool-fit -- --facts tools/bench/facts/target.jsonl.gz --write-artifact "$1"

# Run only the forward-chained check on named facts, writing no machine files (`just forward --facts ARTIFACT --seal 1`)
[group('run')]
forward *args: _dev-shell
    cargo run --release --locked -p togi-tool-fit -- --forward-only "$@"

# Dispatch one step of a research campaign under its launch manifest, never the campaign itself (`just research 427 start --manifest FILE`; see docs/benchmarking.md#research-campaigns)
[group('run')]
research issue *args: _dev-shell
    cargo run --release --locked -p togi-tool-bench -- research "$@"

# Start the release workflow on main and follow it: unless an issue, open or closed, is labeled needs-hardware, once check passes on main, it commits the release, builds the package, pushes to main and publishes
[group('release')]
release: _dev-shell
    url=$(gh workflow run release.yml --ref main); echo "$url"; gh run watch "${url##*/}" --exit-status

# Print the release the release workflow would make from origin/main
[group('release')]
release-preview: _dev-shell
    cargo run --quiet --locked -p togi-tool-release

# Check the changelog fragments in changes/ (the changes flake check)
[group('release')]
changes: _dev-shell
    cargo run --quiet --locked -p togi-tool-release -- -check changes

# Print who may post on the repository, what waits on the owner and on the target machine, untriaged issues, work in progress, ready work, overlaps and the open Ruleset issue
[group('github')]
board: _dev-shell
    nu --no-config-file tools/board/main.nu

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

# Refuse unless HEAD is a clean pull request branch outside a gh stack, then format and refuse if that changed files; RECIPE names the caller, STACK_HINT says what a stack layer does instead
_push-ready recipe stack_hint: _dev-shell
    #!/usr/bin/env bash
    set -euo pipefail
    branch=$(git branch --show-current)
    if [[ -z "$branch" || "$branch" == main ]]; then
        echo "$1: check out a pull request branch, not main or a detached HEAD" >&2
        exit 1
    fi
    rc=0
    gh stack view --json >/dev/null 2>&1 || rc=$?
    case "$rc" in
        0 | 6)
            echo "$1: $branch is a gh stack layer; $2" >&2
            exit 1
            ;;
        2) ;;
        *)
            extensions=$(gh extension list)
            if [[ "$extensions" == *github/gh-stack* ]]; then
                echo "$1: could not tell whether $branch is a gh stack layer (gh stack view exited $rc); see gh stack view" >&2
                exit 1
            fi
            ;;
    esac
    if [[ -n "$(git status --porcelain)" ]]; then
        echo "$1: commit or stash every change first, untracked files included, so the checks run on what is pushed" >&2
        exit 1
    fi
    just fmt
    if [[ -n "$(git status --porcelain)" ]]; then
        echo "$1: just fmt changed files; review and commit them, then run just $1 again" >&2
        exit 1
    fi

# Format, run `just gate`, then push the current branch to the branch of the same name on origin, replacing it after a rebase unless the remote branch has commits this one never contained; stack layers use `gh stack push`
[group('github')]
ship: (_push-ready "ship" "gate it with just gate and push the stack with gh stack push")
    #!/usr/bin/env bash
    set -euo pipefail
    branch=$(git branch --show-current)
    just gate
    git push --force-with-lease --force-if-includes --set-upstream origin "refs/heads/$branch:refs/heads/$branch"

# Print the Go packages changed between a pushed head and HEAD, one per line: `./...` when go.mod or go.sum changed, nothing when no Go file did; a package whose Go files were deleted or moved away counts while it still has Go files at HEAD
_touched-packages pushed:
    #!/usr/bin/env bash
    set -euo pipefail
    files=$(git diff --name-only --no-renames "$1" HEAD)
    if grep -qxE 'go\.(mod|sum)' <<<"$files"; then
        echo './...'
        exit
    fi
    { grep -E '\.go$' <<<"$files" || true; } | while IFS= read -r file; do
        dir=$(dirname "$file")
        [[ "$dir" == . ]] && tree=HEAD: || tree="HEAD:$dir"
        names=$(git ls-tree --name-only "$tree" 2>/dev/null) || continue
        grep -qE '\.go$' <<<"$names" || continue
        [[ "$dir" == . ]] && echo . || echo "./$dir"
    done | sort -u

# Print the crates changed between a pushed head and HEAD, one per line: `--workspace` when Cargo.toml, Cargo.lock, rust-toolchain.toml, clippy.toml, .config/nextest.toml or .cargo/config.toml changed, files every crate's build or tests read, else each crate with a changed file under crates/ that still has its Cargo.toml at HEAD
_touched-crates pushed:
    #!/usr/bin/env bash
    set -euo pipefail
    files=$(git diff --name-only --no-renames "$1" HEAD)
    if grep -qxE 'Cargo\.toml|Cargo\.lock|rust-toolchain\.toml|clippy\.toml|\.config/nextest\.toml|\.cargo/config(\.toml)?' <<<"$files"; then
        echo --workspace
        exit
    fi
    { grep -oE '^crates/[^/]+/' <<<"$files" || true; } | sort -u | while IFS= read -r dir; do
        git cat-file -e "HEAD:${dir}Cargo.toml" 2>/dev/null && basename "$dir"
    done || true

# Push a fix to a pull request under review: format, lint, test the changed Go packages, Rust crates, Nushell board and SQL views, then push as `just ship` does without the gate; the first push of a branch uses `just ship`
[group('github')]
push-fix: (_push-ready "push-fix" "push the stack with gh stack push")
    #!/usr/bin/env bash
    set -euo pipefail
    branch=$(git branch --show-current)
    pushed=$(git rev-parse --verify --quiet "refs/remotes/origin/$branch") || {
        echo "push-fix: origin has no $branch; push its first head with just ship" >&2
        exit 1
    }
    mapfile -t packages < <(just _touched-packages "$pushed")
    just lint
    if ((${#packages[@]} == 0)); then
        echo "push-fix: no Go package changed since $pushed; nothing to test" >&2
    else
        go test "${packages[@]}"
    fi
    if ! git diff --quiet "$pushed" HEAD -- tools/board; then
        just board-test
    fi
    if ! git diff --quiet "$pushed" HEAD -- tools/sql; then
        just sql-test
    fi
    mapfile -t crates < <(just _touched-crates "$pushed")
    if ((${#crates[@]} == 0)); then
        echo "push-fix: no crate changed since $pushed; nothing to test" >&2
    elif [[ "${crates[0]}" == --workspace ]]; then
        just test --workspace
    else
        args=()
        for crate in "${crates[@]}"; do args+=(-p "$crate"); done
        just test "${args[@]}"
    fi
    git push --force-with-lease --force-if-includes --set-upstream origin "refs/heads/$branch:refs/heads/$branch"

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
    exec just --justfile '{{ justfile() }}' as-bot gh "$@"

# Run a command with one robotogi App token in GH_TOKEN (`just as-bot just reviews publish <snapshot directory>`)
[group('github')]
as-bot +cmd: _dev-shell
    #!/usr/bin/env bash
    set -euo pipefail
    token=$(gh-token generate --app-id 5162510 --key "${ROBOTOGI_KEY_FILE:?ROBOTOGI_KEY_FILE must name the robotogi private key file}" --token-only)
    GH_TOKEN=$token exec "$@"

# Print the review track record of merged pull requests; subcommands snapshot, render and publish run a review's mechanical steps (docs/review-record.md)
[group('github')]
reviews *args: _dev-shell
    cargo run --quiet --locked -p togi-tool-reviews -- "$@"
