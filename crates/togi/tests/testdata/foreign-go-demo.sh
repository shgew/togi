#!/usr/bin/env bash
# Demo: the Rust build refuses the Go build's state directory with all four commands and changes nothing in it.
#
#   crates/togi/tests/testdata/foreign-go-demo.sh [path/to/togi]
#
# It copies foreign-go/ (the Go build's state directory; see tests/support/foreign.rs) to a temporary directory, records
# a checksum of every file's bytes, mode and modification time plus the directory listing, runs `togi run`, `status`,
# `events` and `watch` against the copy, and prints each exit status, stderr and whether the record still matches.
# It exits 1 unless every command exits 1 naming the journal, the cutover step and another build's journal, and the
# directory is unchanged. `run` stops before any lock, GRUB write or hardware access, so it is safe on a real host.
set -euo pipefail

togi=${1:-target/debug/togi}
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
state=$(mktemp -d)
trap 'rm -rf "$state" "$state.out" "$state.err"' EXIT
cp -a "$here/foreign-go/." "$state/"

record() { (cd "$state" && find . -printf '%p %y %m %T@ %s\n' | LC_ALL=C sort && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum); }
before=$(record)
failed=0
for command in "run --no-tui --config /nonexistent/togi.json" "run --tuning-boot $state/grubenv --config /nonexistent/togi.json" status events "watch --width 100 --height 30"; do
  echo "== togi --state-dir <dir> $command"
  status=0
  # shellcheck disable=SC2086 # the command words are separate on purpose
  "$togi" --state-dir "$state" $command >"$state.out" 2>"$state.err" </dev/null || status=$?
  echo "exit $status"
  sed "s|$state|<dir>|g" "$state.err"
  [[ $status -eq 1 ]] || { echo "FAIL: exit $status, not 1"; failed=1; }
  [[ ! -s "$state.out" ]] || { echo "FAIL: stdout is not empty"; failed=1; }
  for wanted in "$state/events.jsonl" "docs/howto.md#go-to-rust-cutover" "another build's journal"; do
    grep -qF -- "$wanted" "$state.err" || { echo "FAIL: stderr lacks: ${wanted/$state/<dir>}"; failed=1; }
  done
  if [[ "$(record)" == "$before" ]]; then echo "directory unchanged"; else echo "FAIL: directory changed"; failed=1; fi
done
echo "listing:"; (cd "$state" && find . | LC_ALL=C sort)
exit "$failed"
