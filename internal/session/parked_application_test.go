package session

import (
	"slices"
	"testing"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

// parkedBoundary watches a run for the application of a hunt group's parked profile and says where to crash it: at its
// first nonzero smu.intent, or at the smu.readback that follows. It matches by position (the first nonzero write after
// a hunt.group and before the next profile.applied or trial.intent), not by the event's cause, so it holds whatever the
// session cites. Restoration writes, which cite a session.baseline, are not applications and never match.
type parkedBoundary struct {
	group, hunt  int
	groups       int
	intent       int
	core, offset int
	baselines    []int
	crashes      []int
}

func (b *parkedBoundary) match(atReadback bool) func(journal.Payload, journal.Event) bool {
	return func(p journal.Payload, e journal.Event) bool {
		switch v := p.(type) {
		case *journal.SessionBaseline:
			b.baselines = append(b.baselines, e.Seq)
		case *journal.HuntGroup:
			b.group, b.hunt, b.intent = e.Seq, v.Hunt, 0
			b.groups++
		case *journal.ProfileApplied:
			if v.Condition != machine.Parked {
				b.group, b.intent = 0, 0
			}
		case *journal.TrialIntent:
			if v.Condition != machine.Parked {
				b.group, b.intent = 0, 0
			}
		case *journal.ProfileChange, *journal.HuntEnd:
			b.group, b.intent = 0, 0
		case *journal.SMUIntent:
			restoring := slices.ContainsFunc(e.Cause, func(c int) bool { return slices.Contains(b.baselines, c) })
			if b.group != 0 && b.intent == 0 && !restoring && v.Core != nil && v.Offset != 0 {
				b.intent, b.core, b.offset = e.Seq, *v.Core, v.Offset
				return !atReadback
			}
		case *journal.SMUReadback:
			return atReadback && b.intent != 0 && v.Core == b.core && v.Offset == b.offset
		}
		return false
	}
}

// A crash while a hunt group's parked profile is being applied, after the first nonzero write and before
// profile.applied and trial.intent, is a parked crash. Once the write is read back the idle failure is attributed to
// its core and the hunt ends direct naming it.
func TestParkedApplicationCrash(t *testing.T) {
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
		name       string
		atReadback bool
	}{
		{"first nonzero smu.intent", false},
		{"after its smu.readback", true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			m := newSim(t, small())
			in := simInput(t.TempDir(), m)
			b := &parkedBoundary{}
			// The first crash, after the together profile, queues the hunt; the next lands in its parked application.
			gate := crashAt(ref[applied].Seq, m)
			crash, phase := gate.do, 0
			gate.do = func(e journal.Event) error {
				phase++
				if phase == 1 {
					gate.fired, gate.seen, gate.match = false, 0, b.match(tc.atReadback)
				} else {
					b.crashes = append(b.crashes, e.Seq)
				}
				return crash(e)
			}
			if stop := drive(t, in, gate); stop.Reason != StopCycles {
				t.Fatalf("stopped with %+v", stop)
			}
			if len(b.crashes) != 1 {
				t.Fatalf("crashed in %d hunt group applications, want 1", len(b.crashes))
			}
			if b.groups != 1 {
				t.Fatalf("%d hunt.group events, want 1", b.groups)
			}
			checkParkedCrash(t, readEvents(t, in.Dir), b.crashes[0], b, tc.atReadback)
		})
	}
}

