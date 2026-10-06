package session

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
)

func TestCarryRecordsOnlyPassesOfTheLoadedBackend(t *testing.T) {
	const ran, updated = "/nix/store/64hjzgj1msiyndpdxrk9l3gkjf3sczgj-mprime-31.04b02", "/nix/store/0000000000000000000000000000000a-mprime-31.04b03"
	for _, tc := range []struct {
		name   string
		loaded string
		pass   bool
	}{
		{name: "same mprime", loaded: ran, pass: true},
		{name: "updated mprime", loaded: updated},
	} {
		t.Run(tc.name, func(t *testing.T) {
			m := newSim(t, small())
			c := carriedFixture(t, m)
			c.Facts[0].Backend = ran
			var want []journal.Payload
			for _, f := range c.Facts {
				if f.Outcome != journal.OutcomePass || tc.pass {
					want = append(want, f.Payload())
				}
			}
			in := simInput(t.TempDir(), m)
			in.Carry = c
			in.Config.Backends.Mprime = tc.loaded
			drive(t, in, nil)
			var got []journal.Payload
			for _, e := range readEvents(t, in.Dir) {
				switch e.Data.(type) {
				case *journal.TrialCarried, *journal.FailureCarried:
					got = append(got, e.Data)
				}
			}
			if diff := cmp.Diff(want, got); diff != "" {
				t.Fatalf("carried facts (-want +got):\n%s", diff)
			}
		})
	}
}
