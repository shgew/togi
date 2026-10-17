#!/usr/bin/env bash
# Open DuckDB on a togi state directory with the views of views.sql: `sql.sh DIR [DUCKDB-ARGS...]`.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
[[ -d $1 ]] || { echo "sql: $1 is not a directory" >&2; exit 1; }
dir=$(cd "$1" && pwd -P)
[[ -e $dir/events.jsonl || -d $dir/archive ]] || { echo "sql: $dir has neither events.jsonl nor archive/, so it is not a togi state directory" >&2; exit 1; }
# The views find files by listing these directories; one this user may enter but not list, such as a
# trial directory togi made 0711 for a backend account, would read as no samples instead of failing.
for listed in "$dir/archive" "$dir/trials" "$dir"/trials/* "$dir"/archive/*-trials "$dir"/archive/*-trials/*; do
    [[ ! -d $listed || (-r $listed && -x $listed) ]] || { echo "sql: cannot list $listed; run as root or query a copy of the state directory" >&2; exit 1; }
done

quote() { printf "'%s'" "${1//\'/\'\'}"; }
exec duckdb \
    -cmd "set variable state_dir = $(quote "$dir")" \
    -cmd "set variable empty = $(quote "$here/empty.jsonl")" \
    -cmd ".read \"$here/views.sql\"" \
    "${@:2}"
