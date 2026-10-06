package main

import (
	"encoding/json"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
)

var opened = time.Date(2026, 10, 1, 9, 0, 0, 0, time.UTC)

func recordBody(t *testing.T, findings ...finding) string {
	t.Helper()
	b, err := json.Marshal(record{Version: 3, Findings: findings})
	if err != nil {
		t.Fatal(err)
	}
	return "## Review record\n\n<!-- togi-review " + string(b) + " -->\n"
}

func TestSummarize(t *testing.T) {
	t.Parallel()
	deferred := finding{Source: "A", Priority: "P1", Finding: "leak", Outcome: outcome{Status: "deferred", Issue: 7}}
	nowFixed := deferred
	nowFixed.Outcome = outcome{Status: "fixed", SHA: "abc"}
	pulls := []pull{
		{Number: 12, Title: "blocked", Opened: opened, Check: "failure"},
		{Number: 10, Title: "chain", Opened: opened, Check: "success", Comments: []comment{
			{URL: "second", Created: opened.Add(3 * time.Hour), Body: recordBody(t, nowFixed,
				finding{Source: "B", Priority: "P2", Finding: "race", Outcome: outcome{Status: "rejected", Reason: "guarded"}})},
			{URL: "ping", Created: opened.Add(time.Minute), Body: "@coderabbitai full review"},
			{URL: "first", Created: opened.Add(90 * time.Minute), Body: recordBody(t, deferred,
				finding{Source: "B", Priority: "P3", Finding: "typo", Outcome: outcome{Status: "fixed", SHA: "abc"}})},
		}},
		{Number: 11, Title: "no check", Opened: opened, Comments: []comment{
			{URL: "v2", Created: opened.Add(20 * time.Minute), Body: readFixture(t, "v2.md")},
		}},
		{Number: 9, Title: "before review", Opened: opened},
	}
	rows, err := summarize(pulls)
	if err != nil {
		t.Fatal(err)
	}
	want := []row{
		{Number: 9, Title: "before review"},
		{Number: 10, Title: "chain", Check: "success", Records: 2, FirstRecord: 90 * time.Minute,
			Findings: counts{Priority: [4]int{0, 1, 1, 1}, Outcome: [3]int{2, 1, 0}}},
		{Number: 11, Title: "no check", Records: 1, FirstRecord: 20 * time.Minute,
			Findings: counts{Priority: [4]int{0, 0, 1, 3}, Outcome: [3]int{3, 0, 1}}},
		{Number: 12, Title: "blocked", Check: "failure"},
	}
	if diff := cmp.Diff(want, rows); diff != "" {
		t.Errorf("rows (-want +got):\n%s", diff)
	}
	wantTotals := totals{Pulls: 4, Checked: 2, Successful: 1, Recorded: 2,
		Findings:     counts{Priority: [4]int{0, 1, 2, 4}, Outcome: [3]int{5, 1, 1}},
		FirstRecords: []time.Duration{20 * time.Minute, 90 * time.Minute}}
	if diff := cmp.Diff(wantTotals, total(rows)); diff != "" {
		t.Errorf("totals (-want +got):\n%s", diff)
	}
}

func TestSummarizeMalformedRecord(t *testing.T) {
	t.Parallel()
	_, err := summarize([]pull{{Number: 1, Comments: []comment{{URL: "bad", Body: "<!-- togi-review {\"version\":9} -->"}}}})
	if err == nil || !strings.Contains(err.Error(), "parse record bad: unsupported record version 9") {
		t.Fatalf("summarize error = %v", err)
	}
}

