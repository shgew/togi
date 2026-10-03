package carry

import (
	"bytes"
	"compress/gzip"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"

	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestPrepareRealArchiveChain(t *testing.T) {
	dir := t.TempDir()
	archive := filepath.Join(dir, "archive")
	if err := os.MkdirAll(archive, 0o755); err != nil {
		t.Fatal(err)
	}
	for _, id := range []string{"20260924T204352Z", "20260926T151414Z", "20260927T221954Z"} {
		compressed, err := os.ReadFile(filepath.Join("testdata", id+".jsonl.gz"))
		if err != nil {
			t.Fatal(err)
		}
		r, err := gzip.NewReader(bytes.NewReader(compressed))
		if err != nil {
			t.Fatal(err)
		}
		data, err := io.ReadAll(r)
		closeErr := r.Close()
		if err != nil {
			t.Fatal(err)
		}
		if closeErr != nil {
			t.Fatal(closeErr)
		}
		if err := os.WriteFile(filepath.Join(archive, id+".jsonl"), data, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.WriteFile(filepath.Join(archive, "20260927T221954Z-carry-pending"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
	j, err := journal.Lock(dir, opts())
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	got, err := Prepare(j, journal.Build{Schema: journal.Schema, Ruleset: 8, EvidenceEpoch: 1}, nil, nil)
	if err != nil {
		t.Fatal(err)
	}
	want := &Carry{
		Sources: []journal.CarriedSource{
			{Session: "20260927T221954Z", Path: "archive/20260927T221954Z.jsonl", Schema: 2, Ruleset: 3},
			{Session: "20260926T151414Z", Path: "archive/20260926T151414Z.jsonl", Schema: 2, Ruleset: 2},
			{Session: "20260924T204352Z", Path: "archive/20260924T204352Z.jsonl", Schema: 1, Ruleset: 1},
		},
		Context: &machine.BIOSContext{
			BIOSVersion: "4.43", Board: "ASRock X870E Taichi", CPUModel: "AMD Ryzen 9 9950X3D2 16-Core Processor",
			Microcode: "0xb404038", BoostLimitMHz: 5650,
		},
		Cores: []journal.CarriedCore{
			{Core: 0, CandidateSoloLimit: new(-45), CandidateSoloLimitSession: "20260924T204352Z", CandidateSoloLimitSeq: 3664, FailurePoint: new(-32), FailurePointSession: "20260926T151414Z", FailurePointSeq: 7107, FailurePointSignal: machine.ComputationError},
			{Core: 1, CandidateSoloLimit: new(-42), CandidateSoloLimitSession: "20260924T204352Z", CandidateSoloLimitSeq: 4622, FailurePoint: new(-39), FailurePointSession: "20260924T204352Z", FailurePointSeq: 7962, FailurePointSignal: machine.UnexpectedExit},
			{Core: 2, CandidateSoloLimit: new(-40), CandidateSoloLimitSession: "20260924T204352Z", CandidateSoloLimitSeq: 3345, FailurePoint: new(-38), FailurePointSession: "20260926T151414Z", FailurePointSeq: 1657, FailurePointSignal: machine.Crash},
			{Core: 3, CandidateSoloLimit: new(-38), CandidateSoloLimitSession: "20260924T204352Z", CandidateSoloLimitSeq: 4705, FailurePoint: new(-35), FailurePointSession: "20260926T151414Z", FailurePointSeq: 3573, FailurePointSignal: machine.Crash},
			{Core: 4, CandidateSoloLimit: new(-45), CandidateSoloLimitSession: "20260924T204352Z", CandidateSoloLimitSeq: 3896, FailurePoint: new(-37), FailurePointSession: "20260924T204352Z", FailurePointSeq: 8259, FailurePointSignal: machine.Crash},
			{Core: 5, CandidateSoloLimit: new(-45), CandidateSoloLimitSession: "20260924T204352Z", CandidateSoloLimitSeq: 3952, FailurePoint: new(-37), FailurePointSession: "20260926T151414Z", FailurePointSeq: 5799, FailurePointSignal: machine.ComputationError},
			{Core: 6, CandidateSoloLimit: new(-50), CandidateSoloLimitSession: "20260924T204352Z", CandidateSoloLimitSeq: 4447, FailurePoint: new(-46), FailurePointSession: "20260926T151414Z", FailurePointSeq: 1889, FailurePointSignal: machine.Crash},
			{Core: 7, CandidateSoloLimit: new(-50), CandidateSoloLimitSession: "20260924T204352Z", CandidateSoloLimitSeq: 4497, FailurePoint: new(-43), FailurePointSession: "20260927T221954Z", FailurePointSeq: 454, FailurePointSignal: machine.Crash},
			{Core: 8, CandidateSoloLimit: new(-50), CandidateSoloLimitSession: "20260926T151414Z", CandidateSoloLimitSeq: 201, FailurePoint: new(-49), FailurePointSession: "20260926T151414Z", FailurePointSeq: 7586, FailurePointSignal: machine.ComputationError},
			{Core: 9, CandidateSoloLimit: new(-50), CandidateSoloLimitSession: "20260926T151414Z", CandidateSoloLimitSeq: 259, FailurePoint: new(-48), FailurePointSession: "20260927T221954Z", FailurePointSeq: 4341, FailurePointSignal: machine.ComputationError},
			{Core: 10, CandidateSoloLimit: new(-50), CandidateSoloLimitSession: "20260926T151414Z", CandidateSoloLimitSeq: 317, FailurePoint: new(-49), FailurePointSession: "20260927T221954Z", FailurePointSeq: 2948, FailurePointSignal: machine.ComputationError},
			{Core: 11, CandidateSoloLimit: new(-50), CandidateSoloLimitSession: "20260924T204352Z", CandidateSoloLimitSeq: 4301, FailurePoint: new(-45), FailurePointSession: "20260927T221954Z", FailurePointSeq: 4252, FailurePointSignal: machine.ComputationError},
			{Core: 12, CandidateSoloLimit: new(-50), CandidateSoloLimitSession: "20260927T221954Z", CandidateSoloLimitSeq: 327},
			{Core: 13, CandidateSoloLimit: new(-50), CandidateSoloLimitSession: "20260924T204352Z", CandidateSoloLimitSeq: 4421, FailurePoint: new(-43), FailurePointSession: "20260927T221954Z", FailurePointSeq: 6753, FailurePointSignal: machine.ComputationError},
			{Core: 14, CandidateSoloLimit: new(-50), CandidateSoloLimitSession: "20260927T221954Z", CandidateSoloLimitSeq: 439},
			{Core: 15, CandidateSoloLimit: new(-50), CandidateSoloLimitSession: "20260927T221954Z", CandidateSoloLimitSeq: 510},
		},
	}
	if diff := cmp.Diff(want, got, cmpopts.IgnoreUnexported(Carry{})); diff != "" {
		t.Fatalf("real archive carry (-want +got):\n%s", diff)
	}
	if err := got.ResolveFacts(got.Context); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(filepath.Join("testdata", "real-facts.json"))
	if err != nil {
		t.Fatal(err)
	}
	var wantFacts []facts.Fact
	if err := json.Unmarshal(data, &wantFacts); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(wantFacts, got.Facts); diff != "" {
		t.Fatalf("real archive facts (-want +got):\n%s", diff)
	}
}
