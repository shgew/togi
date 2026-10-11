# The guard: fails a pull request that changes a protected comparison asset (docs/porting.md).
#
#   nu tools/guard/guard.nu <base> <head> [--approved]
#
# It reads both revisions only as git data, except for `nix eval` of each tree's flake check names in pure mode.

export const manifest_path = 'tools/guard/manifest.sha256'
export const workflow_path = '.github/workflows/check.yml'
export const decision_jobs = [eval check]
export const justfile_path = 'justfile'
export const protected_recipe = 'same'
export const guard_prefixes = ['tools/guard/' '.github/workflows/guard.yml']

# List every tracked file of a revision with its mode and blob
export def tree [rev: string]: nothing -> table<mode: string, type: string, object: string, path: string> {
    ^git rev-parse --verify --quiet $"($rev)^{commit}" | ignore
    ^git ls-tree -r -z --full-tree $rev
    | split row (char nul)
    | where $it != ''
    | parse "{mode} {type} {object}\t{path}"
}

def blob-text [tree: table, path: string]: nothing -> any {
    let entry = $tree | where path == $path
    if ($entry | is-empty) { return null }
    ^git cat-file blob ($entry | first | get object) | into binary | decode utf-8
}

# Parse a manifest's `sha256  path` lines
export def parse-manifest []: string -> table<sha256: string, path: string> {
    lines | where $it != '' | parse '{sha256}  {path}'
}

# The sha256 of each path in a tree: null when absent, `symlink` or `submodule` when it is no regular file
export def hashes [tree: table, paths: list<string>]: nothing -> table<path: string, sha256: any> {
    $paths
    | wrap path
    | join --left $tree path
    | each {|row|
        let sha256 = match $row.mode? {
            null => null
            '100644' | '100755' => (^git cat-file blob $row.object | hash sha256)
            '120000' => 'symlink'
            _ => 'submodule'
        }
        {path: $row.path, sha256: $sha256}
    }
}

# The flake check names of a revision's `checks.x86_64-linux`, evaluated in pure mode without building anything
export def flake-checks [rev: string]: nothing -> list<string> {
    ^git rev-parse --verify --quiet $"($rev)^{commit}" | ignore
    let dir = mktemp --directory --tmpdir 'togi-guard.XXXXXX'
    try {
        ^git archive --format=tar $rev | ^tar -x -C $dir
        ^nix eval --json --no-write-lock-file $"path:($dir)#checks.x86_64-linux" --apply builtins.attrNames
        | from json
    } finally {
        rm -rf $dir
    }
}

# What the guard compares at one revision; `checks` comes from flake-checks, `listed` names the paths to hash
export def snapshot [rev: string, checks: list<string>, listed: list<string>]: nothing -> record {
    let tree = tree $rev
    {
        tree: ($tree | select path object)
        manifest: (blob-text $tree $manifest_path | default '')
        hashes: (hashes $tree $listed)
        checks: $checks
        workflow: (blob-text $tree $workflow_path)
        justfile: (blob-text $tree $justfile_path)
    }
}

# The named jobs of a workflow text, each normalized to compact JSON; a job the workflow lacks is null
export def workflow-jobs [text: any, names: list<string>]: nothing -> record {
    let jobs = if $text == null { {} } else { $text | from yaml | get -o jobs | default {} }
    $names | reduce --fold {} {|name, jobs_by_name|
        let job = $jobs | get -o $name
        $jobs_by_name | insert $name (if $job == null { null } else { $job | to json --raw })
    }
}

# A recipe's attribute lines, header and body; null when the justfile text lacks it
export def recipe [text: any, name: string]: nothing -> any {
    if $text == null { return null }
    let lines = $text | lines
    let header = $lines | enumerate | where {|line| $line.item =~ $"^($name)[ :]" } | get -o 0.index
    if $header == null { return null }
    let attributes = $lines | first $header | reverse | take while {|line| $line | str starts-with '[' } | reverse
    let body = $lines | skip ($header + 1) | take while {|line| $line == '' or ($line =~ '^[ \t]') }
    let body = $body | reverse | skip while {|line| $line == '' } | reverse
    [...$attributes ($lines | get $header) ...$body] | str join "\n"
}

def guard-path []: string -> bool {
    let path = $in
    $guard_prefixes | any {|prefix| ($path == $prefix) or (($prefix | str ends-with '/') and ($path | str starts-with $prefix)) }
}

