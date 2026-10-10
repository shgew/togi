package main

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func included(path string, added, removed int) manifestFile {
	return manifestFile{Path: path, Status: "modified", Added: added, Removed: removed, HunkFile: "files/001.diff"}
}

func firstManifest(files ...manifestFile) manifest {
	return manifest{
		Version: manifestVersion, Repository: "shgew/togi", PullRequest: 700, HeadSHA: headSHA, BaseSHA: baseSHA,
		Files: files,
	}
}

func withCoverage(m manifest, ranges ...coverRange) manifest {
	m.Coverage = &coverage{Reported: len(ranges), Ranges: ranges, NotBuilt: []notBuilt{}}
	return m
}

func renderCases() map[string]struct {
	m  manifest
	in findingsInput
} {
	type c = struct {
		m  manifest
		in findingsInput
	}
	model := new("provider/model-1")
	goFiles := withCoverage(firstManifest(included("internal/tuner/step.go", 30, 4), included("internal/tuner/step_test.go", 50, 0)),
		coverRange{"internal/tuner/step.go", 12, 14}, coverRange{"internal/tuner/step.go", 40, 40})
	later := withCoverage(firstManifest(included("internal/tuner/step.go", 3, 1)), coverRange{"internal/tuner/step.go", 12, 12})
	later.Previous = &previousRecord{URL: previousURL, HeadSHA: oldHeadSHA, BaseSHA: oldBaseSHA}
	fallback := firstManifest(included("docs/howto.md", 5, 2))
	fallback.Previous = &previousRecord{URL: previousURL, HeadSHA: oldHeadSHA}
	fallback.Fallback = "the previous record is version 1 and does not name the base the pull request had at its head, so no minimal delta can be established"
	excluded := firstManifest(included("docs/howto.md", 5, 2),
		manifestFile{Path: "go.sum", Status: "modified", Added: 2, Exclusion: "generated"},
		manifestFile{Path: "pkg/testdata/in.txt", Status: "added", Added: 1, Exclusion: "test data (`**/testdata/**`)"})
	onlyExcluded := firstManifest(manifestFile{Path: "go.sum", Status: "modified", Added: 2, Exclusion: "generated"})
	docReviewers := []recordReviewer{{Name: "reviewer-1", Model: model, Files: []string{"docs/howto.md"}}}
	return map[string]c{
		"success": {goFiles, findingsInput{
			Reviewers: []recordReviewer{
				{Name: "reviewer-1", Model: model, Files: []string{"internal/tuner/step.go"}},
				{Name: "reviewer-2", Files: []string{"internal/tuner/step_test.go"}},
			},
			Findings: []recordFinding{
				{Source: "reviewer-1", Priority: "P2", Finding: "Readback skipped on retry | second line\nof text", Location: new("internal/tuner/step.go:42"),
					URL: new("https://github.com/shgew/togi/pull/700#discussion_r1"), Outcome: outcome{Status: "fixed", SHA: fixSHA}},
				{Source: "reviewer-2", Priority: "P3", Finding: "Stale comment <b>&</b>", Outcome: outcome{Status: "rejected", Reason: "wording only"}},
				{Source: "coordinator", Priority: "P2", Finding: "Deferred to a later issue", Outcome: outcome{Status: "deferred", Issue: 123}},
			},
			CoverageJudgment: "Both uncovered ranges are error paths the simulator cannot reach; neither became a finding.",
			Blocked:          "",
		}},
		"blocked": {firstManifest(included("docs/howto.md", 5, 2)), findingsInput{
			Reviewers: docReviewers,
			Findings: []recordFinding{
				{Source: "reviewer-1", Priority: "P1", Finding: "Wrong command", Location: new("docs/howto.md:7"), Outcome: outcome{Status: "deferred", Issue: 9}},
				{Source: "reviewer-1", Priority: "P0", Finding: "Unsafe", Outcome: outcome{Status: "deferred", Issue: 10}},
				{Source: "reviewer-1", Priority: "P2", Finding: "Minor", Outcome: outcome{Status: "deferred", Issue: 11}},
			},
			Blocked: "a review thread is unresolved.",
		}},
		"no-findings": {firstManifest(included("docs/howto.md", 5, 2)), findingsInput{Reviewers: docReviewers}},
		"exclusions":  {excluded, findingsInput{Reviewers: docReviewers}},
		"no-included": {onlyExcluded, findingsInput{}},
		"coverage": {withCoverage(firstManifest(included("a.go", 3, 0)), coverRange{"a.go", 5, 5}), findingsInput{
			Reviewers:        []recordReviewer{{Name: "coordinator", Files: []string{"a.go"}}},
			CoverageJudgment: "Judged a boundary a consumer could see; became a finding.",
			Findings:         []recordFinding{{Source: "coordinator", Priority: "P2", Finding: "Boundary untested", Location: new("a.go:5"), Outcome: outcome{Status: "fixed", SHA: fixSHA}}},
		}},
		"delta": {later, findingsInput{
			Reviewers:        []recordReviewer{{Name: "reviewer-1", Model: model, Files: []string{"internal/tuner/step.go"}}},
			Findings:         []recordFinding{{Source: "reviewer-1", Priority: "P2", Finding: "Earlier finding, now fixed", Outcome: outcome{Status: "fixed", SHA: fixSHA}}},
			CoverageJudgment: "The one range is a line the earlier record judged.",
		}},
		"fallback": {fallback, findingsInput{Reviewers: docReviewers}},
	}
}

