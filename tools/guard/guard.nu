# The guard: fails a pull request that changes a protected comparison asset (docs/porting.md).
#
#   nu tools/guard/guard.nu <base> <head> [--approved]
#
# It compares the base against the tree that merging the head into the base produces, reading both only as
# git data, except for `nix eval` of each tree's flake check names in pure mode without import-from-derivation.

export const manifest_path = 'tools/guard/manifest.sha256'
export const workflow_path = '.github/workflows/check.yml'
export const decision_jobs = [eval build check]
export const justfile_path = 'justfile'
export const protected_recipe = 'same'
export const guard_prefixes = ['tools/guard/' '.github/workflows/guard.yml']

# The arguments of the `nix eval` that lists every system's flake check names of the flake in `dir`
export def checks-eval-args [dir: string]: nothing -> list<string> {
    [
        eval --json --no-write-lock-file --option allow-import-from-derivation 'false'
        $"path:($dir)#checks"
        --apply 'checks: builtins.mapAttrs (system: checks: builtins.attrNames checks) checks'
    ]
}

# The tree a git tree-ish names
def verify-tree [rev: string]: nothing -> nothing {
    ^git rev-parse --verify --quiet $"($rev)^{tree}" | ignore
}

# List every tracked file of a tree-ish with its mode and blob
export def tree [rev: string]: nothing -> table<mode: string, type: string, object: string, path: string> {
    verify-tree $rev
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

# The text a head manifest appends to the base manifest; null when it does not start with the base's bytes
export def appended [base_manifest: string, head_manifest: string]: nothing -> any {
    if not ($head_manifest | str starts-with $base_manifest) { return null }
    $head_manifest | str substring ($base_manifest | str length)..
}

# The lines of an appended manifest section; `sha256` and `path` are null on a line that is no `<sha256>  <path>` entry
export def appended-entries [section: string]: nothing -> table<line: string, sha256: any, path: any> {
    $section | lines | each {|line|
        let entry = $line | parse --regex '^(?<sha256>[0-9a-f]{64})  (?<path>.+)$'
        if ($entry | is-empty) { {line: $line, sha256: null, path: null} } else { {line: $line, sha256: $entry.0.sha256, path: $entry.0.path} }
    }
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

# Flatten `nix eval`'s {system: [name]} record into `system.name` strings
export def check-names []: record -> list<string> {
    transpose system names | each {|row| $row.names | each {|name| $"($row.system).($name)" } } | flatten
}

# The flake check names of every system of a tree-ish's `checks`, evaluated in pure mode without building anything
export def flake-checks [rev: string]: nothing -> list<string> {
    verify-tree $rev
    let dir = mktemp --directory --tmpdir 'togi-guard.XXXXXX'
    try {
        ^git archive --format=tar $rev | ^tar -x -C $dir
        ^nix ...(checks-eval-args $dir) | from json | check-names
    } finally {
        rm -rf $dir
    }
}

# The tree that merging head into base produces, or null when they conflict
export def merge-tree [base: string, head: string]: nothing -> any {
    let result = ^git merge-tree --write-tree --no-messages $base $head | complete
    match $result.exit_code {
        0 => ($result.stdout | lines | first)
        1 => null
        _ => { error make --unspanned {msg: $"git merge-tree failed: ($result.stderr | str trim)"} }
    }
}

# What the guard compares at one tree-ish; `checks` comes from flake-checks, `listed` names the paths to hash
export def snapshot [rev: string, checks: list<string>, listed: list<string>]: nothing -> record {
    let tree = tree $rev
    {
        tree: ($tree | select path mode object)
        manifest: (blob-text $tree $manifest_path | default '')
        hashes: (hashes $tree $listed)
        checks: $checks
        workflow: (blob-text $tree $workflow_path)
        justfile: (blob-text $tree $justfile_path)
    }
}

# A parsed YAML value with every mapping's keys sorted, at every depth; lists keep their order
export def canonical []: any -> any {
    let value = $in
    match ($value | describe | str replace --regex '<.*' '') {
        'record' => ($value | transpose key value | sort-by key | reduce --fold {} {|row, sorted| $sorted | insert $row.key ($row.value | canonical) })
        'list' | 'table' => ($value | each {|item| $item | canonical })
        _ => $value
    }
}

def parse-workflow [text: any]: nothing -> record {
    if $text == null { {} } else { $text | from yaml | default {} }
}

# The named jobs of a workflow text, each normalized to compact JSON with sorted keys; a job the workflow lacks is null
export def workflow-jobs [text: any, names: list<string>]: nothing -> record {
    let jobs = parse-workflow $text | get -o jobs | default {}
    $names | reduce --fold {} {|name, jobs_by_name|
        let job = $jobs | get -o $name
        $jobs_by_name | insert $name (if $job == null { null } else { $job | canonical | to json --raw })
    }
}

# A workflow's top-level keys except `jobs`, each normalized to compact JSON with sorted keys
export def workflow-settings [text: any]: nothing -> record {
    parse-workflow $text | reject -o jobs | transpose key value | reduce --fold {} {|row, settings|
        $settings | insert $row.key ($row.value | canonical | to json --raw)
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

# A justfile's top-level `set` lines, in order
export def settings [text: any]: nothing -> list<string> {
    if $text == null { return [] }
    $text | lines | where {|line| $line =~ '^set[ \t]' }
}

def guard-path []: string -> bool {
    let path = $in
    $guard_prefixes | any {|prefix| ($path == $prefix) or (($prefix | str ends-with '/') and ($path | str starts-with $prefix)) }
}

def manifest-violation [detail: string]: nothing -> record {
    {rule: 'manifest', detail: $detail}
}

# The manifest rule: the head manifest is the base manifest plus well-formed entries that match the head's files
def manifest-violations [base: record, head: record]: nothing -> list<record> {
    if $base.manifest != '' and not ($base.manifest | str ends-with "\n") {
        return [(manifest-violation $"the base's ($manifest_path) does not end with a newline, so an append would extend its last entry")]
    }
    let section = appended $base.manifest $head.manifest
    if $section == null {
        return [(manifest-violation $"($manifest_path) does not start with the base's manifest: an existing entry changed")]
    }
    if $section == '' { return [] }
    let newline = if ($section | str ends-with "\n") { [] } else {
        [(manifest-violation $"the entries appended to ($manifest_path) do not end with a newline")]
    }
    let base_paths = $base.manifest | parse-manifest | get path
    let entries = appended-entries $section | enumerate
    let malformed = $entries | where item.path == null | each {|entry|
        manifest-violation $"appended line ($entry.index + 1) is no `<sha256>  <path>` entry: ($entry.item.line)"
    }
    let valid = $entries | where item.path != null
    let duplicates = $valid | each {|entry|
        let earlier = $valid | where index < $entry.index | get item.path
        if $entry.item.path in $base_paths or $entry.item.path in $earlier {
            manifest-violation $"appended entry ($entry.item.path) is already listed"
        }
    } | compact
    let contents = $valid | each {|entry|
        let path = $entry.item.path
        let found = $head.hashes | where path == $path | get -o 0.sha256
        match $found {
            null => (manifest-violation $"appended entry ($path) is not a file at the head")
            'symlink' | 'submodule' => (manifest-violation $"appended entry ($path) is a ($found) at the head")
            $sha256 if $sha256 != $entry.item.sha256 => (manifest-violation $"appended entry ($path) does not match the file's sha256 at the head")
            _ => null
        }
    } | compact
    [...$newline ...$malformed ...$duplicates ...$contents]
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

    let manifest = manifest-violations $base $head

    let guard_files = $base.tree
    | where {|entry| ($entry.path != $manifest_path) and ($entry.path | guard-path) }
    | join --left ($head.tree | rename path head_mode head_object) path
    | where {|entry| $entry.head_mode != $entry.mode or $entry.head_object != $entry.object }
    | each {|entry| {rule: 'guard-file', detail: $"($entry.path) (if $entry.head_object == null { 'is deleted or renamed' } else { 'changed' })"} }

    let checks = $base.checks
    | where {|name| $name not-in $head.checks }
    | each {|name| {rule: 'flake-check', detail: $"checks.($name) is missing at the head"} }

    let base_jobs = workflow-jobs $base.workflow $decision_jobs
    let head_jobs = workflow-jobs $head.workflow $decision_jobs
    let jobs = $decision_jobs
    | where {|name| ($base_jobs | get $name) != null and ($base_jobs | get $name) != ($head_jobs | get $name) }
    | each {|name| {rule: 'decision-path', detail: $"job ($name) in ($workflow_path) (if ($head_jobs | get $name) == null { 'is removed' } else { 'changed' })"} }
    let base_settings = workflow-settings $base.workflow
    let head_settings = workflow-settings $head.workflow
    let workflow_keys = [...($base_settings | columns) ...($head_settings | columns)] | uniq
    let settings = $workflow_keys
    | where {|key| ($base_settings | get -o $key) != ($head_settings | get -o $key) }
    | each {|key|
        let change = if ($head_settings | get -o $key) == null { 'is removed' } else if ($base_settings | get -o $key) == null { 'is added' } else { 'changed' }
        {rule: 'decision-path', detail: $"top-level key ($key) in ($workflow_path) ($change)"}
    }

    let base_recipe = recipe $base.justfile $protected_recipe
    let head_recipe = recipe $head.justfile $protected_recipe
    let recipes = if $base_recipe == null { [] } else {
        let recipe_change = if $base_recipe == $head_recipe { [] } else {
            [{rule: 'same-recipe', detail: $"recipe ($protected_recipe) in ($justfile_path) (if $head_recipe == null { 'is removed' } else { 'changed' })"}]
        }
        let settings_change = if (settings $base.justfile) == (settings $head.justfile) { [] } else {
            [{rule: 'same-recipe', detail: $"($justfile_path) settings changed"}]
        }
        [...$recipe_change ...$settings_change]
    }

    [...$changed_files ...$manifest ...$guard_files ...$checks ...$jobs ...$settings ...$recipes]
}

# What the head adds that the guard allows: appended manifest entries and new flake checks
export def additions [base: record, head: record]: nothing -> record<entries: list<string>, checks: list<string>> {
    let section = appended $base.manifest $head.manifest
    let entries = if $section == null { [] } else { appended-entries $section | where path != null | get path }
    {entries: $entries, checks: ($head.checks | where {|name| $name not-in $base.checks })}
}

def manifest-at [rev: string]: nothing -> string {
    ^git cat-file blob $"($rev):($manifest_path)" | complete | if $in.exit_code == 0 { $in.stdout } else { '' }
}

# Compare a merged tree against the base, given each one's flake checks; hashes the base's listed paths and the appended ones
export def compare [base: string, merged: string, base_checks: list<string>, merged_checks: list<string>]: nothing -> record {
    let base_manifest = manifest-at $base
    let section = appended $base_manifest (manifest-at $merged)
    let appended_paths = if $section == null { [] } else { appended-entries $section | where path != null | get path }
    let listed = [...($base_manifest | parse-manifest | get path) ...$appended_paths] | uniq
    let before = snapshot $base $base_checks []
    let after = snapshot $merged $merged_checks $listed
    {found: (violations $before $after), added: (additions $before $after)}
}

# The one-line verdict, also the commit status description
export def summary [found: table, approved: bool]: nothing -> string {
    if ($found | is-empty) { return 'no protected asset changed' }
    let rules = $found | get rule | uniq | str join ', '
    let count = $found | length
    if $approved { $"owner-approved: ($count) protected changes \(($rules))" } else { $"($count) protected changes \(($rules))" }
}

# Compare the merge of head into base against base; exit 1 on a protected change unless --approved, and always on a conflict
export def main [
    base: string # base revision, whose manifest and checks are protected
    head: string # head revision, read only as data
    --approved # the pull request carries the owner-approved label: report changes and pass
] {
    let merged = merge-tree $base $head
    if $merged == null {
        let found = [{rule: 'merge', detail: 'the head does not merge cleanly into the base; rebase it'}]
        for row in $found { print $"($row.rule): ($row.detail)" }
        print (summary $found false)
        exit 1
    }
    let result = compare $base $merged (flake-checks $base) (flake-checks $merged)
    let found = $result.found
    let added = $result.added
    for row in $found { print $"($row.rule): ($row.detail)" }
    for path in ($added.entries | first 20) { print $"added manifest entry: ($path)" }
    if ($added.entries | length) > 20 { print $"added manifest entries: ($added.entries | length) in total" }
    for name in $added.checks { print $"added flake check: ($name)" }
    print (summary $found $approved)
    if ($found | is-not-empty) and not $approved { exit 1 }
}
