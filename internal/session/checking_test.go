package session

import (
	"slices"
	"testing"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
)

func TestIdleCrashInChecking(t *testing.T) {
	t.Parallel()
	_, ref := reference(t, small())
	applied := slices.IndexFunc(ref, func(e journal.Event) bool {
		p, ok := e.Data.(*journal.ProfileApplied)
		return ok && p.Condition == machine.Together
	})
	if applied < 0 {
		t.Fatal("reference run never applied the profile")
	}
	firstWrite := -1
	for i := applied - 1; i >= 0; i-- {
		if p, ok := ref[i].Data.(*journal.SMUIntent); ok && p.Core != nil && p.Offset != 0 {
			firstWrite = i
			break
		}
	}
	if firstWrite < 0 {
		t.Fatal("no nonzero write before the profile was applied")
	}
	for _, tc := range []struct {
		name string
		at   journal.Event
	}{
		{"after profile applied", ref[applied]},
		{"partway through application", ref[firstWrite]},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			in := simInput(t.TempDir(), newSim(t, small()))
			stop := drive(t, in, crashAt(tc.at.Seq, in.Machine))
			if stop.Reason != StopCycles {
				t.Fatalf("stopped with %+v", stop)
			}
			events := readEvents(t, in.Dir)
			crash, ok := crashDetectedFor(events, tc.at.Boot)
			if !ok {
				t.Fatal("no crash.detected")
			}
			failure := failureCiting(events, crash.Seq)
			if failure == nil || failure.Signal != machine.Crash || failure.Regime != machine.R6 || failure.Condition != machine.Together {
				t.Fatalf("idle crash failure: %+v", failure)
			}
			failureSeq := 0
			for _, e := range events {
				if e.Kind == journal.KindFailure && slices.Contains(e.Cause, crash.Seq) {
					failureSeq = e.Seq
					break
				}
			}
			if !slices.ContainsFunc(events, func(e journal.Event) bool {
				p, ok := e.Data.(*journal.HuntStart)
				return ok && p.Regime == machine.R6 && p.Trial == "" && p.Failure == failureSeq
			}) {
				t.Fatalf("idle crash failure #%d did not queue an R6 hunt", failureSeq)
			}
		})
	}
}

func TestResumeContinuesBoots(t *testing.T) {
	t.Parallel()
	dir, first := reference(t, small())
	cfg, err := sim.Resume(dir, small())
	if err != nil {
		t.Fatal(err)
	}
	boots := map[string]bool{}
	for _, e := range first {
		boots[e.Boot] = true
	}
	last := first[len(first)-1].Time
	if cfg.Boots != len(boots) || !cfg.Start.After(last) {
		t.Fatalf("Resume: %d boots from %s, want %d after %s", cfg.Boots, cfg.Start, len(boots), last)
	}
	in := simInput(dir, newSim(t, cfg))
	in.Cycles = 2
	if stop := simulate(t, in); stop.Reason != StopCycles {
		t.Fatalf("second run stopped with %+v", stop)
	}
	events := readEvents(t, dir)
	if len(events) == len(first) {
		t.Fatal("second run appended nothing")
	}
	for _, e := range events[len(first):] {
		if boots[e.Boot] {
			t.Fatalf("seq %d reuses boot %s", e.Seq, e.Boot)
		}
		if e.Time.Before(last) {
			t.Fatalf("seq %d at %s, before %s", e.Seq, e.Time, last)
		}
		last = e.Time
	}
}

// A boot's kernel log starts with the MCEs its predecessor's crash left in the banks. Once the predecessor's
// crash.detected cites them, they are no evidence of the later boot's own crash.
func TestCrashEvidenceIsClaimedOnce(t *testing.T) {
	t.Parallel()
	f := newFold()
	for _, e := range []journal.Event{
		{Seq: 1, Boot: "a", Kind: journal.KindSessionStart, Data: &journal.SessionStart{}},
		{Seq: 2, Boot: "b", Kind: journal.KindMCE, Data: &journal.MCE{Core: 3, BankType: machine.LoadStore, FromBoot: "b", Lines: []string{"left by a"}}},
		{Seq: 3, Boot: "b", Kind: journal.KindCrashDetected, Data: &journal.CrashDetected{PreviousBoot: "a"}, Cause: []int{2}},
	} {
		f.Fold(e)
	}
	if got := f.recordedFor("a", "b"); !slices.Equal(got, []int{2}) {
		t.Fatalf("evidence of boot a: %v, want [2]", got)
	}
	if got := f.recordedFor("b", "c"); len(got) != 0 {
		t.Fatalf("evidence of boot b: %v, want none", got)
	}
}

