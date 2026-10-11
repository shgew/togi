use std/assert
use guard.nu *
use manifest.nu in-scope

const testdata = path self testdata

def fixture [name: string]: nothing -> string {
    open --raw ($testdata | path join $name)
}

def base-snapshot []: nothing -> record {
    let manifest = fixture manifest.sha256
    {
        tree: [
            {path: 'go.mod', object: 'a1'}
            {path: 'tools/guard/guard.nu', object: 'g1'}
            {path: 'tools/guard/test.nu', object: 't1'}
            {path: '.github/workflows/guard.yml', object: 'w1'}
            {path: 'tools/sim/main.go', object: 's1'}
        ]
        manifest: $manifest
        hashes: []
        checks: [package lint guard]
        workflow: (fixture check.yml)
        justfile: (fixture justfile)
    }
}

def head-snapshot []: nothing -> record {
    let base = base-snapshot
    $base | update hashes ($base.manifest | parse-manifest | select path sha256)
}

def rules [base: record, head: record]: nothing -> list<string> {
    violations $base $head | get rule | uniq
}

export def test-passes-when-nothing-changed [] {
    assert equal (rules (base-snapshot) (head-snapshot)) []
}

export def test-listed-file-changed-fails [] {
    let head = head-snapshot | update hashes {|row| $row.hashes | update sha256 {|entry| if $entry.path == 'go.mod' { 'ff' } else { $entry.sha256 } } }
    assert equal (rules (base-snapshot) $head) ['listed-file']
    assert str contains (violations (base-snapshot) $head | first | get detail) 'go.mod changed'
}

export def test-listed-file-deleted-or-renamed-fails [] {
    let head = head-snapshot | update hashes {|row| $row.hashes | update sha256 {|entry| if $entry.path == 'tools/sim/main.go' { null } else { $entry.sha256 } } }
    assert equal (rules (base-snapshot) $head) ['listed-file']
    assert str contains (violations (base-snapshot) $head | first | get detail) 'deleted or renamed'
}

export def test-listed-file-replaced-by-a-symlink-fails [] {
    let head = head-snapshot | update hashes {|row| $row.hashes | update sha256 {|entry| if $entry.path == 'go.mod' { 'symlink' } else { $entry.sha256 } } }
    assert equal (rules (base-snapshot) $head) ['listed-file']
}

export def test-appended-manifest-entries-pass-and-are-reported [] {
    let base = base-snapshot
    let head = head-snapshot | update manifest ($base.manifest + ('4' | fill --width 64 --character '4') + "  tools/new/new.go\n")
    assert equal (rules $base $head) []
    assert equal (additions $base $head) {entries: ['tools/new/new.go'], checks: []}
}

export def test-changed-manifest-entry-fails [] {
    let head = head-snapshot | update manifest ((fixture manifest.sha256) | str replace '1111' '9999')
    assert equal (rules (base-snapshot) $head) ['manifest']
}

export def test-removed-manifest-entry-fails [] {
    let head = head-snapshot | update manifest ((fixture manifest.sha256) | lines | skip 1 | str join "\n" | $in + "\n")
    assert equal (rules (base-snapshot) $head) ['manifest']
}

export def test-reordered-manifest-fails [] {
    let head = head-snapshot | update manifest ((fixture manifest.sha256) | lines | reverse | str join "\n" | $in + "\n")
    assert equal (rules (base-snapshot) $head) ['manifest']
}

export def test-changed-last-manifest-entry-fails [] {
    let head = head-snapshot | update manifest ((fixture manifest.sha256) | str replace "main.go\n" "main.go2\n")
    assert equal (rules (base-snapshot) $head) ['manifest']
}

export def test-manifest-scope-lists-the-go-reference-and-its-inputs [] {
    let listed = [
        go.mod go.sum version.go version_test.go
        cmd/togi/main.go cmd/togi/testdata/status.golden
        internal/tuner/rules.go internal/watch/testdata/frames/idle.txt internal/journal/testdata/fuzz/FuzzParse/a
        tools/bench/same.go tools/bench/suite.json tools/bench/machines/flat-hazard.json tools/bench/facts/target.jsonl.gz tools/bench/testdata/x.json
        tools/sim/main.go tools/audit/audit.go tools/fit/testdata/a tools/guard/guard.nu tools/guard/testdata/check.yml .github/workflows/guard.yml
    ]
    assert equal ($listed | where {|path| not (in-scope $path) }) []
}

