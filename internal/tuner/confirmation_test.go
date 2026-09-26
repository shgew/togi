package tuner

import (
	"testing"

	"code.marleb.org/shgew/shycler/internal/journal"
)

func TestConfirmation(t *testing.T) {
	t.Parallel()
	t.Run("confirming core counts passed slots", func(t *testing.T) {
		h := newHarness(t, coreStart{phase: journal.PhaseConfirmation, offset: -20})
		for range 3 {
			h.trial(h.s.Next(), passed)
		}
		if got, total := h.s.Confirmation(0); got != 3 || total != 9 {
			t.Errorf("Confirmation(0) = (%d, %d), want (3, 9)", got, total)
		}
	})
	t.Run("searching core has none", func(t *testing.T) {
		h := newHarness(t, searchAt(-10)...)
		h.trial(h.s.Next(), passed)
		if got, total := h.s.Confirmation(0); got != 0 || total != 9 {
			t.Errorf("Confirmation(0) = (%d, %d), want (0, 9)", got, total)
		}
	})
}
