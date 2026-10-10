package session

import (
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestSampleEvidenceStalledWorker(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name     string
		cores    []int
		regime   machine.Regime
		readings []map[int]int64
		core     *int
		at       *int64
	}{
		{"first stall", []int{2, 7}, machine.R7, []map[int]int64{{2: 1000, 7: 1000}, {2: 1000, 7: 2000}, {2: 1000, 7: 2000}}, new(2), new(int64(2000))},
		{"scheduled R6 suspension", []int{2, 7}, machine.R6, []map[int]int64{{2: 0, 7: 0}, {2: 0, 7: 100}, {2: 0, 7: 200}}, nil, nil},
		{"advancing until end", []int{2, 7}, machine.R7, []map[int]int64{{2: 1000, 7: 1000}, {2: 2000, 7: 2000}}, nil, nil},
		{"tie", []int{2, 7}, machine.R7, []map[int]int64{{2: 1000, 7: 1000}, {2: 1000, 7: 1000}}, nil, nil},
		{"missing reading", []int{2, 7}, machine.R7, []map[int]int64{{2: 1000, 7: 1000}, {2: 1000}, {2: 1000, 7: 2000}}, nil, nil},
		{"single loaded core", []int{2}, machine.R7, []map[int]int64{{2: 1000}, {2: 1000}}, nil, nil},
		{"no samples", []int{2, 7}, machine.R7, nil, nil, nil},
		{"one sample", []int{2, 7}, machine.R7, []map[int]int64{{2: 1000, 7: 1000}}, nil, nil},
		{"transient plateau resumes", []int{2, 7}, machine.R7, []map[int]int64{{2: 1000, 7: 1000}, {2: 1000, 7: 2000}, {2: 2000, 7: 3000}}, nil, nil},
		{"later worker stalls first", []int{2, 7}, machine.R7, []map[int]int64{{2: 1000, 7: 1000}, {2: 2000, 7: 1000}, {2: 2000, 7: 1000}}, new(7), new(int64(2000))},
		{"counter decrease", []int{2, 7}, machine.R7, []map[int]int64{{2: 1000, 7: 1000}, {2: 0, 7: 2000}, {2: 0, 7: 3000}}, nil, nil},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			samples := make([]machine.TrialConditions, len(tc.readings))
			for i, reading := range tc.readings {
				samples[i] = machine.TrialConditions{ElapsedMS: int64(i+1) * 1000, WorkerCPUMS: machine.PerCoreFrom(reading)}
			}
			summary := sampleEvidence(slices.Values(samples), tc.cores, tc.regime, nil)
			if len(samples) > 0 {
				if diff := cmp.Diff(&samples[len(samples)-1], summary.last); diff != "" {
					t.Fatal(diff)
				}
			} else if summary.last != nil {
				t.Fatalf("last sample without samples: %+v", summary.last)
			}
			if diff := cmp.Diff(tc.core, summary.stalledCore); diff != "" {
				t.Fatal(diff)
			}
			if diff := cmp.Diff(tc.at, summary.workerStalledMS); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestRequestedVoltage(t *testing.T) {
	t.Parallel()
	sample := func(a, b float32) machine.TrialConditions {
		table := &machine.PMTable{}
		table.VoltageRequestV[2], table.VoltageRequestV[7] = a, b
		table.VoltageRequestV[0], table.VoltageRequestV[15] = 1.75, 2
		return machine.TrialConditions{PMTable: table}
	}
	for _, tc := range []struct {
		name    string
		cores   []int
		samples []machine.TrialConditions
		median  *float64
		minimum *float64
	}{
		{"loaded subset odd", []int{2, 7}, []machine.TrialConditions{sample(1, 1.125), sample(1.5, 1.25), sample(1.25, 1)}, new(1.25), new(1.125)},
		{"loaded subset even", []int{2, 7}, []machine.TrialConditions{sample(1.5, 1.25), sample(1, 1.125), sample(1.25, 1), sample(1.375, 1.125)}, new(1.3125), new(1.125)},
		{"missing lanes skipped", []int{2, 7}, []machine.TrialConditions{{}, sample(1, 1.125), {}, sample(1.5, 1.25), {}}, new(1.3125), new(1.125)},
		{"single core", []int{7}, []machine.TrialConditions{sample(1.5, 1), sample(1.375, 1.25)}, new(1.125), new(1.0)},
		{"all missing", []int{2, 7}, []machine.TrialConditions{{}, {}}, nil, nil},
		{"no samples", []int{2, 7}, nil, nil, nil},
		{"no loaded cores", nil, []machine.TrialConditions{sample(1, 1.25)}, nil, nil},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			summary := sampleEvidence(slices.Values(tc.samples), tc.cores, machine.R7, nil)
			if diff := cmp.Diff(tc.median, summary.voltageMedianV); diff != "" {
				t.Fatalf("median (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff(tc.minimum, summary.voltageMinV); diff != "" {
				t.Fatalf("minimum (-want +got):\n%s", diff)
			}
		})
	}
}
