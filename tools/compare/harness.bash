#!/usr/bin/env bash
# The comparison harness's shell side; see docs/porting.md. Subcommands:
#   corpus   run every suite session with the Go simulator, as tools/bench does
#   expect   write Go's expected output for every journal the manifests list
#   check    compare a subject's output with the expected output, byte for byte
#   ids      print the manifests' journals: id, mode and path
set -euo pipefail

jobs=${HARNESS_JOBS:-$(nproc)}
self=$(realpath "${BASH_SOURCE[0]}")

ids() {
    local scenario split seed machine kind path hash
    while IFS=$'\t' read -r scenario split seed machine; do
        printf 'suite/%s/%s-%s\tlive\t%s/sessions/%s/%s-%s/events.jsonl.zst\n' "$scenario" "$split" "$seed" "$CORPUS" "$scenario" "$split" "$seed"
    done <"$MANIFESTS/suite.txt"
    while IFS=$'\t' read -r kind path hash; do
        [[ $kind == journal-* ]] && printf 'fixture/%s\t%s\t%s/%s\n' "$path" "${kind#journal-}" "$SOURCE" "$path"
    done <"$MANIFESTS/fixtures.txt"
    return 0
}

sorted_ids() { ids | LC_ALL=C sort; }

plain() {
    case $1 in
    *.zst) zstd -dcq "$1" >"$2" ;;
    *.gz) gzip -dc "$1" >"$2" ;;
    *) cp "$1" "$2" ;;
    esac
}

corpus() {
    local out=$1 runs
    runs=$("$COMPARE" runs tools/bench/suite.json)
    mkdir -p "$out/sessions"
    export HARNESS_SIM HARNESS_OUT=$out
    printf '%s\n' "$runs" | tr '\t\n' ' \0' | xargs -0 -P "$jobs" -n1 "$self" session
    for d in "$out"/sessions/*/*; do
        [[ -f $d/events.jsonl.zst ]] || { echo "corpus: no journal in $d" >&2; exit 1; }
    done
}

session() {
    local dir=${1%% *} flags=${1#* } state
    state=$(mktemp -d)
    mkdir -p "$HARNESS_OUT/sessions/$dir"
    status=0
    # shellcheck disable=SC2086
    "$HARNESS_SIM" $flags --state-dir "$state" >"$state/log" 2>&1 || status=$?
    echo "$status" >"$HARNESS_OUT/sessions/$dir/exit"
    zstd -q -3 "$state/events.jsonl" -o "$HARNESS_OUT/sessions/$dir/events.jsonl.zst"
    rm -rf "$state"
}

expect() {
    local out=$1
    mkdir -p "$out"
    export COMPARE out self
    sorted_ids | xargs -d '\n' -P "$jobs" -n1 "$self" expect-one
}

expect-one() {
    IFS=$'\t' read -r id mode path <<<"$1"
    local tmp
    tmp=$(mktemp -d)
    mkdir -p "$out/$id"
    plain "$path" "$tmp/journal.jsonl"
    "$COMPARE" "$mode" "$tmp/journal.jsonl" "$tmp/out"
    [[ $mode != live ]] || cmp -s "$tmp/journal.jsonl" "$tmp/out/encoded.jsonl" || { echo "expect: Go does not re-encode $id to its own bytes" >&2; exit 1; }
    zstd -q -3 "$tmp/out/encoded.jsonl" -o "$out/$id/encoded.jsonl.zst"
    cp "$tmp/out/state.json" "$out/$id/state.json"
    rm -rf "$tmp"
}

# Every journal the manifests list has expected output, and nothing else does.
coverage() {
    local want have
    want=$(sorted_ids | cut -f1)
    have=$(cd "$EXPECTED" && find . -name state.json -printf '%h\n' | sed 's|^\./||' | LC_ALL=C sort)
    [[ $want == "$have" ]] || { echo "check: expected output and manifests disagree:" >&2; diff <(echo "$want") <(echo "$have") >&2 || true; return 1; }
    have=$(cd "$CORPUS/sessions" && find . -name events.jsonl.zst -printf '%h\n' | sed 's|^\./|suite/|' | LC_ALL=C sort)
    [[ $(echo "$want" | grep '^suite/') == "$have" ]] || { echo "check: corpus and suite manifest disagree:" >&2; diff <(echo "$want" | grep '^suite/') <(echo "$have") >&2 || true; return 1; }
    echo "coverage: $(echo "$want" | wc -l) journals"
}

check() {
    coverage
    export SUBJECT EXPECTED self
    local selected
    selected=$(sorted_ids | awk -F'\t' -v n="${HARNESS_SHARDS:-1}" -v i="${HARNESS_SHARD:-0}" -v sel="${HARNESS_SELECT:-}" '(NR - 1) % n == i && (sel == "" || $1 ~ sel)')
    [[ -n $selected ]] || { echo "check: selection is empty" >&2; return 1; }
    local failed
    failed=$(printf '%s\n' "$selected" | xargs -d '\n' -P "$jobs" -n1 "$self" check-one | LC_ALL=C sort || true)
    [[ -z $failed ]] || { printf '%s\n' "$failed" >&2; return 1; }
    echo "compared: $(echo "$selected" | wc -l) journals"
}

check-one() {
    IFS=$'\t' read -r id mode path <<<"$1"
    local tmp
    tmp=$(mktemp -d)
    plain "$path" "$tmp/journal.jsonl"
    mkdir "$tmp/out"
    if ! HARNESS_ID=$id $SUBJECT "$mode" "$tmp/journal.jsonl" "$tmp/out" 2>"$tmp/err"; then
        echo "FAIL $id: subject failed: $(head -c 300 "$tmp/err")"
    elif [[ ! -f $tmp/out/encoded.jsonl || ! -f $tmp/out/state.json ]]; then
        echo "FAIL $id: subject wrote no encoded.jsonl or state.json"
    else
        zstd -dcq "$EXPECTED/$id/encoded.jsonl.zst" | cmp -s - "$tmp/out/encoded.jsonl" || echo "FAIL $id: encoded.jsonl differs"
        cmp -s "$EXPECTED/$id/state.json" "$tmp/out/state.json" || echo "FAIL $id: state.json differs"
    fi
    rm -rf "$tmp"
}

command=${1:?usage: harness.bash corpus|expect|check|ids}
shift
case $command in
ids) sorted_ids ;;
corpus) corpus "$@" ;;
session) session "$@" ;;
expect) expect "$@" ;;
expect-one) expect-one "$@" ;;
check) check ;;
check-one) check-one "$@" ;;
*) echo "harness.bash: unknown command $command" >&2; exit 2 ;;
esac
