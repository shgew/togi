package main

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/tools/trialfacts"
)

var deletedScenarios = []string{"target-r4-limit", "target-delayed-joint", "target-flat-risk", "target-flat-cost"}

func TestDeletedScenariosAreGone(t *testing.T) {
	suite, err := decodeSuite("suite.json")
	if err != nil {
		t.Fatal(err)
	}
	for _, s := range suite.Scenarios {
		if slices.Contains(deletedScenarios, s.Name) {
			t.Errorf("suite still lists deleted scenario %s", s.Name)
		}
	}
	for _, name := range deletedScenarios {
		path := filepath.Join("machines", name+".json")
		if _, err := os.Stat(path); !os.IsNotExist(err) {
			t.Errorf("%s still exists (stat error %v)", path, err)
		}
	}
	baseline, err := readResults("baseline.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	nonmember := 0
	for _, r := range baseline {
		if slices.Contains(deletedScenarios, r.Scenario) {
			t.Errorf("baseline still records deleted scenario %s seed %d", r.Scenario, r.Seed)
		}
		if r.Scenario == "target-nonmember-mce" {
			nonmember++
		}
	}
	if diff := cmp.Diff(220, len(baseline)); diff != "" {
		t.Errorf("baseline record count (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(8, nonmember); diff != "" {
		t.Errorf("target-nonmember-mce baseline records (-want +got):\n%s", diff)
	}
}

func TestDefaultSuiteRunCounts(t *testing.T) {
	runs, err := loadRuns("suite.json", "all", trialfacts.Extracts{})
	if err != nil {
		t.Fatal(err)
	}
	dev := 0
	for _, r := range runs {
		if r.split == "dev" {
			dev++
		}
	}
	if diff := cmp.Diff([2]int{220, 110}, [2]int{len(runs), dev}); diff != "" {
		t.Fatalf("all-seed and dev run counts (-want +got):\n%s", diff)
	}
}

func TestNonmemberIsSyntheticConclusionGuardOnly(t *testing.T) {
	const name = "target-nonmember-mce"
	g, err := loadGate(archivedGate("suite.json"))
	if err != nil || g == nil {
		t.Fatalf("loadGate = %v, %v", g, err)
	}
	if !slices.Contains(g.Conclude, name) {
		t.Errorf("%s is not a conclusion scenario", name)
	}
	if slices.Contains(g.Gated, name) || slices.Contains(g.Pooled, name) {
		t.Errorf("%s must not be a gated or pooled adversary group", name)
	}
	for _, deleted := range deletedScenarios {
		for group, names := range map[string][]string{"gated": g.Gated, "pooled": g.Pooled, "conclude": g.Conclude} {
			if slices.Contains(names, deleted) {
				t.Errorf("%s group still names %s", group, deleted)
			}
		}
	}
	labelled := false
	for _, note := range g.Notes {
		if strings.Contains(note, name) && strings.Contains(note, "synthetic conclusion guard") && strings.Contains(note, "no hazard or time claim") {
			labelled = true
		}
	}
	if !labelled {
		t.Errorf("gate notes do not label %s as a synthetic conclusion guard: %q", name, g.Notes)
	}

	data, err := os.ReadFile(filepath.Join("machines", name+".json"))
	if err != nil {
		t.Fatal(err)
	}
	var machine struct {
		Description string `json:"description"`
		Cores       int    `json:"cores"`
		Model       struct {
			PastLimitRate float64 `json:"past_limit_rate"`
			Growth        float64 `json:"growth"`
		} `json:"model"`
		Core  []json.RawMessage `json:"core"`
		Joint []json.RawMessage `json:"joint"`
	}
	if err := json.Unmarshal(data, &machine); err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"Synthetic", "#530", "not an admitted adversary"} {
		if !strings.Contains(machine.Description, want) {
			t.Errorf("description lacks %q: %s", want, machine.Description)
		}
	}
	if strings.Contains(machine.Description, "all parameters except") || strings.Contains(machine.Description, "unchanged") {
		t.Errorf("description still claims derived parameters are unchanged: %s", machine.Description)
	}
	if diff := cmp.Diff([]float64{0.0029067995729377782, 2.5754932971816853}, []float64{machine.Model.PastLimitRate, machine.Model.Growth}); diff != "" {
		t.Errorf("synthetic guard's numeric model changed (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(16, machine.Cores); diff != "" || len(machine.Core) != 16 || len(machine.Joint) == 0 {
		t.Errorf("synthetic guard's structure changed: cores %d, core tables %d, joints %d\n%s", machine.Cores, len(machine.Core), len(machine.Joint), diff)
	}

	suite, err := decodeSuite("suite.json")
	if err != nil {
		t.Fatal(err)
	}
	for _, s := range suite.Scenarios {
		if s.Name != name {
			continue
		}
		want := scenario{Name: name, Machine: "machines/" + name + ".json", Replay: true, Dev: []uint64{1, 2, 3, 4}, Holdout: []uint64{101, 102, 103, 104}, Smoke: []uint64{103}}
		if diff := cmp.Diff(want, s); diff != "" {
			t.Errorf("scenario changed (-want +got):\n%s", diff)
		}
	}
}

func TestReportSuiteNotes(t *testing.T) {
	var out bytes.Buffer
	reportSuiteNotes(&out, nil)
	reportSuiteNotes(&out, &gate{})
	if out.Len() != 0 {
		t.Fatalf("nil and note-less gates printed %q", out.String())
	}
	reportSuiteNotes(&out, &gate{Notes: []string{"first", "second"}})
	if diff := cmp.Diff("suite: first\nsuite: second\n", out.String()); diff != "" {
		t.Fatalf("notes output (-want +got):\n%s", diff)
	}

	g, err := loadGate(archivedGate("suite.json"))
	if err != nil || g == nil {
		t.Fatalf("loadGate = %v, %v", g, err)
	}
	out.Reset()
	reportSuiteNotes(&out, g)
	if !strings.Contains(out.String(), "suite: target-nonmember-mce stays as a labelled synthetic conclusion guard") {
		t.Fatalf("archived gate's notes do not label the synthetic guard:\n%s", out.String())
	}
}
