#!/usr/bin/env bash
# Check views.sql on copies of testdata/live: one as committed but with gzip samples, and one holding
# only the current journal.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

cp -R "$here/testdata/live" "$tmp/live"
chmod -R u+w "$tmp/live"
cat "$tmp/live/trials/0001/samples.jsonl" "$tmp/live/trials/0001/samples.jsonl" | gzip > "$tmp/live/trials/0001/samples.jsonl.gz"
gzip "$tmp/live/trials/0002/samples.jsonl"
"$here/sql.sh" "$tmp/live" -bail < "$here/tests.sql"

mkdir "$tmp/bare"
cp "$here/testdata/live/events.jsonl" "$tmp/bare/"
"$here/sql.sh" "$tmp/bare" -bail -list -noheader -c "
    select if(
        (select count(*) from events) = 4 and not exists (from samples) and not exists (from core_samples),
        'ok a directory without archives or samples reads as its journal alone',
        error('failed: a directory without archives or samples')
    )"
