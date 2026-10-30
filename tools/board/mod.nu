const issues_query = 'query($owner: String!, $name: String!, $endCursor: String) {
  repository(owner: $owner, name: $name) {
    nameWithOwner
    issues(states: OPEN, first: 100, after: $endCursor) {
      pageInfo { hasNextPage endCursor }
      nodes {
        number title body isPinned
        labels(first: 50) { nodes { name } }
        assignees(first: 20) { nodes { login } }
        parent { number title }
        blockedBy(first: 50) { nodes { number state } }
        subIssuesSummary { total completed }
      }
    }
  }
}'

const pulls_query = 'query($owner: String!, $name: String!, $endCursor: String) {
  repository(owner: $owner, name: $name) {
    pullRequests(states: OPEN, first: 100, after: $endCursor) {
      pageInfo { hasNextPage endCursor }
      nodes { number body headRefName headRepository { nameWithOwner } }
    }
  }
}'

export def run-gh [args: list<string>]: nothing -> string {
    let result = (^gh ...$args | complete)
    if $result.exit_code != 0 {
        error make {msg: $'gh ($args.0): exit status ($result.exit_code): ($result.stderr | str trim)'}
    }
    $result.stdout
}

def fetch [gh: closure, kind: string, args: list<string>]: nothing -> string {
    try { do $gh $args } catch {|err|
        error make {msg: $'fetch ($kind): ($err.msg)'}
    }
}

def query-args [query: string]: nothing -> list<string> {
    [api graphql --paginate --slurp -F 'owner={owner}' -F 'name={repo}' -f $'query=($query)']
}

def wire-value [value: any, expected: string]: nothing -> any {
    let actual = ($value | describe | split row '<' | first)
    if $actual != $expected and not ($expected == list and $actual == table) {
        error make {msg: $'expected ($expected), got ($actual)'}
    }
    $value
}

def string-field [value: any]: nothing -> string { wire-value $value string }
def int-field [value: any]: nothing -> int { wire-value $value int }
def bool-field [value: any]: nothing -> bool { wire-value $value bool }
def record-field [value: any]: nothing -> record { wire-value $value record }
def list-field [value: any]: nothing -> list { wire-value $value list }

def parent [value: any]: nothing -> any {
    if $value == null { return null }
    let p = (record-field $value)
    {number: (int-field ($p.number? | default 0)), title: (string-field ($p.title? | default ''))}
}

def nodes [value: any]: nothing -> list {
    let connection = (record-field ($value | default {}))
    list-field ($connection.nodes? | default [])
}

def issue [value: record]: nothing -> record {
    let summary = (record-field ($value.subIssuesSummary? | default {}))
    {
        number: (int-field ($value.number? | default 0))
        title: (string-field ($value.title? | default ''))
        body: (string-field ($value.body? | default ''))
        pinned: (bool-field ($value.isPinned? | default false))
        labels: (nodes $value.labels? | each {|l| string-field ($l.name? | default '')})
        assignees: (nodes $value.assignees? | each {|a| string-field ($a.login? | default '')})
        parent: (parent $value.parent?)
        blockers: (nodes $value.blockedBy? | where {|b| $b.state? == OPEN} | each {|b| int-field ($b.number? | default 0)})
        total: (int-field ($summary.total? | default 0))
        completed: (int-field ($summary.completed? | default 0))
    }
}

export def board-output [gh: closure, now: datetime]: nothing -> string {
    let issue_json = (fetch $gh issues (query-args $issues_query))
    let data = (try {
        let pages = (list-field ($issue_json | from json))
        mut repo = ''
        mut issues = []
        for p in $pages {
            $repo = (string-field ($p.data?.repository?.nameWithOwner? | default ''))
            for n in (nodes $p.data?.repository?.issues?) {
                $issues = ($issues | append (issue $n))
            }
        }
        {repo: $repo, issues: $issues}
    } catch {|err| error make {msg: $'decode issues: ($err.msg)'} })
    let pull_json = (fetch $gh 'pull requests' (query-args $pulls_query))
    let pulls = (try {
        mut pulls = []
        for p in (list-field ($pull_json | from json)) {
            for n in (nodes $p.data?.repository?.pullRequests?) {
                let pull = {
                    number: (int-field ($n.number? | default 0))
                    body: (string-field ($n.body? | default ''))
                    repo: (string-field ($n.headRepository?.nameWithOwner? | default ''))
                    branch: (string-field ($n.headRefName? | default ''))
                }
                $pulls = ($pulls | append $pull)
            }
        }
        $pulls
    } catch {|err| error make {msg: $'decode pull requests: ($err.msg)'} })
    let limit_json = (fetch $gh 'interaction limit' [api 'repos/{owner}/{repo}/interaction-limits'])
    let limit = (try {
        let l = (record-field ($limit_json | from json))
        let expires = (if $l.expires_at? == null { null } else { $l.expires_at | into datetime })
        {
            limit: (string-field ($l.limit? | default ''))
            expires: $expires
        }
    } catch {|err| error make {msg: $'decode interaction limit: ($err.msg)'} })
    build $data.repo $data.issues $pulls $limit $now | render
}

