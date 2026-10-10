package main

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/tuner"
	"github.com/shgew/togi/tools/trialfacts"
)

const archivedGateDir = "gates/ruleset-11"

func archivedGate(name string) string { return filepath.Join(archivedGateDir, name) }

func TestArchivedRuleset11Gate(t *testing.T) {
	for name, want := range map[string]string{
		"suite.json":      "afcd7feacbd6a5ddbaf30f1d69b2750749d92f30d52bde95816168ef2f2c895b",
		"baseline.jsonl":  "4cb9921ec9f31bba9899e3c4cd6dc0ebe4cbead1039fd5947f3f39ec8fb1f78c",
		"candidate.jsonl": "6aaa3781211f46b207af2ef8b6810c7cd398b9641ea227ddfedb1bdf7352fc05",
		"report.txt":      "68a1af81c8de041ca863ac004dbac4de83c2d854ada2588558eed9040516e7c6",
	} {
		data, err := os.ReadFile(archivedGate(name))
		if err != nil {
			t.Fatal(err)
		}
		sum := sha256.Sum256(data)
		if got := hex.EncodeToString(sum[:]); got != want {
			t.Errorf("archived %s has sha256 %s, want %s: the archive records what was scored and never changes", name, got, want)
		}
	}

	g, err := loadGate(archivedGate("suite.json"))
	if err != nil || g == nil {
		t.Fatalf("loadGate = %v, %v", g, err)
	}
	if g.ID != "ruleset-11" || g.Split != "all" || g.Baseline.Ruleset != 10 || !slices.Equal(g.Baseline.Commits, []string{"3a9b894", "21052983"}) || g.MaxTimeRatio != 2 || g.Resamples != 10000 || g.BootstrapSeed != [2]uint64{1, 1} || g.Confidence != 0.95 {
		t.Fatalf("archived gate = %+v", g)
	}
	if !slices.Equal(g.Gated, []string{"target-shared-voltage", "target-r7-vf-boost", "target-r7-request-gap", "shared-voltage"}) || !slices.Equal(g.Pooled, []string{"target-shared-voltage", "target-r7-vf-boost", "target-r7-request-gap"}) || !slices.Equal(g.Conclude, []string{"default", "idle-limit", "late-onset", "target-nonmember-mce"}) {
		t.Fatalf("archived gate groups: gated %v, pooled %v, conclude %v", g.Gated, g.Pooled, g.Conclude)
	}

	baseline, err := readResults(archivedGate("baseline.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	for _, r := range baseline {
		if r.Ruleset != 10 || !slices.Contains(g.Baseline.Commits, r.Commit) {
			t.Fatalf("archived baseline run %s/%d is ruleset %d at %s, want ruleset 10 at %v", r.Scenario, r.Seed, r.Ruleset, r.Commit, g.Baseline.Commits)
		}
	}
	candidate, err := readResults(archivedGate("candidate.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	if len(candidate) != 220 {
		t.Fatalf("archived candidate has %d runs, want 220", len(candidate))
	}
	for _, r := range candidate {
		if r.Ruleset != 11 || r.Dirty || r.Commit != "c7e8794a" || r.Status != "concluded" {
			t.Fatalf("archived candidate run %s/%d: ruleset %d, dirty %v, commit %s, status %s", r.Scenario, r.Seed, r.Ruleset, r.Dirty, r.Commit, r.Status)
		}
	}

	verdict := judgeGate(g, candidate, baseline)
	if verdict.Refused != "" {
		t.Fatalf("archived gate refused: %s", verdict.Refused)
	}
	var got bytes.Buffer
	reportGate(&got, g, verdict)
	report, err := os.ReadFile(archivedGate("report.txt"))
	if err != nil {
		t.Fatal(err)
	}
	var want []string
	for line := range strings.Lines(string(report)) {
		if strings.HasPrefix(line, "gate ") {
			want = append(want, line)
		}
	}
	if diff := cmp.Diff(strings.Join(want, ""), got.String()); diff != "" {
		t.Errorf("archived candidate and baseline do not reproduce report.txt's gate lines (-report +rejudged):\n%s", diff)
	}
	if verdict.pass() || !strings.Contains(got.String(), "gate ruleset-11: FAIL 5 of 21 criteria") {
		t.Errorf("archived verdict: want FAIL 5 of 21 criteria, got:\n%s", got.String())
	}
}

func TestCommittedSuiteRegistersNoGate(t *testing.T) {
	g, err := loadGate("suite.json")
	if err != nil || g != nil {
		t.Fatalf("loadGate(suite.json) = %+v, %v; the Ruleset 11 gate is archived and the next gate is not registered yet", g, err)
	}
}

func TestCommittedBaselineIsRuleset11(t *testing.T) {
	baseline, err := readResults("baseline.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	runs, err := loadRuns("suite.json", "all", trialfacts.Extracts{})
	if err != nil {
		t.Fatal(err)
	}
	have := make(map[key]bool, len(baseline))
	for _, r := range baseline {
		if r.Ruleset != tuner.Ruleset || r.Dirty || r.Commit != "5145f657" {
			t.Fatalf("baseline run %s/%d is ruleset %d at %s (dirty %v), want ruleset %d at the clean commit 5145f657 that recorded it: re-record with just bench-baseline at a clean commit and update this pin", r.Scenario, r.Seed, r.Ruleset, r.Commit, r.Dirty, tuner.Ruleset)
		}
		have[key{r.Scenario, r.Seed}] = true
	}
	if len(baseline) != len(runs) || len(have) != len(runs) {
		t.Fatalf("baseline has %d runs over %d distinct seeds, the suite has %d", len(baseline), len(have), len(runs))
	}
	for _, run := range runs {
		if !have[key{run.scenario.Name, run.seed}] {
			t.Errorf("baseline has no run for %s seed %d", run.scenario.Name, run.seed)
		}
	}
}