export def test-manifest-scope-leaves-the-rest-out [] {
    let unlisted = [
        version.txt CHANGELOG.md flake.nix justfile README.md
        tools/release/main.go tools/board/testdata/board.golden tools/reviews/main.go tools/cover/main.go
        tools/bench/baseline.jsonl tools/bench/program.md tools/guard/manifest.sha256 docs/porting.md .github/workflows/check.yml
        internal/journal/README.md cmd/togi/doc.txt
    ]
    assert equal ($unlisted | where {|path| in-scope $path }) []
}

export def test-guard-file-changed-fails [] {
    let head = head-snapshot | update tree {|row| $row.tree | update object {|entry| if $entry.path == 'tools/guard/test.nu' { 't2' } else { $entry.object } } }
    assert equal (rules (base-snapshot) $head) ['guard-file']
}

export def test-guard-workflow-deleted-fails [] {
    let head = head-snapshot | update tree {|row| $row.tree | where path != '.github/workflows/guard.yml' }
    assert equal (rules (base-snapshot) $head) ['guard-file']
}

export def test-file-added-to-the-guard-directory-passes [] {
    let head = head-snapshot | update tree {|row| $row.tree | append {path: 'tools/guard/testdata/new.yml', object: 'n1'} }
    assert equal (rules (base-snapshot) $head) []
}

export def test-guard-file-changed-with-the-manifest-entry-updated-still-fails [] {
    let base = base-snapshot
    let head = head-snapshot
    | update tree {|row| $row.tree | update object {|entry| if $entry.path == 'tools/guard/guard.nu' { 'g2' } else { $entry.object } } }
    | update hashes {|row| $row.hashes | update sha256 {|entry| if $entry.path == 'tools/guard/guard.nu' { 'ee' } else { $entry.sha256 } } }
    assert equal (rules $base $head) ['guard-file']
}

export def test-added-flake-check-passes-and-is-reported [] {
    let base = base-snapshot
    let head = head-snapshot | update checks [package lint guard comparison]
    assert equal (rules $base $head) []
    assert equal (additions $base $head).checks ['comparison']
}

export def test-missing-flake-check-fails [] {
    let head = head-snapshot | update checks [package guard]
    assert equal (violations (base-snapshot) $head) [{rule: 'flake-check', detail: 'checks.x86_64-linux.lint is missing at the head'}]
}

export def test-renamed-flake-check-fails [] {
    let head = head-snapshot | update checks [package lint guard2]
    assert equal (rules (base-snapshot) $head) ['flake-check']
}

export def test-unrelated-workflow-job-change-passes [] {
    let head = head-snapshot | update workflow ((fixture check.yml) | str replace 'nix build ".#checks.x86_64-linux.$CHECK"' 'nix build --print-build-logs ".#checks.x86_64-linux.$CHECK"')
    assert equal (rules (base-snapshot) $head) []
}

export def test-comment-and-whitespace-changes-in-the-workflow-pass [] {
    let head = head-snapshot | update workflow ("# a comment\n" + ((fixture check.yml) | str replace --all 'ubuntu-26.04' "'ubuntu-26.04'"))
    assert equal (rules (base-snapshot) $head) []
}

export def test-discovery-job-change-fails [] {
    let head = head-snapshot | update workflow ((fixture check.yml) | str replace 'builtins.attrNames' 'builtins.filter')
    assert equal (violations (base-snapshot) $head | get detail) ['job eval in .github/workflows/check.yml changed']
}

export def test-aggregate-job-change-fails [] {
    let head = head-snapshot | update workflow ((fixture check.yml) | str replace 'test "$BUILD" = success' 'true')
    assert equal (violations (base-snapshot) $head | get detail) ['job check in .github/workflows/check.yml changed']
}

export def test-aggregate-job-removed-fails [] {
    let text = fixture check.yml
    let head = head-snapshot | update workflow ($text | split row "  check:\n" | first)
    assert equal (violations (base-snapshot) $head | get detail) ['job check in .github/workflows/check.yml is removed']
}

