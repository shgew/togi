package machine

import (
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestClampOffsetSafety(t *testing.T) {
	t.Parallel()
	for o := -100; o <= 100; o++ {
		got := ClampOffset(o)
		if got < -50 || got > 0 {
			t.Fatalf("unsafe offset: ClampOffset(%d) = %d", o, got)
		}
		if o >= -50 && o <= 0 && got != o {
			t.Fatalf("legal offset changed: %d to %d", o, got)
		}
		if o < -50 && got != -50 || o > 0 && got != 0 {
			t.Fatalf("out-of-range offset not clamped to nearest bound: %d to %d", o, got)
		}
	}
}

func TestRegimeScope(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		regime         Regime
		valid, perCore bool
	}{
		{R1, true, false}, {R2, true, false}, {R3, true, false}, {R4, true, false}, {R5, true, false}, {R6, true, true}, {R7, true, true}, {"R8", false, false}, {"", false, false},
	} {
		t.Run(string(tc.regime), func(t *testing.T) {
			if diff := cmp.Diff([]bool{tc.valid, tc.perCore}, []bool{tc.regime.Valid(), tc.regime.InstancePerCore()}); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}