export def priority [issue: record]: nothing -> string {
    [P0 P1 P2 P3] | where {|p| $p in $issue.labels} | get -o 0 | default ''
}

export def ready [issue: record]: nothing -> bool {
    ($issue.assignees | is-empty) and ($issue.labels | any {|l| $l in [design feature bugfix]}) and (priority $issue) != '' and ($issue.blockers | is-empty) and $issue.total == $issue.completed
}

def clean-path [path: string]: nothing -> string {
    let absolute = ($path | str starts-with '/')
    let parts = ($path | split row '/' | reduce --fold [] {|part, parts|
        if $part in ['' '.'] { $parts } else if $part == '..' {
            if ($parts | is-not-empty) and ($parts | last) != '..' { $parts | drop } else if $absolute { $parts } else { $parts | append '..' }
        } else { $parts | append $part }
    })
    let joined = ($parts | str join '/')
    if $absolute { '/' + $joined } else if $joined == '' { '.' } else { $joined }
}

export def touches [body: string]: nothing -> list<string> {
    let line = ($body | str replace --all "\r\n" "\n" | parse --regex '(?m)^Touches:[ \t]*(?<paths>.+)$' | get -o 0.paths)
    if $line == null { return [] }
    $line | split row ',' | each {|p| $p | str trim | str trim --char '`'} | where $it != '' | each {|p| clean-path $p}
}

export def overlaps [a: string, b: string]: nothing -> bool {
    $a == $b or $a == '.' or $b == '.' or ($b | str starts-with ($a + '/')) or ($a | str starts-with ($b + '/'))
}

export def pulls-for [number: int, pulls: list<record>]: nothing -> list<record> {
    $pulls | where {|p|
        $p.body | parse --regex '(?<![A-Za-z0-9_])(?i:close[sd]?|fix(?:e[sd])?|resolve[sd]?|refs?)[ \t]*:?[ \t]+#(?<number>[0-9]+)(?![A-Za-z0-9_])' | any {|m| $m.number == ($number | into string)}
    } | sort-by number
}

export def work [repo: string, pulls: list<record>]: nothing -> list<string> {
    $pulls | where repo != '' | select repo branch | uniq | each {|p|
        if $p.repo == $repo { $p.branch } else { $'($p.repo):($p.branch)' }
    }
}

export def group [issues: list<record>]: nothing -> list<record> {
    $issues | each {|i|
        $i | insert priority (priority $i) | insert parent_order ($i.parent?.number? | default 9223372036854775807)
    } | sort-by priority parent_order | reduce --fold [] {|i, groups|
        let last = (if ($groups | is-empty) { {} } else { $groups | last })
        if $last.priority? == $i.priority and $last.parent?.number? == $i.parent?.number? {
            $groups | update (($groups | length) - 1) {|g| $g | update issues ($g.issues | append $i)}
        } else {
            $groups | append {priority: $i.priority, parent: $i.parent, issues: [$i]}
        }
    }
}

export def interactions [limit: record, now: datetime]: nothing -> string {
    let renew = ': run `just lock-interactions`'
    if $limit.limit == '' or $limit.expires == null or $now >= $limit.expires { return ('open to everyone' + $renew) }
    let until = ($limit.expires | format date '%Y-%m-%d')
    if $limit.limit != collaborators_only {
        return $'limited to ($limit.limit) until ($until), not only collaborators($renew)'
    }
    let left = ($limit.expires - $now)
    if $left > 30day { return $'collaborators only until ($until)' }
    let days = (($left / 1day) | math ceil | into int)
    $'collaborators only until ($until), ($days) days left($renew)'
}