# Every rule the head breaks against the base, as {rule, detail} rows; an empty table passes
export def violations [base: record, head: record]: nothing -> table<rule: string, detail: string> {
    let listed = $base.manifest | parse-manifest | where {|entry| not ($entry.path | guard-path) }
    let changed_files = $listed
    | join --left ($head.hashes | rename path head) path
    | where {|entry| $entry.head != $entry.sha256 }
    | each {|entry|
        let detail = match $entry.head {
            null => $"($entry.path) is deleted or renamed"
            'symlink' | 'submodule' => $"($entry.path) is now a ($entry.head)"
            _ => $"($entry.path) changed"
        }
        {rule: 'listed-file', detail: $detail}
    }

    let manifest = if ($head.manifest | str starts-with $base.manifest) { [] } else {
        [{rule: 'manifest', detail: $"($manifest_path) does not start with the base's manifest: an existing entry changed"}]
    }

    let guard_files = $base.tree
    | where {|entry| ($entry.path != $manifest_path) and ($entry.path | guard-path) }
    | join --left ($head.tree | rename path head) path
    | where {|entry| $entry.head != $entry.object }
    | each {|entry| {rule: 'guard-file', detail: $"($entry.path) (if $entry.head == null { 'is deleted or renamed' } else { 'changed' })"} }

    let checks = $base.checks
    | where {|name| $name not-in $head.checks }
    | each {|name| {rule: 'flake-check', detail: $"checks.x86_64-linux.($name) is missing at the head"} }

    let base_jobs = workflow-jobs $base.workflow $decision_jobs
    let head_jobs = workflow-jobs $head.workflow $decision_jobs
    let jobs = $decision_jobs
    | where {|name| ($base_jobs | get $name) != null and ($base_jobs | get $name) != ($head_jobs | get $name) }
    | each {|name| {rule: 'decision-path', detail: $"job ($name) in ($workflow_path) (if ($head_jobs | get $name) == null { 'is removed' } else { 'changed' })"} }

    let base_recipe = recipe $base.justfile $protected_recipe
    let head_recipe = recipe $head.justfile $protected_recipe
    let recipes = if $base_recipe == null or $base_recipe == $head_recipe { [] } else {
        [{rule: 'same-recipe', detail: $"recipe ($protected_recipe) in ($justfile_path) (if $head_recipe == null { 'is removed' } else { 'changed' })"}]
    }

    [...$changed_files ...$manifest ...$guard_files ...$checks ...$jobs ...$recipes]
}

# What the head adds that the guard allows: appended manifest entries and new flake checks
export def additions [base: record, head: record]: nothing -> record<entries: list<string>, checks: list<string>> {
    let appended = if ($head.manifest | str starts-with $base.manifest) {
        $head.manifest | str substring ($base.manifest | str length).. | parse-manifest | get path
    } else { [] }
    {entries: $appended, checks: ($head.checks | where {|name| $name not-in $base.checks })}
}

# The one-line verdict, also the commit status description
export def summary [found: table, approved: bool]: nothing -> string {
    if ($found | is-empty) { return 'no protected asset changed' }
    let rules = $found | get rule | uniq | str join ', '
    let count = $found | length
    if $approved { $"owner-approved: ($count) protected changes \(($rules))" } else { $"($count) protected changes \(($rules))" }
}

# Compare head against base; exit 1 on a protected change unless --approved
export def main [
    base: string # base revision, whose manifest and checks are protected
    head: string # head revision, read only as data
    --approved # the pull request carries the owner-approved label: report changes and pass
] {
    let listed = ^git cat-file blob $"($base):($manifest_path)" | complete | if $in.exit_code == 0 { $in.stdout } else { '' } | parse-manifest | get path
    let before = snapshot $base (flake-checks $base) []
    let after = snapshot $head (flake-checks $head) $listed
    let found = violations $before $after
    let added = additions $before $after
    for row in $found { print $"($row.rule): ($row.detail)" }
    for path in ($added.entries | first 20) { print $"added manifest entry: ($path)" }
    if ($added.entries | length) > 20 { print $"added manifest entries: ($added.entries | length) in total" }
    for name in $added.checks { print $"added flake check: ($name)" }
    print (summary $found $approved)
    if ($found | is-not-empty) and not $approved { exit 1 }
}
