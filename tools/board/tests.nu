use std/assert
use mod.nu [board-output ready priority touches overlaps pulls-for work group interactions build]

const now = 2026-10-06T12:00:00Z
const fixtures = path self testdata

const base = {
    number: 0, title: '', body: '', pinned: false, labels: [feature P2], assignees: [],
    parent: null, blockers: [], total: 0, completed: 0
}

def fixture-gh [args: list<string>]: nothing -> string {
    assert equal ($args | first) api
    let query = ($args | last)
    if $query =~ 'issues\(' or $query =~ 'pullRequests\(' {
        assert equal ($args | first 9) [api graphql --paginate --slurp -F 'owner={owner}' -F 'name={repo}' -f]
    }
    let file = if $query =~ 'issues\(' { 'issues.json' } else if $query =~ 'pullRequests\(' { 'pulls.json' } else if ($query | str ends-with '/interaction-limits') { 'limit.json' } else { error make {msg: $'unexpected gh call: ($args)'} }
    open --raw ($fixtures | path join $file)
}

def assert-error [action: closure, expected: string] {
    let message = (try { do $action; null } catch {|err| $err.msg})
    assert ($message != null) $'expected error: ($expected)'
    assert str contains $message $expected
}

def test-golden [] {
    assert equal (board-output {|args| fixture-gh $args} $now) (open --raw ($fixtures | path join board.golden))
}

def test-fetch-failures [] {
    for case in [
        {failing: 'issues(states: OPEN', expected: 'fetch issues: HTTP 401'}
        {failing: 'pullRequests(', expected: 'fetch pull requests: HTTP 401'}
        {failing: '/interaction-limits', expected: 'fetch interaction limit: HTTP 401'}
    ] {
        assert-error { board-output {|args|
            if ($args | last | str contains $case.failing) { error make {msg: 'HTTP 401'} }
            fixture-gh $args
        } $now } $case.expected
    }
}

def test-decode-failures [] {
    for case in [
        {failing: 'issues(states: OPEN', expected: 'decode issues:'}
        {failing: 'pullRequests(', expected: 'decode pull requests:'}
        {failing: '/interaction-limits', expected: 'decode interaction limit:'}
    ] {
        for invalid in ['{' '42'] {
            assert-error { board-output {|args|
                if ($args | last | str contains $case.failing) { $invalid } else { fixture-gh $args }
            } $now } $case.expected
        }
    }
    assert-error { board-output {|args|
        if ($args | last | str ends-with '/interaction-limits') {
            '{"limit":"collaborators_only","expires_at":"not a timestamp"}'
        } else { fixture-gh $args }
    } $now } 'decode interaction limit:'
    assert-error { board-output {|args|
        if ($args | last | str contains 'issues(states: OPEN') {
            '[{"data":{"repository":{"issues":{"nodes":[{"number":"wrong"}]}}}}]'
        } else { fixture-gh $args }
    } $now } 'decode issues:'
}

def test-interactions [] {
    for case in [
        {limit: '', expires: null, expected: 'open to everyone: run `just lock-interactions`'}
        {limit: collaborators_only, expires: $now, expected: 'open to everyone: run `just lock-interactions`'}
        {limit: existing_users, expires: 2027-01-06T12:00:00Z, expected: 'limited to existing_users until 2027-01-06, not only collaborators: run `just lock-interactions`'}
        {limit: collaborators_only, expires: ($now + 30day + 1sec), expected: 'collaborators only until 2026-11-05'}
        {limit: collaborators_only, expires: ($now + 30day), expected: 'collaborators only until 2026-11-05, 30 days left: run `just lock-interactions`'}
        {limit: collaborators_only, expires: ($now + 1hr), expected: 'collaborators only until 2026-10-06, 1 days left: run `just lock-interactions`'}
    ] {
        assert equal (interactions $case $now) $case.expected
    }
}

def test-ready [] {
    for case in [
        {edit: {}, expected: true}
        {edit: {labels: [design P3]}, expected: true}
        {edit: {labels: [bugfix P0]}, expected: true}
        {edit: {labels: [idea P1]}, expected: false}
        {edit: {labels: [research P1]}, expected: false}
        {edit: {labels: [feature]}, expected: false}
        {edit: {assignees: [someone]}, expected: false}
        {edit: {blockers: [7]}, expected: false}
        {edit: {total: 3, completed: 2}, expected: false}
        {edit: {total: 3, completed: 3}, expected: true}
    ] {
        assert equal (ready ($base | merge $case.edit)) $case.expected
    }
}

