#!/usr/bin/env bash
# Check views.sql on copies of testdata/live: one as committed but with gzip samples, one holding only
# the current journal, one whose trial ends while DuckDB is open, and one with a trial directory the
# user cannot list.
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

cp -R "$here/testdata/live" "$tmp/moved"
chmod -R u+w "$tmp/moved"
"$here/sql.sh" "$tmp/moved" -bail -list -noheader <<EOF
.shell gzip '$tmp/moved/trials/0002/samples.jsonl'
.read '$here/views.sql'
select if(
    (select count(*) from samples where session = '20260102T000000Z' and trial = '0002') = 1,
    'ok reading the views again follows a trial whose samples were compressed since',
    error('failed: reading the views again after a trial''s samples were compressed')
);
EOF

if ((EUID != 0)); then
    cp -R "$here/testdata/live" "$tmp/unlisted"
    chmod -R u+w "$tmp/unlisted"
    chmod a-r "$tmp/unlisted/trials/0001"
    if "$here/sql.sh" "$tmp/unlisted" -c 'select 1' > /dev/null 2>&1; then
        echo "failed: a trial directory the user cannot list is refused" >&2
        exit 1
    fi
    chmod u+r "$tmp/unlisted/trials/0001"
    echo "ok a trial directory the user cannot list is refused"
fi