func TestRenderGolden(t *testing.T) {
	t.Parallel()
	for name, tt := range renderCases() {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			if err := writeManifest(dir, tt.m); err != nil {
				t.Fatal(err)
			}
			findings, err := json.Marshal(tt.in)
			if err != nil {
				t.Fatal(err)
			}
			findingsFile := filepath.Join(dir, "findings.json")
			if err := os.WriteFile(findingsFile, findings, 0o644); err != nil {
				t.Fatal(err)
			}
			tl, _, _, _, _ := testTools(t, nil, nil)
			if err := runRender(tl, []string{dir, findingsFile}); err != nil {
				t.Fatal(err)
			}
			md, js := readFile(t, filepath.Join(dir, recordFile)), readFile(t, filepath.Join(dir, recordJSONFile))
			checkGolden(t, "render/"+name+".md", md)
			checkGolden(t, "render/"+name+".json", js)
			checkSameData(t, md, js)
		})
	}
}

// checkSameData holds the Markdown and the JSON to carrying one record: the hidden block is the JSON file, and what the JSON names is in the text.
func checkSameData(t *testing.T, md, js string) {
	t.Helper()
	if !strings.HasSuffix(md, "\n"+recordOpen+strings.TrimSuffix(js, "\n")+recordClose+"\n") {
		t.Error("the Markdown does not end with the JSON block")
	}
	if strings.Count(md, recordOpen) != 1 || strings.Contains(strings.TrimSuffix(js, "\n"), "\n") || strings.ContainsAny(js, "<>&") {
		t.Error("the block must be one line, once, with <, > and & escaped")
	}
	if _, err := parseRecord(md); err != nil {
		t.Errorf("the record does not parse as a historical one: %v", err)
	}
	var r recordModel
	dec := json.NewDecoder(strings.NewReader(js))
	dec.DisallowUnknownFields()
	if err := dec.Decode(&r); err != nil {
		t.Fatal(err)
	}
	if r.Version != 3 {
		t.Errorf("version = %d", r.Version)
	}
	for _, want := range append([]string{r.HeadSHA, r.BaseSHA, "**" + r.Verdict + "**"}, r.Files...) {
		if !strings.Contains(md, want) {
			t.Errorf("the text lacks %q that the JSON carries", want)
		}
	}
	for _, e := range r.ExcludedFiles {
		if !strings.Contains(md, e.Path) || !strings.Contains(md, e.Reason) {
			t.Errorf("the text lacks the exclusion of %s", e.Path)
		}
	}
	if rows := strings.Count(md, "\n| ") - 1; len(r.Findings) > 0 && rows != len(r.Findings) {
		t.Errorf("the table has %d rows for %d findings", rows, len(r.Findings))
	}
	if (len(r.Findings) == 0) != strings.Contains(md, "No findings.") {
		t.Error("the text says no findings when there are some, or the reverse")
	}
	if r.Previous != nil && !strings.Contains(md, r.Previous.URL+", range "+r.Previous.HeadSHA+".."+r.HeadSHA) {
		t.Error("the text lacks the previous record and range")
	}
}