// A same-boot resume restores the offsets, which does not close the running hunt group: the tuner runs its parked trial
// again without a new hunt.group, so a crash in that re-application is a parked crash and the hunt ends direct.
func TestParkedApplicationCrashAfterSameBootRestore(t *testing.T) {
	t.Parallel()
	_, ref := reference(t, small())
	applied := slices.IndexFunc(ref, func(e journal.Event) bool {
		p, ok := e.Data.(*journal.ProfileApplied)
		return ok && p.Condition == machine.Together
	})
	if applied < 0 {
		t.Fatal("reference run never applied the profile")
	}
	m := newSim(t, small())
	in := simInput(t.TempDir(), m)
	b := &parkedBoundary{}
	boundary := b.match(true)
	phase := 0
	gate := &appendGate{after: true}
	gate.match = func(p journal.Payload, e journal.Event) bool {
		bounded := boundary(p, e)
		switch phase {
		case 0:
			return e.Seq == ref[applied].Seq
		case 1:
			_, group := p.(*journal.HuntGroup)
			return group
		default:
			return bounded
		}
	}
	gate.do = func(e journal.Event) error {
		phase++
		gate.fired, gate.seen = false, 0
		switch phase {
		case 1:
			m.Crash()
			return machine.ErrCrashed
		case 2:
			return errKilled
		}
		b.crashes = append(b.crashes, e.Seq)
		m.Crash()
		return machine.ErrCrashed
	}
	if stop := drive(t, in, gate); stop.Reason != StopCycles {
		t.Fatalf("stopped with %+v", stop)
	}
	events := readEvents(t, in.Dir)
	if len(b.crashes) != 1 {
		t.Fatalf("crashed in %d hunt group re-applications, want 1", len(b.crashes))
	}
	if b.groups != 1 {
		t.Fatalf("%d hunt.group events, want 1: the resume must not open a new group", b.groups)
	}
	if !slices.ContainsFunc(events, func(e journal.Event) bool { return e.Seq > b.group && e.Kind == journal.KindProfileRestored }) {
		t.Fatal("the resume did not restore the offsets while the group was open")
	}
	checkParkedCrash(t, events, b.crashes[0], b, true)
}

func checkParkedCrash(t *testing.T, events []journal.Event, crashSeq int, b *parkedBoundary, atReadback bool) {
	t.Helper()
	var boot string
	for _, e := range events {
		if e.Seq == crashSeq {
			boot = e.Boot
		}
	}
	for _, e := range events {
		if e.Boot == boot && e.Seq > b.group && e.Seq <= crashSeq && (e.Kind == journal.KindProfileApplied || e.Kind == journal.KindTrialIntent) {
			t.Fatalf("%s #%d recorded before the crash at #%d", e.Kind, e.Seq, crashSeq)
		}
	}
	detected, ok := crashDetectedFor(events, boot)
	if !ok {
		t.Fatal("no crash.detected")
	}
	if c := detected.Data.(*journal.CrashDetected).Condition; c != machine.Parked {
		t.Fatalf("crash.detected condition %q, want %q", c, machine.Parked)
	}
	failure := failureCiting(events, detected.Seq)
	if failure == nil || failure.Signal != machine.Crash || failure.Condition != machine.Parked {
		t.Fatalf("idle failure %+v, want a parked crash", failure)
	}

	failureSeq := 0
	for _, e := range events {
		if e.Kind == journal.KindFailure && slices.Contains(e.Cause, detected.Seq) {
			failureSeq = e.Seq
			break
		}
	}
	if !atReadback {
		if failure.Attribution != journal.Unattributed {
			t.Fatalf("a write with no readback attributed the failure: %+v", failure)
		}
		return
	}
	if failure.Attribution != journal.Attributed || failure.Core == nil || *failure.Core != b.core || failure.Offset == nil || *failure.Offset != b.offset {
		t.Fatalf("idle failure %+v, want attributed to core %02d at %d", failure, b.core, b.offset)
	}
	ended := slices.IndexFunc(events, func(e journal.Event) bool {
		p, ok := e.Data.(*journal.HuntEnd)
		return ok && p.Hunt == b.hunt && slices.Contains(e.Cause, failureSeq)
	})
	if ended < 0 {
		t.Fatalf("hunt %d did not end on failure #%d", b.hunt, failureSeq)
	}
	if end := events[ended].Data.(*journal.HuntEnd); end.Result != "direct" || !slices.Equal(end.Cores, []int{b.core}) {
		t.Fatalf("hunt end %+v, want direct naming core %02d", end, b.core)
	}
}

