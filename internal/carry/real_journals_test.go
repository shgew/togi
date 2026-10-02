package carry

import (
	"bytes"
	"compress/gzip"
	"io"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"

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
	got := prepare(t, dir, nil)
	want := &Carry{
		Sources: []journal.CarriedSource{
			src("20260927T221954Z", 3),
			src("20260926T151414Z", 2),
			{Session: "20260924T204352Z", Path: "archive/20260924T204352Z.jsonl", Schema: 1, Ruleset: 1},
		},
		Context: &machine.BIOSContext{
			BIOSVersion: "4.43", Board: "ASRock X870E Taichi", CPUModel: "AMD Ryzen 9 9950X3D2 16-Core Processor",
			Microcode: "0xb404038", BoostLimitMHz: 5650,
		},
		Cores: []journal.CarriedCore{
			{Core: 0, Edge: new(-45), EdgeSession: "20260924T204352Z", EdgeSeq: 3664, FailedMark: new(-32), MarkSession: "20260926T151414Z", MarkSeq: 7107, MarkSignal: machine.ComputationError},
			{Core: 1, Edge: new(-42), EdgeSession: "20260924T204352Z", EdgeSeq: 4622, FailedMark: new(-39), MarkSession: "20260924T204352Z", MarkSeq: 7962, MarkSignal: machine.UnexpectedExit},
			{Core: 2, Edge: new(-40), EdgeSession: "20260924T204352Z", EdgeSeq: 3345, FailedMark: new(-38), MarkSession: "20260926T151414Z", MarkSeq: 1657, MarkSignal: machine.Crash},
			{Core: 3, Edge: new(-38), EdgeSession: "20260924T204352Z", EdgeSeq: 4705, FailedMark: new(-35), MarkSession: "20260926T151414Z", MarkSeq: 3573, MarkSignal: machine.Crash},
			{Core: 4, Edge: new(-45), EdgeSession: "20260924T204352Z", EdgeSeq: 3896, FailedMark: new(-37), MarkSession: "20260924T204352Z", MarkSeq: 8259, MarkSignal: machine.Crash},
			{Core: 5, Edge: new(-45), EdgeSession: "20260924T204352Z", EdgeSeq: 3952, FailedMark: new(-37), MarkSession: "20260926T151414Z", MarkSeq: 5799, MarkSignal: machine.ComputationError},
			{Core: 6, Edge: new(-50), EdgeSession: "20260924T204352Z", EdgeSeq: 4447, FailedMark: new(-46), MarkSession: "20260926T151414Z", MarkSeq: 1889, MarkSignal: machine.Crash},
			{Core: 7, Edge: new(-50), EdgeSession: "20260924T204352Z", EdgeSeq: 4497, FailedMark: new(-43), MarkSession: "20260927T221954Z", MarkSeq: 454, MarkSignal: machine.Crash},
			{Core: 8, Edge: new(-50), EdgeSession: "20260926T151414Z", EdgeSeq: 201, FailedMark: new(-49), MarkSession: "20260926T151414Z", MarkSeq: 7586, MarkSignal: machine.ComputationError},
			{Core: 9, Edge: new(-50), EdgeSession: "20260926T151414Z", EdgeSeq: 259, FailedMark: new(-48), MarkSession: "20260927T221954Z", MarkSeq: 4341, MarkSignal: machine.ComputationError},
			{Core: 10, Edge: new(-50), EdgeSession: "20260926T151414Z", EdgeSeq: 317, FailedMark: new(-49), MarkSession: "20260927T221954Z", MarkSeq: 2948, MarkSignal: machine.ComputationError},
			{Core: 11, Edge: new(-50), EdgeSession: "20260924T204352Z", EdgeSeq: 4301, FailedMark: new(-45), MarkSession: "20260927T221954Z", MarkSeq: 4252, MarkSignal: machine.ComputationError},
			{Core: 12, Edge: new(-50), EdgeSession: "20260927T221954Z", EdgeSeq: 327},
			{Core: 13, Edge: new(-50), EdgeSession: "20260924T204352Z", EdgeSeq: 4421, FailedMark: new(-43), MarkSession: "20260927T221954Z", MarkSeq: 6753, MarkSignal: machine.ComputationError},
			{Core: 14, Edge: new(-50), EdgeSession: "20260927T221954Z", EdgeSeq: 439},
			{Core: 15, Edge: new(-50), EdgeSession: "20260927T221954Z", EdgeSeq: 510},
		},
	}
	if diff := cmp.Diff(want, got, cmpopts.IgnoreUnexported(Carry{})); diff != "" {
		t.Fatalf("real archive carry (-want +got):\n%s", diff)
	}
}
