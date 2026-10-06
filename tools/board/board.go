package main

import (
	"cmp"
	"math"
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
	Branches     []string
	SubIssues    int
	SubClosed    int
}

type parent struct {
	Number int
	Title  string
}

// pull is an open pull request.
type pull struct {
	Number int
	Body   string
	Head   string
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

// touches parses the issue's `Touches:` line into paths.
func touches(body string) []string {
	m := touchesLine.FindStringSubmatch(strings.ReplaceAll(body, "\r\n", "\n"))
	if m == nil {
		return nil
	}
	var paths []string
	for item := range strings.SplitSeq(m[1], ",") {
		path := strings.TrimRight(strings.Trim(strings.TrimSpace(item), "`"), "/")
		if path != "" {
			paths = append(paths, path)
		}
	}
	return paths
}

// overlaps reports whether two paths are equal or one is a directory containing the other.
func overlaps(a, b string) bool {
	return a == b || strings.HasPrefix(b, a+"/") || strings.HasPrefix(a, b+"/")
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

func build(issues []issue, pulls []pull) board {
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
			b.InProgress = append(b.InProgress, progress{issue: i, Touches: touches(i.Body), Pulls: pullsFor(i, pulls)})
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

// pullsFor returns the open pull requests that name the issue with a closing or Refs keyword, or whose head is one of its linked branches.
func pullsFor(i issue, pulls []pull) []int {
	var out []int
	for _, p := range pulls {
		named := slices.ContainsFunc(issueRef.FindAllStringSubmatch(p.Body, -1), func(m []string) bool {
			return m[1] == strconv.Itoa(i.Number)
		})
		if named || slices.Contains(i.Branches, p.Head) {
			out = append(out, p.Number)
		}
	}
	slices.Sort(out)
	return out
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