// A hunt.group arms the parked condition for its application's first nonzero write, whatever cause that write cites.
// It stays armed through the group's parked profile.applied, trial.intent, a crash and a restoration, because a resumed
// session may run the same group again without a new hunt.group; a together profile.applied or trial.intent, a profile
// change and the hunt's end disarm it, so later applications keep their own condition.
func TestHuntGroupArmsParkedUntilReplaced(t *testing.T) {
	t.Parallel()
	f := newFold()
	const boot = "b"
	seq := 0
	fold := func(data journal.Payload, cause ...int) int {
		seq++
		f.Fold(journal.Event{Seq: seq, Boot: boot, Kind: data.Kind(), Cause: cause, Data: data})
		return seq
	}
	core := 0
	check := func(step string, want machine.Condition) {
		t.Helper()
		if got := f.appliedCond[boot]; got != want {
			t.Fatalf("%s: condition %q, want %q", step, got, want)
		}
	}
	baseline := fold(&journal.SessionBaseline{Offsets: []int{0, 0}})
	profile := fold(&journal.ProfileChange{To: []int{-10, -10}})
	fold(&journal.ProfileApplied{Offsets: []int{-10, -10}, Condition: machine.Together}, profile)
	check("together profile.applied", machine.Together)

	fold(&journal.HuntGroup{Hunt: 1, Group: 1})
	fold(&journal.SMUIntent{Op: journal.SMUSet, Core: &core, Offset: 0}, profile)
	check("zero write before the first nonzero write", machine.Together)
	fold(&journal.SMUIntent{Op: journal.SMUSet, Core: &core, Offset: -10}, profile)
	check("first nonzero write of the group's application", machine.Parked)
	fold(&journal.CrashDetected{PreviousBoot: boot})
	fold(&journal.SMUIntent{Op: journal.SMUSet, Core: &core, Offset: -10}, profile)
	check("first nonzero write of the same group after a crash", machine.Parked)
	fold(&journal.ProfileApplied{Offsets: []int{-10, 0}, Condition: machine.Parked}, profile)
	check("parked profile.applied", machine.Parked)
	fold(&journal.SMUIntent{Op: journal.SMUSet, Core: &core, Offset: -10}, profile)
	check("write of the same group after its profile.applied", machine.Parked)
	fold(&journal.ProfileRestored{Offsets: []int{0, 0}}, baseline)
	check("profile.restored while the group is open", "")
	fold(&journal.SMUIntent{Op: journal.SMUSet, Core: &core, Offset: 0}, baseline)
	check("restoration write after profile.restored", "")
	fold(&journal.SMUIntent{Op: journal.SMUSet, Core: &core, Offset: -10}, profile)
	check("first nonzero write of the group's re-application after profile.restored", machine.Parked)

	fold(&journal.ProfileApplied{Offsets: []int{-12, -10}, Condition: machine.Together}, profile)
	check("together profile.applied after the group", machine.Together)
	fold(&journal.SMUIntent{Op: journal.SMUSet, Core: &core, Offset: -12}, profile)
	check("together write after the group", machine.Together)

	fold(&journal.HuntGroup{Hunt: 1, Group: 2})
	fold(&journal.SMUIntent{Op: journal.SMUSet, Core: &core, Offset: -8}, baseline)
	check("restoration write while a group is open", machine.Together)
	fold(&journal.HuntEnd{Hunt: 1, Result: "direct"})
	fold(&journal.SMUIntent{Op: journal.SMUSet, Core: &core, Offset: -9}, profile)
	check("write after the hunt ended", machine.Together)

	fold(&journal.HuntGroup{Hunt: 2, Group: 1})
	fold(&journal.ProfileChange{From: []int{-10, -10}, To: []int{-9, -9}})
	fold(&journal.SMUIntent{Op: journal.SMUSet, Core: &core, Offset: -9}, profile)
	check("write after a profile change closed the group", machine.Together)
}
