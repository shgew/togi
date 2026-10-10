package main

import (
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/sim"
)

const (
	anchorMachine = "machines/target-shared-voltage.json"
	avx2Workload  = "mprime-avx2-36k-248k-allcore"
	avx512        = "mprime-avx512-36k-248k-allcore"
)

// TestAnchorOverlaysDifferFromAnchorOnlyByTheirMechanism decodes each anchor adversary and requires it to equal the
// decoded anchor with exactly the named required-voltage edits applied: no numeric tolerance, so the signal mix, the
// per-core tables and every scalar must be the anchor's own.
func TestAnchorOverlaysDifferFromAnchorOnlyByTheirMechanism(t *testing.T) {
	anchor, err := sim.LoadMachine(anchorMachine)
	if err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		name  string
		apply func(*sim.SharedVoltage)
	}{
		{"target-r7-vf-boost", func(v *sim.SharedVoltage) {
			v.Workload[avx2Workload].Core[5].ThresholdClockVPer100MHz = .06
		}},
		{"target-r7-request-gap", func(v *sim.SharedVoltage) {
			core := v.Workload[avx512].Core
			for c, threshold := range map[int]float64{9: 1.18, 10: 1.22, 11: 1.18} {
				core[c].ThresholdV = threshold
				core[c].ThresholdClockVPer100MHz = .06
			}
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			path := filepath.Join("machines", tc.name+".json")
			got, files, err := sim.LoadMachineWithFiles(path)
			if err != nil {
				t.Fatal(err)
			}
			want := anchor.Clone()
			tc.apply(want.SharedVoltage)
			if diff := cmp.Diff(want, got, cmp.AllowUnexported(sim.Replay{})); diff != "" {
				t.Fatalf("overlay is not today's anchor plus its mechanism (-want +got):\n%s", diff)
			}
			if cmp.Equal(anchor, got, cmp.AllowUnexported(sim.Replay{})) {
				t.Fatal("overlay equals the anchor: its mechanism changed nothing")
			}
			if diff := cmp.Diff([]string{path, anchorMachine}, files); diff != "" {
				t.Fatalf("machine files, overlay first (-want +got):\n%s", diff)
			}
			if _, err := sim.New(got); err != nil {
				t.Fatalf("overlay machine is not a valid simulator: %v", err)
			}
		})
	}
}
