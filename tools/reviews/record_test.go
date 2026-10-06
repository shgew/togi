package main

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

type recordSummary struct {
	Version  int
	Findings []string
}

func summarizeRecord(r record) recordSummary {
	s := recordSummary{Version: r.Version}
	for _, f := range r.Findings {
		detail := f.Outcome.SHA
		switch {
		case f.Outcome.Issue != 0:
			detail = fmt.Sprintf("#%d", f.Outcome.Issue)
		case f.Outcome.Reason != "":
			detail = "with reason"
		}
		s.Findings = append(s.Findings, fmt.Sprintf("%s %s %s %s", f.Source, f.Priority, f.Outcome.Status, detail))
	}
	return s
}

func readFixture(t *testing.T, name string) string {
	t.Helper()
	b, err := os.ReadFile(filepath.Join("testdata", name))
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}

func TestParseRecordVersions(t *testing.T) {
	t.Parallel()
	const fix273 = "e0ed29e08c7f9277da3739f546e24af6c978ee9b"
	const fix287 = "9db8b22b904dba6b31bdae5e3f9f24038a2bd83f"
	const fix358 = "575bdc4843bcf8cd8896e2ac523632b11a8bbfbe"
	tests := []struct {
		fixture string
		want    recordSummary
	}{
		{"v1.md", recordSummary{Version: 1, Findings: []string{
			"SimRuntime273 P2 fixed " + fix273,
			"SimDocs273 P2 fixed " + fix273,
			"CodeRabbit P2 fixed " + fix273,
			"Coordinator273 P2 fixed " + fix273,
		}}},
		{"v2.md", recordSummary{Version: 2, Findings: []string{
			"R287S1 P3 fixed " + fix287,
			"R287S3 P2 deferred #263",
			"R287S4 P3 fixed " + fix287,
			"R287S4 P3 fixed " + fix287,
		}}},
		{"v3.md", recordSummary{Version: 3, Findings: []string{
			"ReviewPR358.Watch P2 fixed " + fix358,
			"ReviewPR358.Simrun P2 fixed " + fix358,
			"ReviewPR358.JournalLegacy P2 rejected with reason",
			"ReviewPR358.Docs P3 fixed " + fix358,
			"ReviewPR358.Docs P2 fixed " + fix358,
			"ReviewPR358.Docs P2 fixed " + fix358,
			"ReviewPR358.Docs P2 fixed " + fix358,
		}}},
		{"v3-delta.md", recordSummary{Version: 3}},
	}
	for _, tt := range tests {
		t.Run(tt.fixture, func(t *testing.T) {
			t.Parallel()
			r, err := parseRecord(readFixture(t, tt.fixture))
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tt.want, summarizeRecord(r)); diff != "" {
				t.Errorf("record (-want +got):\n%s", diff)
			}
		})
	}
}

func TestParseRecordRejects(t *testing.T) {
	t.Parallel()
	block := func(json string) string { return "## Review record\n\n<!-- togi-review " + json + " -->\n" }
	tests := []struct {
		name, body, want string
	}{
		{"no block", "@coderabbitai full review", errNoRecord.Error()},
		{"unterminated", "<!-- togi-review {\"version\":3}", "unterminated togi-review block"},
		{"bad json", block(`{"version":3,`), "decode togi-review block"},
		{"unknown version", block(`{"version":4,"findings":[]}`), "unsupported record version 4"},
		{"unknown priority", block(`{"version":3,"findings":[{"source":"R","priority":"P4","outcome":{"status":"fixed","sha":"a"}}]}`), `finding from R: unknown priority "P4"`},
		{"unknown outcome", block(`{"version":3,"findings":[{"source":"R","priority":"P1","outcome":{"status":"open"}}]}`), `finding from R: unknown outcome "open"`},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			_, err := parseRecord(tt.body)
			if err == nil || !strings.Contains(err.Error(), tt.want) {
				t.Fatalf("parseRecord error = %v, want containing %q", err, tt.want)
			}
			if tt.name == "no block" && !errors.Is(err, errNoRecord) {
				t.Errorf("parseRecord error = %v, want errNoRecord", err)
			}
		})
	}
}
