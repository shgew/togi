package main

import (
	"cmp"
	"math"
	"path"
	"regexp"
	"slices"
	"strconv"
	"strings"
)

// issue is an open issue as the board needs it.
type issue struct {
	Number       int
	Title        string
	Body         string
	Pinned       bool
	Labels       []string
	Assignees    []string
	Parent       *parent
	OpenBlockers []int
	Branches     []branch
	SubIssues    int
	SubClosed    int
}

type parent struct {
	Number int
	Title  string
}

// branch is a branch of a repository named owner/name; Repo is empty when the repository is gone.
type branch struct {
	Repo, Name string
}

// pull is an open pull request.
type pull struct {
	Number int
	Body   string
	Head   branch
}

var (
	touchesLine = regexp.MustCompile(`(?m)^Touches:[ \t]*(.+)$`)
	rulesetName = regexp.MustCompile(`^Ruleset (\d+)$`)
	// issueRef matches the keywords pull requests use to name their issues (AGENTS.md: Closes, Refs).
	issueRef = regexp.MustCompile(`(?i)\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?|refs?)\s*:?\s+#(\d+)\b`)
)

var priorities = []string{"P0", "P1", "P2", "P3"}

var readyKinds = []string{"design", "feature", "bugfix"}

func (i issue) has(label string) bool { return slices.Contains(i.Labels, label) }

// priority returns the highest P-label, or "" when the issue has none.
func (i issue) priority() string {
	for _, p := range priorities {
		if i.has(p) {
			return p
		}
	}
	return ""
}

// ready reports whether an agent may claim the issue: decided, prioritized, unblocked, unassigned, and not a container whose open sub-issues are the work.
func (i issue) ready() bool {
	return len(i.Assignees) == 0 &&
		slices.ContainsFunc(readyKinds, i.has) &&
		i.priority() != "" &&
		len(i.OpenBlockers) == 0 &&
		i.SubClosed == i.SubIssues
}

// touches parses the issue's `Touches:` line into clean paths, "." being the repository root.
func touches(body string) []string {
	m := touchesLine.FindStringSubmatch(strings.ReplaceAll(body, "\r\n", "\n"))
	if m == nil {
		return nil
	}
	var paths []string
	for item := range strings.SplitSeq(m[1], ",") {
		if p := strings.Trim(strings.TrimSpace(item), "`"); p != "" {
			paths = append(paths, path.Clean(p))
		}
	}
	return paths
}

// overlaps reports whether two clean paths are equal or one is a directory containing the other.
func overlaps(a, b string) bool {
	return a == b || a == "." || b == "." || strings.HasPrefix(b, a+"/") || strings.HasPrefix(a, b+"/")
}

// shared returns the paths in a that overlap any path in b.
func shared(a, b []string) []string {
	var out []string
	for _, p := range a {
		if slices.ContainsFunc(b, func(q string) bool { return overlaps(p, q) }) {
			out = append(out, p)
		}
	}
	return out
}

type progress struct {
	issue
	// Work holds the linked branches and the heads of the issue's pull requests, which GitHub unlinks once a pull request is opened from them.
	Work    []string
	Touches []string
	Pulls   []int
}

type readyGroup struct {
	Priority string
	Parent   *parent
	Issues   []readyIssue
}

type readyIssue struct {
	issue
	Touches []string
}

type overlap struct {
	Ready, Progress issue
	Paths           []string
}

type board struct {
	Waiting    []issue
	Untriaged  []issue
	InProgress []progress
	Ready      []readyGroup
	Overlaps   []overlap
	Rulesets   []issue
}

// build sorts the issues into the board; repo is the owner/name of their repository.
func build(repo string, issues []issue, pulls []pull) board {
	issues = slices.Clone(issues)
	slices.SortFunc(issues, func(a, b issue) int { return a.Number - b.Number })
	var b board
	var ready []readyIssue
	for _, i := range issues {
		if i.has("needs-decision") {
			b.Waiting = append(b.Waiting, i)
		}
		if i.has("needs-triage") {
			b.Untriaged = append(b.Untriaged, i)
		}
		if len(i.Assignees) > 0 {
			own := pullsFor(i, pulls)
			p := progress{issue: i, Work: work(repo, i.Branches, own), Touches: touches(i.Body)}
			for _, pr := range own {
				p.Pulls = append(p.Pulls, pr.Number)
			}
			b.InProgress = append(b.InProgress, p)
		}
		if i.ready() {
			ready = append(ready, readyIssue{issue: i, Touches: touches(i.Body)})
		}
		if rulesetName.MatchString(i.Title) {
			b.Rulesets = append(b.Rulesets, i)
		}
	}
	b.Ready = group(ready)
	for _, r := range ready {
		for _, p := range b.InProgress {
			if paths := shared(r.Touches, p.Touches); len(paths) > 0 {
				b.Overlaps = append(b.Overlaps, overlap{Ready: r.issue, Progress: p.issue, Paths: paths})
			}
		}
	}
	return b
}

// pullsFor returns, by number, the open pull requests that name the issue with a closing or Refs keyword, or whose head is one of its linked branches.
func pullsFor(i issue, pulls []pull) []pull {
	var out []pull
	for _, p := range pulls {
		named := slices.ContainsFunc(issueRef.FindAllStringSubmatch(p.Body, -1), func(m []string) bool {
			return m[1] == strconv.Itoa(i.Number)
		})
		if named || p.Head.Repo != "" && slices.Contains(i.Branches, p.Head) {
			out = append(out, p)
		}
	}
	slices.SortFunc(out, func(a, b pull) int { return a.Number - b.Number })
	return out
}

// work returns the names of the linked branches and of the pull requests' heads, once each, qualifying branches of other repositories as owner/name:branch and leaving out heads whose repository is gone.
func work(repo string, linked []branch, pulls []pull) []string {
	branches := slices.Clone(linked)
	for _, p := range pulls {
		if p.Head.Repo != "" && !slices.Contains(branches, p.Head) {
			branches = append(branches, p.Head)
		}
	}
	names := make([]string, len(branches))
	for k, b := range branches {
		names[k] = b.Name
		if b.Repo != repo {
			names[k] = b.Repo + ":" + b.Name
		}
	}
	return names
}

// group orders ready issues by priority, then by block parent, issues without a parent last.
func group(ready []readyIssue) []readyGroup {
	slices.SortStableFunc(ready, func(a, b readyIssue) int {
		return cmp.Or(
			cmp.Compare(a.priority(), b.priority()),
			cmp.Compare(parentOrder(a.Parent), parentOrder(b.Parent)),
		)
	})
	var groups []readyGroup
	for _, r := range ready {
		last := len(groups) - 1
		if last < 0 || groups[last].Priority != r.priority() || parentOrder(groups[last].Parent) != parentOrder(r.Parent) {
			groups = append(groups, readyGroup{Priority: r.priority(), Parent: r.Parent})
			last++
		}
		groups[last].Issues = append(groups[last].Issues, r)
	}
	return groups
}

func parentOrder(p *parent) int {
	if p == nil {
		return math.MaxInt
	}
	return p.Number
}
