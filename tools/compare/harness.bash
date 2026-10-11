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

# Each session as tools/bench runs it: its directory, then the simulator flags. A scenario's machine for a seed is the
# seed's index in its split modulo the machines of the scenario, the path resolved beside the suite file.
runs() {
    jq -r '.scenarios[] | . as $s | (if (.machines // [] | length) > 0 then .machines else [.machine // ""] end) as $m
        | ("dev", "holdout") as $split | ($s[$split] // []) | to_entries[]
        | "\($s.name)/\($split)-\(.value)\t--seed \(.value) --max-boots 1000"
        + (if $m[.key % ($m | length)] != "" then " --machine tools/bench/\($m[.key % ($m | length)])" else "" end)
        + (if $s.replay then " --replay-facts" else "" end)' tools/bench/suite.json
}

corpus() {
    local out=$1
    mkdir -p "$out/sessions"
    export HARNESS_SIM HARNESS_OUT=$out
    runs | tr '\t\n' ' \0' | xargs -0 -P "$jobs" -n1 "$self" session
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
    echo "$flags" >"$HARNESS_OUT/sessions/$dir/flags"
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

# Every journal the manifests list has both expected outputs, and no other file is in the expected output.
coverage() {
    local want have files
    want=$(sorted_ids | cut -f1)
    files=$(echo "$want" | awk '{ print $0 "/encoded.jsonl.zst"; print $0 "/state.json" }' | LC_ALL=C sort)
    have=$(cd "$EXPECTED" && find . ! -type d -printf '%P\n' | LC_ALL=C sort)
    [[ $files == "$have" ]] || { echo "check: expected output and manifests disagree:" >&2; diff <(echo "$files") <(echo "$have") >&2 || true; return 1; }
    have=$(cd "$CORPUS/sessions" && find . -name events.jsonl.zst -printf '%h\n' | sed 's|^\./|suite/|' | LC_ALL=C sort)
    [[ $(echo "$want" | grep '^suite/') == "$have" ]] || { echo "check: corpus and suite manifest disagree:" >&2; diff <(echo "$want" | grep '^suite/') <(echo "$have") >&2 || true; return 1; }
    local scenario split seed machine flags
    while IFS=$'\t' read -r scenario split seed machine; do
        flags=$(<"$CORPUS/sessions/$scenario/$split-$seed/flags")
        if [[ $machine == - ]]; then [[ $flags != *--machine* ]]; else [[ $flags == *" --machine $machine"* ]]; fi ||
            { echo "check: corpus ran $scenario/$split-$seed with '$flags', the suite manifest says machine $machine" >&2; return 1; }
    done <"$MANIFESTS/suite.txt"
    echo "coverage: $(echo "$want" | wc -l) journals"
}

check() {
    coverage
    export SUBJECT EXPECTED self
    local selected
    selected=$(sorted_ids | awk -F'\t' -v n="${HARNESS_SHARDS:-1}" -v i="${HARNESS_SHARD:-0}" -v sel="${HARNESS_SELECT:-}" '(NR - 1) % n == i && (sel == "" || $1 ~ sel)')
    [[ -n $selected ]] || { echo "check: selection is empty" >&2; return 1; }
    # A worker that stops before it compares, a failed decompression for example, prints no FAIL line: its exit status
    # is the only trace, and it fails the check apart from the FAIL lines.
    local failed status=0
    failed=$(printf '%s\n' "$selected" | xargs -d '\n' -P "$jobs" -n1 "$self" check-one) || status=$?
    failed=$(LC_ALL=C sort <<<"$failed")
    [[ -z $failed ]] || printf '%s\n' "$failed" >&2
    ((status == 0)) || echo "check: a comparison worker failed" >&2
    [[ -z $failed && $status == 0 ]] || return 1
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