def test-priority [] {
    assert equal (priority {labels: [P3 P1 feature]}) P1
}

def test-touches [] {
    for case in [
        {body: 'Touches: `internal/tuner`, `docs/spec/tuner.md`', expected: [internal/tuner docs/spec/tuner.md]}
        {body: 'Touches: internal/watch,justfile', expected: [internal/watch justfile]}
        {body: 'Touches: `cmd/togi/`', expected: [cmd/togi]}
        {body: 'Touches: ./internal/tuner, `internal//watch/./frame.go`', expected: [internal/tuner internal/watch/frame.go]}
        {body: 'Touches: `.`, `./`', expected: ['.' '.']}
        {body: "## Why\r\n\r\nTouches: `a`, `b`\r\n\r\n## Links", expected: [a b]}
        {body: "Touches:\n<!-- comment -->", expected: []}
        {body: 'It Touches: a', expected: []}
        {body: 'no line', expected: []}
        {body: 'Touches: ../a/../b, /a/../../b, a/.., ../../a', expected: [../b /b '.' ../../a]}
    ] {
        assert equal (touches $case.body) $case.expected
    }
}

def test-overlaps [] {
    for case in [
        [internal/tuner internal/tuner true]
        [internal/tuner internal/tuner/hunt.go true]
        [internal/tuner/hunt.go internal/tuner true]
        [internal/tuner internal/tunerx false]
        [internal/tuner/hunt.go internal/tuner/evidence.go false]
        [docs docs/spec/tuner.md true]
        ['.' internal/tuner/hunt.go true]
        [justfile '.' true]
    ] {
        assert equal (overlaps $case.0 $case.1) $case.2
    }
}

def test-pulls-for [] {
    let pulls = [
        {number: 5, body: 'Resolves #12, Refs #4'}
        {number: 1, body: 'Closes #12'}
        {number: 2, body: 'Refs #120'}
        {number: 3, body: 'Mentions #12 in passing'}
        {number: 7, body: ''}
        {number: 8, body: 'Fixes: #12'}
        {number: 9, body: 'Refs #12 too'}
        {number: 10, body: "Block: bug fixes\n  #12 Title"}
        {number: 11, body: "Closes\r\n#12"}
    ]
    assert equal (pulls-for 12 $pulls | get number) [1 5 8 9]
}

def test-work [] {
    assert equal (work shgew/togi [
        {repo: shgew/togi, branch: linked}
        {repo: shgew/togi, branch: linked}
        {repo: shgew/togi, branch: opened}
        {repo: someone/togi, branch: linked}
        {repo: '', branch: gone}
    ]) [linked opened 'someone/togi:linked']
}

def test-group [] {
    let a = {number: 2, title: 'Block: a'}
    let b = {number: 9, title: 'Block: b'}
    let issues = [
        {number: 1, labels: [P2], parent: $b}
        {number: 2, labels: [P1], parent: null}
        {number: 3, labels: [P1], parent: $b}
        {number: 4, labels: [P1], parent: $a}
        {number: 5, labels: [P1], parent: $b}
    ]
    assert equal (group $issues | each {|g| $g.issues | get number}) [[4] [3 5] [2] [1]]
}

def test-hardware-exclusivity [] {
    for assignees in [[] [shgew]] {
        let issues = [
            ($base | merge {number: 1, labels: [feature P1 needs-hardware], assignees: $assignees, body: 'Touches: `internal/tuner`'})
            ($base | merge {number: 2, body: 'Touches: `internal/tuner`'})
            ($base | merge {number: 3, assignees: [shgew], body: 'Touches: `internal/tuner/hunt.go`'})
        ]
        let b = (build shgew/togi $issues [] {limit: '', expires: null} $now)
        assert equal ($b.hardware | get number) [1]
        assert equal ($b.progress | get number) [3]
        assert equal ($b.overlaps | each {|o| [$o.ready $o.progress]} | flatten) [2 3]
        assert equal ($b.ready | each {|g| $g.issues | get number} | flatten) [2]
    }
}

def main [] {
    nu-check --debug ($env.CURRENT_FILE | path dirname | path join main.nu)
    nu-check --debug --as-module ($env.CURRENT_FILE | path dirname | path join mod.nu)
    test-golden
    test-fetch-failures
    test-decode-failures
    test-interactions
    test-ready
    test-priority
    test-touches
    test-overlaps
    test-pulls-for
    test-work
    test-group
    test-hardware-exclusivity
    print 'board: 12 tests passed'
}
