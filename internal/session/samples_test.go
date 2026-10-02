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
				samples[i] = machine.TrialConditions{ElapsedMS: int64(i+1) * 1000, WorkerCPUMS: reading}
			}
			last, core, at := sampleEvidence(slices.Values(samples), tc.cores, tc.regime)
			if len(samples) > 0 {
				if diff := cmp.Diff(&samples[len(samples)-1], last); diff != "" {
					t.Fatal(diff)
				}
			} else if last != nil {
				t.Fatalf("last sample without samples: %+v", last)
			}
			if diff := cmp.Diff(tc.core, core); diff != "" {
				t.Fatal(diff)
			}
			if diff := cmp.Diff(tc.at, at); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}