export def test-same-recipe-change-fails [] {
    let head = head-snapshot | update justfile ((fixture justfile) | str replace '--same "$dir"' '--same "$dir" --smoke')
    assert equal (violations (base-snapshot) $head | get detail) ['recipe same in justfile changed']
}

export def test-same-recipe-attribute-change-fails [] {
    let head = head-snapshot | update justfile ((fixture justfile) | str replace "[group('run')]\nsame" "same")
    assert equal (rules (base-snapshot) $head) ['same-recipe']
}

export def test-same-recipe-removed-fails [] {
    let head = head-snapshot | update justfile ((fixture justfile) | split row "# Prove decisions" | first)
    assert equal (rules (base-snapshot) $head) ['same-recipe']
}

export def test-other-recipe-change-passes [] {
    let head = head-snapshot | update justfile ((fixture justfile) | str replace 'go test ./... "$@"' 'go test -shuffle=on ./... "$@"' | str replace '--smoke' '--smoke --keep-going')
    assert equal (rules (base-snapshot) $head) []
}

export def test-recipe-extraction-stops-at-the-next-recipe [] {
    assert equal (recipe (fixture justfile) 'same') "[group('run')]\nsame base=\"origin/main\" *args: _dev-shell\n    dir=$(mktemp -d); trap 'rm -rf \"$dir\"' EXIT\n    go run ./tools/bench --same \"$dir\" \"${@:2}\""
    assert equal (recipe (fixture justfile) 'missing') null
}

export def test-approved-reports-and-passes [] {
    let found = [{rule: 'listed-file', detail: 'go.mod changed'}]
    assert equal (summary $found true) 'owner-approved: 1 protected changes (listed-file)'
    assert equal (summary $found false) '1 protected changes (listed-file)'
    assert equal (summary [] false) 'no protected asset changed'
}

def --wrapped git-in [dir: string, ...args] {
    with-env {GIT_AUTHOR_NAME: guard, GIT_AUTHOR_EMAIL: guard@example.invalid, GIT_COMMITTER_NAME: guard, GIT_COMMITTER_EMAIL: guard@example.invalid, GIT_CONFIG_GLOBAL: /dev/null, GIT_CONFIG_SYSTEM: /dev/null} {
        ^git -C $dir ...$args
    }
}

export def test-snapshot-reads-a-git-revision [] {
    let dir = mktemp --directory --tmpdir 'togi-guard-test.XXXXXX'
    git-in $dir init --quiet --initial-branch main
    mkdir ($dir | path join tools guard)
    'module x' | save ($dir | path join go.mod)
    fixture manifest.sha256 | save ($dir | path join tools guard manifest.sha256)
    git-in $dir add --all
    git-in $dir commit --quiet --message base
    'module y' | save --force ($dir | path join go.mod)
    ln -s go.mod ($dir | path join link)
    git-in $dir add --all
    git-in $dir commit --quiet --message head

    cd $dir
    let listed = ['go.mod' 'gone.go' 'link']
    let base = snapshot HEAD~1 [package] $listed
    let head = snapshot HEAD [package] $listed
    cd /
    rm -rf $dir

    assert equal ($base.hashes | where path == go.mod | first | get sha256) ('module x' | hash sha256)
    assert equal ($head.hashes | where path == go.mod | first | get sha256) ('module y' | hash sha256)
    assert equal ($head.hashes | where path == gone.go | first | get sha256) null
    assert equal ($head.hashes | where path == link | first | get sha256) 'symlink'
    assert equal ($base.hashes | where path == link | first | get sha256) null
    assert equal $base.manifest (fixture manifest.sha256)
    assert equal $base.workflow null
    assert equal $base.justfile null
}

def main [] {
    let tests = scope commands | where name =~ '^test-' | get name
    let module = $testdata | path dirname | path join test.nu
    let failures = $tests | each {|name|
        let result = nu --no-config-file --commands $"use ($module) *; ($name)" | complete
        if $result.exit_code == 0 { null } else { {test: $name, error: ($result.stderr | str trim)} }
    } | compact
    print $"($tests | length) tests, ($failures | length) failed"
    $failures | each {|failure| print $"FAIL ($failure.test): ($failure.error)" } | ignore
    if ($failures | is-not-empty) { exit 1 }
}