func TestBuildRecordRejects(t *testing.T) {
	t.Parallel()
	goM := withCoverage(firstManifest(included("a.go", 1, 0)), coverRange{"a.go", 1, 1})
	docM := firstManifest(included("docs/x.md", 1, 0))
	rv := func(files ...string) []recordReviewer { return []recordReviewer{{Name: "r", Files: files}} }
	finding := func(o outcome) []recordFinding {
		return []recordFinding{{Source: "r", Priority: "P2", Finding: "x", Outcome: o}}
	}
	carry := docM
	carry.CarryForward = true
	tests := []struct {
		name string
		m    manifest
		in   findingsInput
		want string
	}{
		{"carry forward", carry, findingsInput{}, "carry the record forward"},
		{"no reviewers", docM, findingsInput{}, "reviewers are required"},
		{"uncovered file", docM, findingsInput{Reviewers: rv()}, "no reviewer covers docs/x.md"},
		{"foreign file", docM, findingsInput{Reviewers: rv("docs/x.md", "other.md")}, "does not include"},
		{"reviewers without files", firstManifest(manifestFile{Path: "go.sum", Exclusion: "generated"}), findingsInput{Reviewers: rv()}, "no file is included"},
		{"no coverage", firstManifest(included("a.go", 1, 0)), findingsInput{Reviewers: rv("a.go"), CoverageJudgment: "j"}, "has no coverage"},
		{"no judgment", goM, findingsInput{Reviewers: rv("a.go")}, "coverage_judgment is required"},
		{"judgment without Go", docM, findingsInput{Reviewers: rv("docs/x.md"), CoverageJudgment: "j"}, "changes no Go code"},
		{"bad priority", docM, findingsInput{Reviewers: rv("docs/x.md"), Findings: []recordFinding{{Source: "r", Priority: "P4", Finding: "x", Outcome: outcome{Status: "rejected", Reason: "r"}}}}, "unknown priority"},
		{"short sha", docM, findingsInput{Reviewers: rv("docs/x.md"), Findings: finding(outcome{Status: "fixed", SHA: "abc"})}, "outcome must be"},
		{"two outcome forms", docM, findingsInput{Reviewers: rv("docs/x.md"), Findings: finding(outcome{Status: "deferred", Issue: 1, Reason: "x"})}, "outcome must be"},
		{"unknown status", docM, findingsInput{Reviewers: rv("docs/x.md"), Findings: finding(outcome{Status: "open"})}, "outcome must be"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			_, err := buildRecord(tt.m, tt.in)
			if err == nil || !strings.Contains(err.Error(), tt.want) {
				t.Fatalf("error = %v, want one containing %q", err, tt.want)
			}
		})
	}
}

func TestRenderRejectsUnknownFindingsField(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	if err := writeManifest(dir, firstManifest()); err != nil {
		t.Fatal(err)
	}
	file := filepath.Join(dir, "findings.json")
	if err := os.WriteFile(file, []byte(`{"reviewer":[]}`), 0o644); err != nil {
		t.Fatal(err)
	}
	tl, _, _, _, _ := testTools(t, nil, nil)
	if err := runRender(tl, []string{dir, file}); err == nil || !strings.Contains(err.Error(), "unknown field") {
		t.Fatalf("error = %v, want an unknown-field refusal", err)
	}
	if err := runRender(tl, []string{dir}); err == nil || !strings.Contains(err.Error(), "usage: reviews render") {
		t.Fatalf("error = %v, want usage", err)
	}
}

func TestRenderVerdict(t *testing.T) {
	t.Parallel()
	cases := renderCases()
	for name, want := range map[string]string{"success": "success", "no-findings": "success", "blocked": "blocked"} {
		r, err := buildRecord(cases[name].m, cases[name].in)
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff(want, r.Verdict); diff != "" {
			t.Errorf("%s verdict (-want +got):\n%s", name, diff)
		}
	}
}
