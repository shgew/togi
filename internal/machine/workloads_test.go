package machine

import "testing"

func TestWorkloadRegimeContracts(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		regime         Regime
		count, threads int
		bases          []Regime
	}{
		{R1, 3, 1, []Regime{R1}}, {R2, 3, 1, []Regime{R2}}, {R3, 6, 1, []Regime{R1, R2}}, {R4, 9, 1, []Regime{R1}}, {R5, 6, 2, []Regime{R1, R2}}, {R6, 3, 1, []Regime{R1}}, {R7, 3, 1, []Regime{R2}},
	} {
		t.Run(string(tc.regime), func(t *testing.T) {
			ws := Workloads(tc.regime)
			if len(ws) != tc.count {
				t.Fatalf("workload breadth = %d, want %d", len(ws), tc.count)
			}
			seen := map[string]bool{}
			for _, w := range ws {
				if seen[w.ID] || w.Threads != tc.threads {
					t.Fatalf("duplicate workload or wrong thread count: %+v", w)
				}
				seen[w.ID] = true
				base, ok := WorkloadByID(w.Base)
				if !ok || base.Base != base.ID || base.Backend != w.Backend {
					t.Fatalf("derived workload lost base identity: %+v", w)
				}
				allowed := false
				for _, r := range tc.bases {
					for _, b := range Workloads(r) {
						allowed = allowed || b.ID == w.Base
					}
				}
				if !allowed {
					t.Fatalf("workload derives from wrong regime: %+v", w)
				}
				if tc.regime == R4 {
					if w.DutyPct != 25 && w.DutyPct != 50 && w.DutyPct != 75 {
						t.Fatalf("unsupported duty: %+v", w)
					}
				} else if w.DutyPct != 0 {
					t.Fatalf("unexpected duty cycle: %+v", w)
				}
			}
		})
	}
	if w, ok := WorkloadByID("not-a-workload"); ok || w != (Workload{}) {
		t.Fatalf("unknown workload resolved: %+v, %v", w, ok)
	}
}
