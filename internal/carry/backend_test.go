package carry

import (
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

var (
	oldBackends  = journal.ConfigBackends{Mprime: "/nix/store/64hjzgj1msiyndpdxrk9l3gkjf3sczgj-mprime-31.04b02", Ycruncher: "/nix/store/n5g91xa9pzcfqyyh62xpwz3v53f87y0a-y-cruncher-0.8.7.9547"}
	newMprime    = journal.ConfigBackends{Mprime: "/nix/store/0000000000000000000000000000000a-mprime-31.04b03", Ycruncher: oldBackends.Ycruncher}
	newYcruncher = journal.ConfigBackends{Mprime: oldBackends.Mprime, Ycruncher: "/nix/store/0000000000000000000000000000000b-y-cruncher-0.8.7.9548"}
)

// workloadTrial records a decisive alone R1 trial of w on core 0 and returns its fact identity.
func workloadTrial(w *writer, workload machine.Workload, outcome journal.Outcome) factID {
	w.t.Helper()
	w.trials++
	id := fmt.Sprintf("%04d", w.trials)
	w.add(&journal.TrialIntent{Trial: id, Core: new(0), Offset: new(-30), Regime: machine.R1, Workload: workload.ID, Condition: machine.Alone, Phase: journal.PhaseSearch, DurationS: 90, Profile: []int{-30, 0}})
	signal := machine.Signal("")
	if outcome == journal.OutcomeFailure {
		signal = machine.ComputationError
	}
	end := w.add(&journal.TrialEnd{Trial: id, Outcome: outcome, Signal: signal, DurationS: 90})
	if outcome == journal.OutcomeFailure {
		w.add(&journal.Failure{Trial: id, Core: new(0), Offset: new(-30), Attribution: journal.Attributed, Condition: machine.Alone, Signal: signal})
	}
	return factID{w.session, end}
}

func TestCarriedPassesKeyedByBackendStorePath(t *testing.T) {
	mprime, ycruncher := machine.Workloads(machine.R1)[0], machine.Workloads(machine.R1)[1]
	if mprime.Backend != machine.Mprime || ycruncher.Backend != machine.Ycruncher {
		t.Fatalf("R1 workloads %s and %s no longer run mprime and y-cruncher", mprime.ID, ycruncher.ID)
	}
	for _, tc := range []struct {
		name                    string
		current                 journal.ConfigBackends
		mprimePass, ycruncherOK bool
	}{
		{name: "same backends", current: oldBackends, mprimePass: true, ycruncherOK: true},
		{name: "mprime updated between sessions", current: newMprime, ycruncherOK: true},
		{name: "y-cruncher updated between sessions", current: newYcruncher, mprimePass: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			w := newJournal(t, dir, "A", 3, &context, machine.CoreInfo{Core: 0}, machine.CoreInfo{Core: 1})
			w.add(&journal.ConfigLoaded{Config: journal.ConfigSnapshot{Backends: oldBackends}})
			mprimeFailure := workloadTrial(w, mprime, journal.OutcomeFailure)
			mprimePass := workloadTrial(w, mprime, journal.OutcomePass)
			ycruncherFailure := workloadTrial(w, ycruncher, journal.OutcomeFailure)
			ycruncherPass := workloadTrial(w, ycruncher, journal.OutcomePass)
			w.close()
			c := prepare(t, dir, []defect.Entry{})
			if err := c.ResolveFacts(&context, tc.current); err != nil {
				t.Fatal(err)
			}
			want := []factID{mprimeFailure}
			if tc.mprimePass {
				want = append(want, mprimePass)
			}
			want = append(want, ycruncherFailure)
			if tc.ycruncherOK {
				want = append(want, ycruncherPass)
			}
			if diff := cmp.Diff(want, factKeys(c.Facts), cmp.AllowUnexported(factID{})); diff != "" {
				t.Fatalf("carried facts (-want +got):\n%s", diff)
			}
		})
	}
}

func TestCopiedPassKeepsItsOriginalBackend(t *testing.T) {
	mprime := machine.Workloads(machine.R1)[0]
	for _, tc := range []struct {
		name         string
		dropOriginal bool
		wantCopy     bool
	}{
		{name: "original archive names the earlier mprime"},
		{name: "missing original archive trusts the copying session", dropOriginal: true, wantCopy: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			cores := []machine.CoreInfo{{Core: 0}, {Core: 1}}
			a := newJournal(t, dir, "A", 6, &context, cores...)
			a.add(&journal.ConfigLoaded{Config: journal.ConfigSnapshot{Backends: oldBackends}})
			aPass := workloadTrial(a, mprime, journal.OutcomePass)
			a.archive(dir)
			first, err := prepareFacts(dir, "A", nil, &context, 1)
			if err != nil {
				t.Fatal(err)
			}
			// A carry before ruleset 10 copied passes whatever backend the new session loaded.
			b := newJournal(t, dir, "B", 6, &context, cores...)
			b.add(&journal.ConfigLoaded{Config: journal.ConfigSnapshot{Backends: newMprime}})
			for _, f := range first {
				b.add(f.Payload())
			}
			b.add(&journal.SessionCarried{Sources: []journal.CarriedSource{src("A", 6)}, FailurePoints: true})
			bPass := workloadTrial(b, mprime, journal.OutcomePass)
			b.archive(dir)
			if tc.dropOriginal {
				if err := os.Remove(filepath.Join(dir, "archive", "A.jsonl")); err != nil {
					t.Fatal(err)
				}
			}
			if err := os.WriteFile(filepath.Join(dir, "archive", "B-carry-pending"), nil, 0o644); err != nil {
				t.Fatal(err)
			}
			j, err := journal.Lock(dir, opts())
			if err != nil {
				t.Fatal(err)
			}
			defer j.Close()
			c, err := Prepare(j, journal.Build{Schema: journal.Schema, Ruleset: 7}, []defect.Entry{}, &context)
			if err != nil {
				t.Fatal(err)
			}
			if err := c.ResolveFacts(&context, newMprime); err != nil {
				t.Fatal(err)
			}
			var want []factID
			if tc.wantCopy {
				want = append(want, aPass)
			}
			want = append(want, bPass)
			if diff := cmp.Diff(want, factKeys(c.Facts), cmp.AllowUnexported(factID{})); diff != "" {
				t.Fatalf("carried passes (-want +got):\n%s", diff)
			}
		})
	}
}
