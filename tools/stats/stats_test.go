package main

import (
	"bytes"
	"compress/gzip"
	"flag"
	"io"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

var update = flag.Bool("update", false, "rewrite testdata/*.golden from the current output")

func TestSimulatedJointReport(t *testing.T) {
	f, err := os.Open(filepath.Join("testdata", "joint.jsonl.gz"))
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	r, err := gzip.NewReader(f)
	if err != nil {
		t.Fatal(err)
	}
	defer r.Close()
	data, err := io.ReadAll(r)
	if err != nil {
		t.Fatal(err)
	}
	journalPath := filepath.Join(t.TempDir(), "events.jsonl")
	if err := os.WriteFile(journalPath, data, 0o600); err != nil {
		t.Fatal(err)
	}
	events, err := readJournal(journalPath)
	if err != nil {
		t.Fatal(err)
	}
	var got bytes.Buffer
	if err := report(&got, events, time.Time{}); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join("testdata", "joint.golden")
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
		t.Errorf("report mismatch (-want +got):\n%s\nrun `go test ./tools/stats -update` to accept the new output", diff)
	}
}

func TestRunGrouping(t *testing.T) {
	for _, tc := range []struct {
		name  string
		boots []string
		kinds []journal.Payload
		want  []string
	}{
		{"crash reboot continuation", []string{"a", "b", "b", "c", "c"}, []journal.Payload{&journal.ConfigLoaded{}, &journal.ConfigLoaded{}, &journal.CrashDetected{}, &journal.ConfigLoaded{}, &journal.Shutdown{Reason: journal.ShutdownSignal}}, []string{"signal"}},
		{"shutdown splits boots", []string{"a", "a", "b", "b"}, []journal.Payload{&journal.ConfigLoaded{}, &journal.Shutdown{Reason: journal.ShutdownSignal}, &journal.ConfigLoaded{}, &journal.Shutdown{Reason: journal.ShutdownCommand}}, []string{"signal", "command"}},
		{"restart within boot", []string{"a", "a"}, []journal.Payload{&journal.ConfigLoaded{}, &journal.ConfigLoaded{}}, []string{"open", "open"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			events := make([]journal.Event, len(tc.kinds))
			for i, v := range tc.kinds {
				events[i] = journal.Event{Seq: i + 1, Boot: tc.boots[i], Time: time.Unix(int64(i), 0), Data: v}
			}
			p := project(events)
			got := make([]string, len(p.runs))
			for i, r := range p.runs {
				got[i] = r.ending
			}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestPriorPasses(t *testing.T) {
	target := journal.TrialIntent{Regime: machine.R7, Workload: "load", Cores: []int{1, 0}, DurationS: 120, Profile: []int{-10, -10}, Condition: machine.Masked}
	makeTrial := func(seq int, profile []int, result journal.Outcome, modify func(*journal.TrialIntent)) *trial {
		in := target
		in.Profile = profile
		if modify != nil {
			modify(&in)
		}
		return &trial{intent: &in, key: class(&in, nil), endSeq: seq, end: &journal.TrialEnd{Outcome: result}}
	}
	for _, tc := range []struct {
		name    string
		history []*trial
		want    int
	}{
		{"equal and deeper count", []*trial{makeTrial(1, []int{-10, -10}, journal.OutcomePass, nil), makeTrial(2, []int{-11, -10}, journal.OutcomePass, nil)}, 2},
		{"shallow and incomparable do not", []*trial{makeTrial(1, []int{-9, -10}, journal.OutcomePass, nil), makeTrial(2, []int{-11, -9}, journal.OutcomePass, nil)}, 0},
		{"later shallow failure invalidates", []*trial{makeTrial(1, []int{-11, -11}, journal.OutcomePass, nil), makeTrial(2, []int{-9, -9}, journal.OutcomeFailure, nil), makeTrial(3, []int{-10, -10}, journal.OutcomePass, nil)}, 1},
		{"deeper failure does not invalidate", []*trial{makeTrial(1, []int{-10, -10}, journal.OutcomePass, nil), makeTrial(2, []int{-11, -11}, journal.OutcomeFailure, nil)}, 1},
		{"same sorted cores and different condition", []*trial{makeTrial(1, []int{-10, -10}, journal.OutcomePass, func(in *journal.TrialIntent) { in.Cores = []int{0, 1}; in.Condition = machine.Resident })}, 1},
		{"class fields matter", []*trial{makeTrial(1, []int{-10, -10}, journal.OutcomePass, func(in *journal.TrialIntent) { in.DurationS = 300 }), makeTrial(2, []int{-10, -10}, journal.OutcomePass, func(in *journal.TrialIntent) { in.Workload = "other" }), makeTrial(3, []int{-10, -10}, journal.OutcomePass, func(in *journal.TrialIntent) { in.Regime = machine.R6 }), makeTrial(4, []int{-10, -10}, journal.OutcomePass, func(in *journal.TrialIntent) { in.Cores = []int{0} })}, 0},
		{"hunt boundary excluded", []*trial{makeTrial(10, []int{-10, -10}, journal.OutcomePass, nil), makeTrial(11, []int{-10, -10}, journal.OutcomePass, nil)}, 0},
		{"inconclusive does not invalidate", []*trial{makeTrial(1, []int{-10, -10}, journal.OutcomePass, nil), makeTrial(2, []int{-9, -9}, journal.OutcomeInconclusive, nil)}, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if diff := cmp.Diff(tc.want, priorPasses(tc.history, &target, 10, nil, nil)); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}
