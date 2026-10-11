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
            {path: 'go.mod', mode: '100644', object: 'a1'}
            {path: 'tools/guard/guard.nu', mode: '100644', object: 'g1'}
            {path: 'tools/guard/test.nu', mode: '100644', object: 't1'}
            {path: '.github/workflows/guard.yml', mode: '100644', object: 'w1'}
            {path: 'tools/sim/main.go', mode: '100644', object: 's1'}
        ]
        manifest: $manifest
        hashes: []
        checks: [x86_64-linux.package x86_64-linux.lint x86_64-linux.guard aarch64-darwin.package]
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
        tools/bench/same.go tools/bench/suite.json tools/bench/baseline.jsonl tools/bench/gates/ruleset-11/a.json tools/bench/machines/flat-hazard.json tools/bench/facts/target.jsonl.gz tools/bench/testdata/x.json
        tools/sim/main.go tools/audit/audit.go tools/fit/testdata/a tools/guard/guard.nu tools/guard/testdata/check.yml .github/workflows/guard.yml
    ]
    assert equal ($listed | where {|path| not (in-scope $path) }) []
}

export def test-manifest-scope-leaves-the-rest-out [] {
    let unlisted = [
        version.txt CHANGELOG.md flake.nix justfile README.md
        tools/release/main.go tools/board/testdata/board.golden tools/reviews/main.go tools/cover/main.go
        tools/bench/program.md tools/guard/manifest.sha256 docs/porting.md .github/workflows/check.yml
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
    let head = head-snapshot | update tree {|row| $row.tree | append {path: 'tools/guard/testdata/new.yml', mode: '100644', object: 'n1'} }
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
    let head = head-snapshot | update checks [x86_64-linux.package x86_64-linux.lint x86_64-linux.guard aarch64-darwin.package x86_64-linux.comparison]
    assert equal (rules $base $head) []
    assert equal (additions $base $head).checks ['x86_64-linux.comparison']
}

export def test-missing-flake-check-fails [] {
    let head = head-snapshot | update checks [x86_64-linux.package x86_64-linux.guard aarch64-darwin.package]
    assert equal (violations (base-snapshot) $head) [{rule: 'flake-check', detail: 'checks.x86_64-linux.lint is missing at the head'}]
}

export def test-renamed-flake-check-fails [] {
    let head = head-snapshot | update checks [x86_64-linux.package x86_64-linux.lint x86_64-linux.guard2 aarch64-darwin.package]
    assert equal (rules (base-snapshot) $head) ['flake-check']
}

export def test-macos-job-change-fails [] {
    let head = head-snapshot | update workflow ((fixture check.yml) | str replace 'nix build ".#checks.aarch64-darwin.package"' 'true')
    assert equal (violations (base-snapshot) $head | get detail) ['job macos in .github/workflows/check.yml changed']
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
    let base = snapshot HEAD~1 [x86_64-linux.package] $listed
    let head = snapshot HEAD [x86_64-linux.package] $listed
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

def entry-line [path: string, content: string]: nothing -> string {
    $"($content | hash sha256)  ($path)\n"
}

def zeros [char: string]: nothing -> string {
    $char | fill --width 64 --character $char
}

# A head whose manifest appends one entry for a file the head has, with the matching hash
def appending-head [path: string, content: string]: nothing -> record {
    let base = base-snapshot
    head-snapshot
    | update manifest ($base.manifest + (entry-line $path $content))
    | update hashes {|row| $row.hashes | append {path: $path, sha256: ($content | hash sha256)} }
}

export def test-appended-entry-with-a-matching-file-passes [] {
    let head = appending-head 'tools/new/new.go' 'package new'
    assert equal (rules (base-snapshot) $head) []
    assert equal (additions (base-snapshot) $head).entries ['tools/new/new.go']
}

export def test-appended-entry-with-a-wrong-hash-fails [] {
    let base = base-snapshot
    let head = head-snapshot
    | update manifest ($base.manifest + (zeros '4') + "  tools/new/new.go\n")
    | update hashes {|row| $row.hashes | append {path: 'tools/new/new.go', sha256: ('package new' | hash sha256)} }
    assert equal (violations $base $head | get detail) ["appended entry tools/new/new.go does not match the file's sha256 at the head"]
}

export def test-appended-entry-for-a-missing-file-fails [] {
    let base = base-snapshot
    let head = head-snapshot
    | update manifest ($base.manifest + (zeros '4') + "  tools/new/new.go\n")
    | update hashes {|row| $row.hashes | append {path: 'tools/new/new.go', sha256: null} }
    assert equal (violations $base $head | get detail) ['appended entry tools/new/new.go is not a file at the head']
}

export def test-appended-entry-for-a-symlink-fails [] {
    let head = appending-head 'tools/new/new.go' 'package new' | update hashes {|row| $row.hashes | update sha256 {|entry| if $entry.path == 'tools/new/new.go' { 'symlink' } else { $entry.sha256 } } }
    assert equal (violations (base-snapshot) $head | get detail) ['appended entry tools/new/new.go is a symlink at the head']
}

export def test-appended-section-without-a-trailing-newline-fails [] {
    let head = appending-head 'tools/new/new.go' 'package new' | update manifest {|row| $row.manifest | str trim --right }
    assert equal (violations (base-snapshot) $head | get detail) ['the entries appended to tools/guard/manifest.sha256 do not end with a newline']
}

export def test-appended-entry-already-listed-fails [] {
    let base = base-snapshot
    let head = head-snapshot | update manifest ($base.manifest + (entry-line 'go.mod' 'module x'))
    assert equal (rules $base $head) ['manifest']
    assert str contains (violations $base $head | first | get detail) 'go.mod is already listed'
}

export def test-appended-entry-twice-fails [] {
    let base = base-snapshot
    let line = entry-line 'tools/new/new.go' 'package new'
    let head = appending-head 'tools/new/new.go' 'package new' | update manifest ($base.manifest + $line + $line)
    assert equal (violations $base $head | get detail) ['appended entry tools/new/new.go is already listed']
}

export def test-appended-line-that-is-no-entry-fails [] {
    let base = base-snapshot
    let head = head-snapshot | update manifest ($base.manifest + "not an entry\n")
    assert equal (rules $base $head) ['manifest']
    assert str contains (violations $base $head | first | get detail) 'is no `<sha256>  <path>` entry'
}

export def test-appended-line-with-an-uppercase-hash-fails [] {
    let base = base-snapshot
    let head = head-snapshot | update manifest ($base.manifest + (zeros 'A') + "  tools/new/new.go\n")
    assert equal (rules $base $head) ['manifest']
}

export def test-base-manifest-without-a-trailing-newline-fails [] {
    let base = base-snapshot | update manifest {|row| $row.manifest | str trim --right }
    let head = head-snapshot | update manifest {|row| ($row.manifest | str trim --right) + ".bak\n" }
    assert equal (rules $base $head) ['manifest']
    assert str contains (violations $base $head | first | get detail) 'does not end with a newline'
}

export def test-guard-file-replaced-by-a-symlink-with-the-same-blob-fails [] {
    let head = head-snapshot | update tree {|row| $row.tree | update mode {|entry| if $entry.path == '.github/workflows/guard.yml' { '120000' } else { $entry.mode } } }
    assert equal (violations (base-snapshot) $head | get detail) ['.github/workflows/guard.yml changed']
}

export def test-guard-file-made-executable-fails [] {
    let head = head-snapshot | update tree {|row| $row.tree | update mode {|entry| if $entry.path == 'tools/guard/guard.nu' { '100755' } else { $entry.mode } } }
    assert equal (rules (base-snapshot) $head) ['guard-file']
}

export def test-dropped-darwin-check-fails [] {
    let head = head-snapshot | update checks [x86_64-linux.package x86_64-linux.lint x86_64-linux.guard]
    assert equal (violations (base-snapshot) $head | get detail) ['checks.aarch64-darwin.package is missing at the head']
}

export def test-dropped-system-fails [] {
    let base = base-snapshot | update checks [x86_64-linux.package aarch64-linux.package]
    let head = head-snapshot | update checks [x86_64-linux.package]
    assert equal (violations $base $head | get detail) ['checks.aarch64-linux.package is missing at the head']
}

export def test-check-names-flatten-every-system [] {
    assert equal ({x86_64-linux: [package lint], aarch64-darwin: [package]} | check-names) [x86_64-linux.package x86_64-linux.lint aarch64-darwin.package]
}

export def test-flake-eval-disables-import-from-derivation [] {
    let args = checks-eval-args /tmp/x
    let at = $args | enumerate | where item == '--option' | first | get index
    assert equal ($args | slice $at..($at + 2)) [--option allow-import-from-derivation 'false']
    assert ($args | any {|arg| $arg == 'path:/tmp/x#checks' })
}

export def test-build-job-matrix-change-fails [] {
    let head = head-snapshot | update workflow ((fixture check.yml) | str replace 'fromJSON(needs.eval.outputs.checks)' '[package]')
    assert equal (violations (base-snapshot) $head | get detail) ['job build in .github/workflows/check.yml changed']
}

export def test-build-job-step-removed-fails [] {
    let head = head-snapshot | update workflow ((fixture check.yml) | str replace '      - run: nix build ".#checks.x86_64-linux.$CHECK"' '      - run: "true"')
    assert equal (rules (base-snapshot) $head) ['decision-path']
}

export def test-workflow-env-change-fails [] {
    let head = head-snapshot | update workflow ((fixture check.yml) | str replace 'accept-flake-config = false' "extra-substituters = https://example.invalid")
    assert equal (violations (base-snapshot) $head | get detail) ['top-level key env in .github/workflows/check.yml changed']
}

export def test-workflow-top-level-key-removed-or-added-fails [] {
    let removed = head-snapshot | update workflow ((fixture check.yml) | str replace "permissions:\n  contents: read\n\n" '')
    assert equal (violations (base-snapshot) $removed | get detail) ['top-level key permissions in .github/workflows/check.yml is removed']
    let added = head-snapshot | update workflow ((fixture check.yml) | str replace "jobs:\n" "defaults:\n  run:\n    shell: bash\n\njobs:\n")
    assert equal (violations (base-snapshot) $added | get detail) ['top-level key defaults in .github/workflows/check.yml is added']
}

export def test-workflow-trigger-change-fails [] {
    let head = head-snapshot | update workflow ((fixture check.yml) | str replace "  push:\n    branches: [main]\n" '')
    assert equal (violations (base-snapshot) $head | get detail) ['top-level key on in .github/workflows/check.yml changed']
}

export def test-new-workflow-job-passes [] {
    let head = head-snapshot | update workflow ((fixture check.yml) + "\n  extra:\n    runs-on: ubuntu-26.04\n    steps:\n      - run: true\n")
    assert equal (rules (base-snapshot) $head) []
}

export def test-reordered-workflow-keys-pass [] {
    let reordered = (fixture check.yml) | str replace "    runs-on: ubuntu-26.04\n    steps:\n      - run: nix build \".#checks.x86_64-linux.$CHECK\"" "    steps:\n      - run: nix build \".#checks.x86_64-linux.$CHECK\"\n    runs-on: ubuntu-26.04"
    assert ($reordered != (fixture check.yml))
    let head = head-snapshot | update workflow ($reordered | str replace "permissions:\n  contents: read\n\nenv:\n  NIX_CONFIG: accept-flake-config = false\n" "env:\n  NIX_CONFIG: accept-flake-config = false\n\npermissions:\n  contents: read\n")
    assert equal (rules (base-snapshot) $head) []
}

export def test-canonical-sorts-keys-at-every-depth-and-keeps-list-order [] {
    assert equal ({b: {z: 1, a: [{y: 1, x: 2}, 3]}, a: 0} | canonical | to json --raw) '{"a":0,"b":{"a":[{"x":2,"y":1},3],"z":1}}'
}

export def test-canonical-keeps-null-list-elements [] {
    assert equal ([build macos null] | canonical | to json --raw) '["build","macos",null]'
}

export def test-null-added-to-a-job-list-fails [] {
    let head = head-snapshot | update workflow ((fixture check.yml) | str replace 'needs: [build, macos]' 'needs: [build, macos, null]')
    assert equal (violations (base-snapshot) $head | get detail) ['job check in .github/workflows/check.yml changed']
}

export def test-justfile-set-line-change-fails [] {
    let head = head-snapshot | update justfile ((fixture justfile) | str replace 'set shell := ["bash", "-euo", "pipefail", "-c"]' 'set shell := ["true"]')
    assert equal (violations (base-snapshot) $head | get detail) ['justfile settings, exports or imports changed']
}

export def test-justfile-set-line-added-fails [] {
    let head = head-snapshot | update justfile ("set dotenv-load\n" + (fixture justfile))
    assert equal (rules (base-snapshot) $head) ['same-recipe']
}

export def test-justfile-export-added-fails [] {
    let head = head-snapshot | update justfile ("export BASH_ENV := justfile_directory() + \"/skip.sh\"\n" + (fixture justfile))
    assert equal (violations (base-snapshot) $head | get detail) ['justfile settings, exports or imports changed']
}

export def test-justfile-import-added-fails [] {
    for line in ["import 'skip.just'" "import? 'skip.just'"] {
        let head = head-snapshot | update justfile ($"($line)\n" + (fixture justfile))
        assert equal (violations (base-snapshot) $head | get detail) ['justfile settings, exports or imports changed']
    }
}

export def test-justfile-module-added-fails [] {
    for line in ['mod skip' 'mod? skip'] {
        let head = head-snapshot | update justfile ($"($line)\n" + (fixture justfile))
        assert equal (rules (base-snapshot) $head) ['same-recipe']
    }
}

# A fresh repository with the base files committed on `main`
def new-repo [files: record]: nothing -> string {
    let dir = mktemp --directory --tmpdir 'togi-guard-test.XXXXXX'
    git-in $dir init --quiet --initial-branch main
    write-files $dir $files
    commit-all $dir base
    $dir
}

def write-files [dir: string, files: record] {
    for file in ($files | transpose path content) {
        mkdir ($dir | path join $file.path | path dirname)
        $file.content | save --force ($dir | path join $file.path)
    }
}

def commit-all [dir: string, message: string] {
    git-in $dir add --all
    git-in $dir commit --quiet --message $message
}

# The guard's comparison of two branches of a repository, as the violations found
def compare-branches [dir: string, base: string, head: string]: nothing -> any {
    cd $dir
    let merged = merge-tree $base $head
    if $merged == null { return null }
    (compare $base $merged [] []).found
}

def listed-repo []: nothing -> string {
    new-repo {
        go.mod: 'module x'
        'tools/guard/manifest.sha256': (entry-line go.mod 'module x')
    }
}

export def test-additive-head-passes-after-the-base-branch-moved [] {
    let dir = listed-repo
    git-in $dir switch --quiet --create feature
    write-files $dir {'docs/x.md': 'unrelated'}
    commit-all $dir feature
    git-in $dir switch --quiet main
    write-files $dir {
        'internal/new.go': 'package new'
        'tools/guard/manifest.sha256': ((entry-line go.mod 'module x') + (entry-line internal/new.go 'package new'))
    }
    commit-all $dir 'main moves ahead'
    let found = compare-branches $dir main feature
    cd /
    rm -rf $dir
    assert equal $found []
}

export def test-head-editing-a-file-the-moved-base-listed-fails [] {
    let dir = new-repo {
        go.mod: 'module x'
        'internal/a.go': 'package a'
        'tools/guard/manifest.sha256': (entry-line go.mod 'module x')
    }
    git-in $dir switch --quiet --create feature
    write-files $dir {'internal/a.go': 'package changed'}
    commit-all $dir feature
    git-in $dir switch --quiet main
    write-files $dir {'tools/guard/manifest.sha256': ((entry-line go.mod 'module x') + (entry-line internal/a.go 'package a'))}
    commit-all $dir 'main lists a.go'
    let found = compare-branches $dir main feature
    cd /
    rm -rf $dir
    assert equal ($found | select rule detail) [{rule: 'listed-file', detail: 'internal/a.go changed'}]
}

export def test-conflicting-head-reports-no-merge [] {
    let dir = listed-repo
    git-in $dir switch --quiet --create feature
    write-files $dir {go.mod: 'module feature'}
    commit-all $dir feature
    git-in $dir switch --quiet main
    write-files $dir {go.mod: 'module main'}
    commit-all $dir 'main edits too'
    let merged = (cd $dir; merge-tree main feature)
    cd /
    rm -rf $dir
    assert equal $merged null
}

export def test-real-deletion-of-a-listed-file-fails [] {
    let dir = listed-repo
    git-in $dir switch --quiet --create feature
    git-in $dir rm --quiet go.mod
    commit-all $dir feature
    let found = compare-branches $dir main feature
    cd /
    rm -rf $dir
    assert equal ($found | select rule detail) [{rule: 'listed-file', detail: 'go.mod is deleted or renamed'}]
}

export def test-real-rename-of-a-listed-file-fails [] {
    let dir = listed-repo
    git-in $dir switch --quiet --create feature
    git-in $dir mv go.mod go2.mod
    commit-all $dir feature
    let found = compare-branches $dir main feature
    cd /
    rm -rf $dir
    assert equal ($found | select rule detail) [{rule: 'listed-file', detail: 'go.mod is deleted or renamed'}]
}

export def test-real-append-hashes-the-appended-path [] {
    let dir = listed-repo
    git-in $dir switch --quiet --create feature
    write-files $dir {
        'internal/new.go': 'package new'
        'tools/guard/manifest.sha256': ((entry-line go.mod 'module x') + (entry-line internal/new.go 'package other'))
    }
    commit-all $dir feature
    let found = compare-branches $dir main feature
    cd /
    rm -rf $dir
    assert equal ($found | get detail) ["appended entry internal/new.go does not match the file's sha256 at the head"]
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

