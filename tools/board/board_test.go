package main

import (
	"bytes"
	"errors"
	"flag"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

var update = flag.Bool("update", false, "rewrite testdata/*.golden from the current output")

// fixtureGH answers the board's two GraphQL queries from testdata.
func fixtureGH(t *testing.T) ghFunc {
	t.Helper()
	return func(args ...string) ([]byte, error) {
		q := args[len(args)-1]
		switch {
		case strings.Contains(q, "issues("):
			return os.ReadFile(filepath.Join("testdata", "issues.json"))
		case strings.Contains(q, "pullRequests("):
			return os.ReadFile(filepath.Join("testdata", "pulls.json"))
		}
		t.Fatalf("unexpected gh call %q", args)
		return nil, nil
	}
}

func TestBoardGolden(t *testing.T) {
	var got bytes.Buffer
	if err := run(&got, fixtureGH(t)); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join("testdata", "board.golden")
	if *update {
		if err := os.WriteFile(path, got.Bytes(), 0o644); err != nil {
			t.Fatal(err)
		}
		return
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(string(want), got.String()); diff != "" {
		t.Errorf("board mismatch (-want +got):\n%s\nrun `go test ./tools/board -update` to accept the new output", diff)
	}
}

func TestRunReportsFetchFailure(t *testing.T) {
	tests := []struct {
		name, failing, want string
	}{
		{"issues", "issues(", "fetch issues: HTTP 401"},
		{"pull requests after issues", "pullRequests(", "fetch pull requests: HTTP 401"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			fixture := fixtureGH(t)
			gh := func(args ...string) ([]byte, error) {
				if strings.Contains(args[len(args)-1], tt.failing) {
					return nil, errors.New("HTTP 401")
				}
				return fixture(args...)
			}
			var out bytes.Buffer
			err := run(&out, gh)
			if err == nil || !strings.Contains(err.Error(), tt.want) {
				t.Fatalf("run error = %v, want %q", err, tt.want)
			}
			if out.Len() > 0 {
				t.Errorf("run wrote %q, want nothing", out.String())
			}
		})
	}
}

func TestReady(t *testing.T) {
	base := issue{Labels: []string{"feature", "P2"}}
	tests := []struct {
		name string
		edit func(*issue)
		want bool
	}{
		{"decided, prioritized, unblocked, unassigned", func(*issue) {}, true},
		{"design", func(i *issue) { i.Labels = []string{"design", "P3"} }, true},
		{"bugfix", func(i *issue) { i.Labels = []string{"bugfix", "P0"} }, true},
		{"idea", func(i *issue) { i.Labels = []string{"idea", "P1"} }, false},
		{"research only", func(i *issue) { i.Labels = []string{"research", "P1"} }, false},
		{"no priority means unscheduled", func(i *issue) { i.Labels = []string{"feature"} }, false},
		{"assigned", func(i *issue) { i.Assignees = []string{"someone"} }, false},
		{"open blocker", func(i *issue) { i.OpenBlockers = []int{7} }, false},
		{"open sub-issue", func(i *issue) { i.SubIssues, i.SubClosed = 3, 2 }, false},
		{"every sub-issue closed", func(i *issue) { i.SubIssues, i.SubClosed = 3, 3 }, true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			i := base
			i.Labels = append([]string(nil), base.Labels...)
			tt.edit(&i)
			if got := i.ready(); got != tt.want {
				t.Errorf("ready() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestPriorityTakesTheHighest(t *testing.T) {
	if got := (issue{Labels: []string{"P3", "P1", "feature"}}).priority(); got != "P1" {
		t.Errorf("priority() = %q, want P1", got)
	}
}

func TestTouches(t *testing.T) {
	tests := []struct {
		name, body string
		want       []string
	}{
		{"backticks", "Touches: `internal/tuner`, `docs/spec/tuner.md`", []string{"internal/tuner", "docs/spec/tuner.md"}},
		{"bare paths", "Touches: internal/watch,justfile", []string{"internal/watch", "justfile"}},
		{"trailing slash", "Touches: `cmd/togi/`", []string{"cmd/togi"}},
		{"dot components", "Touches: ./internal/tuner, `internal//watch/./frame.go`", []string{"internal/tuner", "internal/watch/frame.go"}},
		{"root", "Touches: `.`, `./`", []string{".", "."}},
		{"inside a body with CRLF", "## Why\r\n\r\nTouches: `a`, `b`\r\n\r\n## Links", []string{"a", "b"}},
		{"empty line", "Touches:\n<!-- comment -->", nil},
		{"not at line start", "It Touches: a", nil},
		{"none", "no line", nil},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if diff := cmp.Diff(tt.want, touches(tt.body)); diff != "" {
				t.Errorf("touches mismatch (-want +got):\n%s", diff)
			}
		})
	}
}

func TestOverlaps(t *testing.T) {
	tests := []struct {
		a, b string
		want bool
	}{
		{"internal/tuner", "internal/tuner", true},
		{"internal/tuner", "internal/tuner/hunt.go", true},
		{"internal/tuner/hunt.go", "internal/tuner", true},
		{"internal/tuner", "internal/tunerx", false},
		{"internal/tuner/hunt.go", "internal/tuner/evidence.go", false},
		{"docs", "docs/spec/tuner.md", true},
		{".", "internal/tuner/hunt.go", true},
		{"justfile", ".", true},
	}
	for _, tt := range tests {
		if got := overlaps(tt.a, tt.b); got != tt.want {
			t.Errorf("overlaps(%q, %q) = %v, want %v", tt.a, tt.b, got, tt.want)
		}
	}
}

func TestPullsFor(t *testing.T) {
	linked := branch{Repo: "shgew/togi", Name: "linked"}
	pulls := []pull{
		{Number: 5, Body: "Resolves #12, Refs #4"},
		{Number: 1, Body: "Closes #12"},
		{Number: 2, Body: "Refs #120"},
		{Number: 3, Body: "Mentions #12 in passing"},
		{Number: 4, Head: linked},
		{Number: 6, Head: branch{Repo: "someone/togi", Name: "linked"}},
		{Number: 7, Head: branch{Name: "linked"}},
	}
	var got []int
	for _, p := range pullsFor(issue{Number: 12, Branches: []branch{linked}}, pulls) {
		got = append(got, p.Number)
	}
	if diff := cmp.Diff([]int{1, 4, 5}, got); diff != "" {
		t.Errorf("pullsFor mismatch (-want +got):\n%s", diff)
	}
}

func TestWorkAddsPullHeads(t *testing.T) {
	linked := branch{Repo: "shgew/togi", Name: "linked"}
	pulls := []pull{
		{Number: 1, Head: linked},
		{Number: 2, Head: branch{Repo: "shgew/togi", Name: "opened"}},
		{Number: 3, Head: branch{Repo: "someone/togi", Name: "linked"}},
		{Number: 4, Head: branch{Name: "gone"}},
	}
	got := work("shgew/togi", []branch{linked}, pulls)
	if diff := cmp.Diff([]string{"linked", "opened", "someone/togi:linked"}, got); diff != "" {
		t.Errorf("work mismatch (-want +got):\n%s", diff)
	}
}

func TestGroupOrdersByPriorityThenBlock(t *testing.T) {
	a, b := &parent{Number: 2, Title: "Block: a"}, &parent{Number: 9, Title: "Block: b"}
	ready := []readyIssue{
		{Number: 1, Labels: []string{"P2"}, Parent: b},
		{Number: 2, Labels: []string{"P1"}},
		{Number: 3, Labels: []string{"P1"}, Parent: b},
		{Number: 4, Labels: []string{"P1"}, Parent: a},
		{Number: 5, Labels: []string{"P1"}, Parent: b},
	}
	var got [][]int
	for _, g := range group(ready) {
		var numbers []int
		for _, i := range g.Issues {
			numbers = append(numbers, i.Number)
		}
		got = append(got, numbers)
	}
	if diff := cmp.Diff([][]int{{4}, {3, 5}, {2}, {1}}, got); diff != "" {
		t.Errorf("groups mismatch (-want +got):\n%s", diff)
	}
}