func TestRun(t *testing.T) {
	t.Parallel()
	type author struct {
		Login string `json:"login"`
	}
	type commentNode struct {
		Author    *author `json:"author"`
		CreatedAt string  `json:"createdAt"`
		URL       string  `json:"url"`
		Body      string  `json:"body"`
	}
	pullJSON := func(number int, title string, conclusions []string, comments ...commentNode) map[string]any {
		var runs []map[string]any
		for _, c := range conclusions {
			runs = append(runs, map[string]any{"conclusion": c})
		}
		return map[string]any{
			"number": number, "title": title, "createdAt": "2026-10-01T09:00:00Z",
			"commits": map[string]any{"nodes": []any{map[string]any{"commit": map[string]any{
				"checkSuites": map[string]any{"nodes": []any{map[string]any{"checkRuns": map[string]any{"nodes": runs}}}},
			}}}},
			"comments": map[string]any{"pageInfo": map[string]any{"hasNextPage": false}, "nodes": comments},
		}
	}
	page := func(pulls ...map[string]any) map[string]any {
		return map[string]any{"data": map[string]any{"repository": map[string]any{"pullRequests": map[string]any{"nodes": pulls}}}}
	}
	bot, owner := &author{robotogiLogin}, &author{"shgew"}
	pages := []any{
		page(
			pullJSON(358, "Rename failure points", []string{"FAILURE", "SUCCESS"},
				commentNode{owner, "2026-10-01T09:05:00Z", "u0", readFixture(t, "v1.md")},
				commentNode{bot, "2026-10-01T11:30:00Z", "u1", readFixture(t, "v3.md")}),
			pullJSON(287, "Fit the target machine", nil,
				commentNode{nil, "2026-10-01T09:01:00Z", "u2", "ghost"},
				commentNode{bot, "2026-10-01T09:45:00Z", "u3", readFixture(t, "v2.md")}),
		),
		page(
			pullJSON(273, "Simulate memory", []string{""}, commentNode{bot, "2026-10-01T10:00:00Z", "u4", readFixture(t, "v1.md")}),
			pullJSON(200, "Before the App", nil),
		),
	}
	out, err := json.Marshal(pages)
	if err != nil {
		t.Fatal(err)
	}
	var gotArgs []string
	gh := func(args ...string) ([]byte, error) {
		gotArgs = args
		return out, nil
	}
	var w strings.Builder
	if err := run(&w, gh); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]string{"api", "graphql", "--paginate", "--slurp", "-F", "owner={owner}", "-F", "name={repo}", "-f", "query=" + pullsQuery}, gotArgs); diff != "" {
		t.Errorf("gh args (-want +got):\n%s", diff)
	}
	want := `PR     CHECK    RECORDS  P0  P1  P2  P3  FIXED  REJECTED  DEFERRED  FIRST RECORD  TITLE
#200   none     0        0   0   0   0   0      0         0         -             Before the App
#273   pending  1        0   0   4   0   4      0         0         1h00m         Simulate memory
#287   none     1        0   0   1   3   3      0         1         45m           Fit the target machine
#358   success  1        0   0   6   1   6      1         0         2h30m         Rename failure points
total  2/4      3/4      0   0   11  4   13     1         1

4 merged pull requests: 2 with robotogi's review check on the head (1 successful), 3 with a review record.
Findings: P0 0, P1 0, P2 11, P3 4; fixed 13, rejected 1, deferred 1.
Time from opening to the first record: median 1h00m, longest 2h30m.
`
	if diff := cmp.Diff(want, w.String()); diff != "" {
		t.Errorf("output (-want +got):\n%s", diff)
	}
}

func TestRunTruncatedComments(t *testing.T) {
	t.Parallel()
	gh := func(...string) ([]byte, error) {
		return []byte(`[{"data":{"repository":{"pullRequests":{"nodes":[{"number":5,"comments":{"pageInfo":{"hasNextPage":true},"nodes":[]}}]}}}}]`), nil
	}
	err := run(&strings.Builder{}, gh)
	if err == nil || !strings.Contains(err.Error(), "pull request #5 has more than 100 comments") {
		t.Fatalf("run error = %v", err)
	}
}

func TestFormatDuration(t *testing.T) {
	t.Parallel()
	for d, want := range map[time.Duration]string{
		29 * time.Second:               "0m",
		90 * time.Second:               "2m",
		59 * time.Minute:               "59m",
		26*time.Hour + 4*time.Minute:   "26h04m",
		time.Hour + 30*time.Second - 1: "1h00m",
	} {
		if got := formatDuration(d); got != want {
			t.Errorf("formatDuration(%s) = %q, want %q", d, got, want)
		}
	}
}