export def build [repo: string, issues: list<record>, pulls: list<record>, limit: record, now: datetime]: nothing -> record {
    let sorted = ($issues | sort-by number)
    let progress = ($sorted | where {|i| ($i.assignees | is-not-empty) and 'needs-hardware' not-in $i.labels} | each {|i|
        let own = (pulls-for $i.number $pulls)
        $i | insert work (work $repo $own) | insert touches (touches $i.body) | insert pulls ($own | get number)
    })
    let ready_issues = ($sorted | where {|i| (ready $i) and 'needs-hardware' not-in $i.labels} | each {|i| $i | insert touches (touches $i.body)})
    {
        interactions: (interactions $limit $now)
        waiting: ($sorted | where {|i| 'needs-decision' in $i.labels})
        hardware: ($sorted | where {|i| 'needs-hardware' in $i.labels})
        blocked: ($sorted | where {|i| ($i.blockers | is-not-empty) and ($i.assignees | is-empty) and 'needs-hardware' not-in $i.labels})
        untriaged: ($sorted | where {|i| 'needs-triage' in $i.labels})
        progress: $progress
        ready: (group $ready_issues)
        overlaps: ($ready_issues | each {|r|
            $progress | each {|p|
                let paths = ($r.touches | where {|a| $p.touches | any {|b| overlaps $a $b}})
                if ($paths | is-not-empty) { {ready: $r.number, progress: $p.number, paths: $paths} }
            }
        } | flatten)
        rulesets: ($sorted | where title =~ '^Ruleset ([0-9]+)$')
    }
}

def head [i: record]: nothing -> string { $'#($i.number) ($i.title)' }
def assigned [i: record]: nothing -> string {
    if ($i.assignees | is-empty) { '' } else { ' (@' + ($i.assignees | str join ', @') + ')' }
}
def display-list [items: list<string>]: nothing -> string {
    if ($items | is-empty) { 'none' } else { $items | str join ', ' }
}
def section [title: string, lines: list<string>]: nothing -> string {
    [$title (if ($lines | is-empty) { '  none' } else { $lines | str join "\n" })] | str join "\n"
}

export def render []: record -> string {
    let b = $in
    [
        (section Interactions [$'  ($b.interactions)'])
        (section 'Waiting on you (needs-decision)' ($b.waiting | each {|i| '  ' + (head $i) + (assigned $i)}))
        (section 'Waiting on the target machine (needs-hardware)' ($b.hardware | each {|i| '  ' + (head $i) + (assigned $i)}))
        (section Blocked ($b.blocked | each {|i| '  ' + (head $i) + ' — blocked by ' + ($i.blockers | each {|n| $'#($n)'} | str join ', ')}))
        (section 'Untriaged (needs-triage)' ($b.untriaged | each {|i| '  ' + (head $i)}))
        (section 'In progress' ($b.progress | each {|i|
            [
                ('  ' + (head $i) + (assigned $i))
                (if $i.parent != null { $'    block: #($i.parent.number) ($i.parent.title)' })
                $'    branches: (display-list $i.work)'
                $'    pull requests: (display-list ($i.pulls | each {|n| $'#($n)'}))'
                $'    touches: (display-list $i.touches)'
            ] | compact | str join "\n"
        }))
        (section Ready ($b.ready | enumerate | each {|g|
            [
                (if $g.index == 0 or ($b.ready | get ($g.index - 1) | get priority) != $g.item.priority { $'  ($g.item.priority)' })
                (if $g.item.parent == null { '    no block' } else { $'    #($g.item.parent.number) ($g.item.parent.title)' })
                ($g.item.issues | each {|i| ['      ' (head $i) "\n        touches: " (display-list $i.touches)] | str join} | str join "\n")
            ] | compact | str join "\n"
        }))
        (section Overlaps ($b.overlaps | each {|o| $'  #($o.ready) overlaps in-progress #($o.progress) on ($o.paths | str join ", ")'}))
        (section Ruleset ($b.rulesets | each {|i|
            let pinned = (if $i.pinned { 'pinned' } else { 'not pinned' })
            '  ' + (head $i) + $' ' + '(' + $'($pinned), ($i.completed) of ($i.total) sub-issues closed)'
        }))
    ] | str join "\n\n" | $in + "\n"
}
