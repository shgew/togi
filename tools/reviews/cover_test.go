package main

import (
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

// addedSection is one patch of a delta's hunk file: it adds lines first..last of path.
func addedSection(path string, first, last int) string {
	var b strings.Builder
	b.WriteString("diff --git a/" + path + " b/" + path + "\n--- a/" + path + "\n+++ b/" + path + "\n")
	b.WriteString("@@ -" + strconv.Itoa(first-1) + ",0 +" + strconv.Itoa(first) + "," + strconv.Itoa(last-first+1) + " @@\n")
	for range last - first + 1 {
		b.WriteString("+line\n")
	}
	return b.String()
}

// coverSnapshot writes a snapshot directory whose one included file a.go has the given sections concatenated in its hunk file, as a delta's does.
func coverSnapshot(t *testing.T, sections ...string) string {
	t.Helper()
	dir := t.TempDir()
	if err := os.MkdirAll(filepath.Join(dir, "files"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "files/001.diff"), []byte(strings.Join(sections, "")), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := writeManifest(dir, firstManifest(included("a.go", 0, 0))); err != nil {
		t.Fatal(err)
	}
	return dir
}

// TestCoverMergesOverlappingDeltaHunks counts one reported interval once when two delta patches of the same file overlap.
func TestCoverMergesOverlappingDeltaHunks(t *testing.T) {
	t.Parallel()
	dir := coverSnapshot(t, addedSection("a.go", 10, 20), addedSection("a.go", 15, 25))
	m, err := addCover(dir, "a.go:10-25\n")
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]coverRange{{"a.go", 10, 25}}, m.Coverage.Ranges); diff != "" {
		t.Errorf("ranges (-want +got):\n%s", diff)
	}
	if m.Coverage.Reported != 1 {
		t.Errorf("reported = %d, want 1", m.Coverage.Reported)
	}
	stored, err := readManifest(dir)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(m.Coverage, stored.Coverage); diff != "" {
		t.Errorf("stored coverage differs (-written +read):\n%s", diff)
	}

	var summary strings.Builder
	if err := printSummary(&summary, dir, stored); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(summary.String(), "uncovered: 1 of 1 reported ranges inside the hunks") || strings.Count(summary.String(), "  a.go:") != 1 {
		t.Errorf("summary = %q", summary.String())
	}

	r, err := buildRecord(stored, findingsInput{
		Reviewers:        []recordReviewer{{Name: "r", Files: []string{"a.go"}}},
		CoverageJudgment: "Judged.",
	})
	if err != nil {
		t.Fatal(err)
	}
	if r.Coverage == nil || r.Coverage.Uncovered != 1 {
		t.Fatalf("record coverage = %+v, want 1 uncovered", r.Coverage)
	}
	if md := r.markdown(); !strings.Contains(md, "- Coverage: 1 uncovered range. Judged.") {
		t.Errorf("markdown lacks a single uncovered range:\n%s", md)
	}
}

func TestIntersectCoverMergesPerPath(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name     string
		hunks    map[string][]span
		ranges   []coverRange
		want     []coverRange
		reported int
	}{
		{
			name:     "overlapping hunks from two patches",
			hunks:    map[string][]span{"a.go": {{10, 20}, {15, 25}}},
			ranges:   []coverRange{{"a.go", 10, 25}},
			want:     []coverRange{{"a.go", 10, 25}},
			reported: 1,
		},
		{
			name:     "exact duplicate hunks",
			hunks:    map[string][]span{"a.go": {{10, 20}, {10, 20}}},
			ranges:   []coverRange{{"a.go", 12, 18}},
			want:     []coverRange{{"a.go", 12, 18}},
			reported: 1,
		},
		{
			name:     "nested hunk",
			hunks:    map[string][]span{"a.go": {{10, 30}, {15, 20}}},
			ranges:   []coverRange{{"a.go", 5, 40}},
			want:     []coverRange{{"a.go", 10, 30}},
			reported: 1,
		},
		{
			name:     "disjoint hunks stay separate",
			hunks:    map[string][]span{"a.go": {{10, 20}, {30, 40}}},
			ranges:   []coverRange{{"a.go", 10, 40}},
			want:     []coverRange{{"a.go", 10, 20}, {"a.go", 30, 40}},
			reported: 1,
		},
		{
			name:     "adjacent intervals stay separate",
			hunks:    map[string][]span{"a.go": {{10, 20}, {21, 25}}},
			ranges:   []coverRange{{"a.go", 10, 25}},
			want:     []coverRange{{"a.go", 10, 20}, {"a.go", 21, 25}},
			reported: 1,
		},
		{
			name:     "touching on one line merges",
			hunks:    map[string][]span{"a.go": {{10, 20}, {20, 25}}},
			ranges:   []coverRange{{"a.go", 10, 25}},
			want:     []coverRange{{"a.go", 10, 25}},
			reported: 1,
		},
		{
			name:     "overlapping reported ranges",
			hunks:    map[string][]span{"a.go": {{10, 30}}},
			ranges:   []coverRange{{"a.go", 10, 20}, {"a.go", 15, 25}},
			want:     []coverRange{{"a.go", 10, 25}},
			reported: 2,
		},
		{
			name:     "other paths do not merge",
			hunks:    map[string][]span{"a.go": {{10, 20}}, "b.go": {{15, 25}}},
			ranges:   []coverRange{{"a.go", 10, 20}, {"b.go", 15, 25}},
			want:     []coverRange{{"a.go", 10, 20}, {"b.go", 15, 25}},
			reported: 2,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			got := intersectCover(tc.ranges, []notBuilt{{"a.go", "darwin/arm64"}, {"z.go", "darwin/arm64"}}, tc.hunks)
			if diff := cmp.Diff(tc.want, got.Ranges); diff != "" {
				t.Errorf("ranges (-want +got):\n%s", diff)
			}
			if got.Reported != tc.reported {
				t.Errorf("reported = %d, want %d", got.Reported, tc.reported)
			}
			if diff := cmp.Diff([]notBuilt{{"a.go", "darwin/arm64"}}, got.NotBuilt); diff != "" {
				t.Errorf("not built (-want +got):\n%s", diff)
			}
		})
	}
}
