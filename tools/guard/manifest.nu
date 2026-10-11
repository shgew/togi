# Prints the protected-asset manifest (docs/porting.md): `sha256  path` lines, sorted by path.
#
#   nu tools/guard/manifest.nu            every protected file of the working tree
#   nu tools/guard/manifest.nu --append   only the lines the committed manifest lacks, to append to it

use guard.nu [manifest_path parse-manifest]

const roots = [cmd/togi internal tools/sim tools/bench tools/trialfacts tools/replay tools/stats tools/fit tools/modelcheck tools/forecast tools/facts tools/audit]
const root_files = [go.mod go.sum version.go version_test.go]
const bench_inputs = [tools/bench/suite.json tools/bench/baseline.jsonl]
const bench_input_dirs = [tools/bench/machines/ tools/bench/facts/ tools/bench/gates/]
const guard_dirs = [tools/guard/]
const guard_files = [.github/workflows/guard.yml]

# Whether a tracked path is a protected comparison asset
export def in-scope [path: string]: nothing -> bool {
    let under_root = $roots | any {|root| $path | str starts-with $"($root)/" }
    let go_reference = $under_root and (($path | str ends-with '.go') or ($path | path basename) in [go.mod go.sum])
    let test_input = $under_root and ('/testdata/' in $path)
    let bench_input = ($path in $bench_inputs) or ($bench_input_dirs | any {|dir| $path | str starts-with $dir })
    let guard = ($path in $guard_files) or ($guard_dirs | any {|dir| $path | str starts-with $dir })
    ($path in $root_files) or $go_reference or $test_input or $bench_input or ($guard and $path != $manifest_path)
}

# The manifest lines of the working tree's tracked protected files
export def entries [root: string]: nothing -> table<sha256: string, path: string> {
    ^git -C $root ls-files -z
    | split row (char nul)
    | where {|path| $path != '' and (in-scope $path) }
    | sort
    | each {|path| {sha256: (open --raw ($root | path join $path) | hash sha256), path: $path} }
}

def render []: table -> string {
    each {|entry| $"($entry.sha256)  ($entry.path)\n" } | str join
}

def main [
    --append # print only the entries the committed manifest lacks; fail if an existing entry no longer matches
] {
    let root = ^git rev-parse --show-toplevel | str trim
    let current = entries $root
    if not $append {
        print --no-newline ($current | render)
        return
    }
    let committed = open --raw ($root | path join $manifest_path) | parse-manifest
    let stale = $committed
    | join --left $current path
    | where {|entry| $entry.sha256_ != $entry.sha256 }
    | get path
    if ($stale | is-not-empty) {
        error make --unspanned {msg: $"existing manifest entries no longer match the tree: ($stale | str join ', ')"}
    }
    print --no-newline ($current | where {|entry| $entry.path not-in $committed.path } | render)
}