// nextBootMCEKernel adds an uncorrected machine check to every boot but the crashed one.
type nextBootMCEKernel struct {
	machine.Kernel
	crashed string
}

func (k *nextBootMCEKernel) MCEs(boot string, since time.Duration) ([]machine.MCE, error) {
	mces, err := k.Kernel.MCEs(boot, since)
	if err != nil || boot == k.crashed {
		return mces, err
	}
	return append(mces, machine.MCE{CPU: 0, Core: 0, Bank: 0, BankType: machine.LoadStore, Lines: []string{"[Hardware Error]: Uncorrected error on CPU 0 (test)"}}), nil
}

func TestThermalTripIdleCrashClassification(t *testing.T) {
	t.Parallel()
	_, ref := reference(t, small())
	applied := slices.IndexFunc(ref, func(e journal.Event) bool {
		p, ok := e.Data.(*journal.ProfileApplied)
		return ok && p.Condition == machine.Together
	})
	if applied < 0 {
		t.Fatal("reference run never applied the profile")
	}
	for _, tc := range []struct {
		name     string
		evidence bool
	}{
		{"uncorrected MCE in the next boot is an idle failure", true},
		{"no evidence is a thermal dead end", false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			in := simInput(t.TempDir(), newSim(t, small()))
			if tc.evidence {
				seams := in.Machine.Seams()
				seams.Kernel = &nextBootMCEKernel{Kernel: seams.Kernel, crashed: ref[applied].Boot}
				in.Seams = &seams
			}
			gate := crashAt(ref[applied].Seq, in.Machine)
			gate.do = func(journal.Event) error {
				in.Machine.NextReset(machine.ResetThermalTrip)
				in.Machine.Crash()
				return machine.ErrCrashed
			}
			stop := drive(t, in, gate)
			events := readEvents(t, in.Dir)
			crash, ok := crashDetectedFor(events, ref[applied].Boot)
			if !ok {
				t.Fatal("no crash.detected")
			}
			p := crash.Data.(*journal.CrashDetected)
			if p.ResetReason != machine.ResetThermalTrip || p.Inconclusive == tc.evidence {
				t.Fatalf("crash %+v, want thermal_trip with inconclusive=%v", p, !tc.evidence)
			}
			failure := failureCiting(events, crash.Seq)
			if !tc.evidence {
				if failure != nil || stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != journal.DeadEndThermalTrip {
					t.Fatalf("failure %+v, stop %+v, want the thermal dead end", failure, stop)
				}
				return
			}
			if failure == nil || failure.Signal != machine.Crash || failure.Regime != machine.R6 || failure.Condition != machine.Together || failure.Attribution != journal.Unattributed {
				t.Fatalf("idle failure: %+v", failure)
			}
			if len(crash.Cause) != 1 {
				t.Fatalf("crash evidence %v, want the next boot's MCE", crash.Cause)
			}
			mce := events[crash.Cause[0]-1].Data.(*journal.MCE)
			if mce.Corrected || mce.FromBoot == p.PreviousBoot {
				t.Fatalf("crash evidence %+v, want an uncorrected MCE from the next boot", mce)
			}
			if failureCiting(events, crash.Cause[0]) != failure {
				t.Fatal("idle failure does not cite the crash's MCE")
			}
			for _, e := range events {
				if d, ok := e.Data.(*journal.DeadEnd); ok && d.Condition == journal.DeadEndThermalTrip {
					t.Fatalf("thermal dead end after a crash with evidence: %+v", d)
				}
			}
		})
	}
}
