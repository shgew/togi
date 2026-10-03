package session

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/tuningboot"
)

type bootEnvironment struct {
	values   map[string]string
	writes   map[string]int
	getErr   map[string]error
	writeErr map[string]error
	onWrite  func(string)
}

func (b *bootEnvironment) Get(name string) (string, error) {
	return b.values[name], b.getErr[name]
}

func (b *bootEnvironment) Set(values map[string]string) error {
	for name := range values {
		if err := b.writeErr[name]; err != nil {
			return err
		}
	}
	for name, value := range values {
		b.write(name)
		if b.values == nil {
			b.values = make(map[string]string)
		}
		b.values[name] = value
	}
	return nil
}

func (b *bootEnvironment) Unset(names ...string) error {
	for _, name := range names {
		if err := b.writeErr[name]; err != nil {
			return err
		}
	}
	for _, name := range names {
		b.write(name)
		delete(b.values, name)
	}
	return nil
}

func (b *bootEnvironment) write(name string) {
	if b.onWrite != nil {
		b.onWrite(name)
	}
	if b.writes == nil {
		b.writes = make(map[string]int)
	}
	b.writes[name]++
}

func stoppedTuningBoot(t *testing.T, in simRun, wrap func(*journal.Journal) Journal) (Stop, error) {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	return simulateBoot(ctx, in, wrap)
}

func TestTuningBootResetsCountOnlyAfterFirstDurableAppend(t *testing.T) {
	t.Parallel()
	in := simInput(t.TempDir(), newSim(t, small()))
	bl := &fakeBootloader{}
	bl.values = map[string]string{tuningboot.CountVariable: "2"}
	in.Bootloader = bl
	var durable bool
	bl.onWrite = func(name string) {
		if name == tuningboot.CountVariable && !durable {
			t.Fatal("restart count changed before a durable journal append")
		}
	}
	wrap := func(j *journal.Journal) Journal {
		return &observedBootJournal{Journal: j, append: func(p journal.Payload, cause []int) (journal.Event, error) {
			if !durable && bl.values[tuningboot.CountVariable] != "2" {
				t.Fatal("restart count reset before Append")
			}
			e, err := j.Append(p, cause...)
			if err == nil {
				durable = true
			}
			return e, err
		}}
	}
	stop, err := stoppedTuningBoot(t, in, wrap)
	if err != nil || stop.Reason != StopSignal {
		t.Fatalf("run: stop=%+v err=%v", stop, err)
	}
	if !durable || len(readEvents(t, in.Dir)) < 2 || bl.writes[tuningboot.CountVariable] != 1 {
		t.Fatalf("durable=%v count writes=%d", durable, bl.writes[tuningboot.CountVariable])
	}
	if count := bl.values[tuningboot.CountVariable]; count != "" && count != "0" {
		t.Fatalf("restart count after progress: %q", count)
	}
}

type observedBootJournal struct {
	Journal
	append func(journal.Payload, []int) (journal.Event, error)
}

func (j *observedBootJournal) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	return j.append(p, cause)
}

func TestTuningBootFailedAppendDoesNotResetCount(t *testing.T) {
	t.Parallel()
	in := simInput(t.TempDir(), newSim(t, small()))
	bl := &fakeBootloader{}
	bl.values = map[string]string{tuningboot.CountVariable: "2"}
	in.Bootloader = bl
	failure := errors.New("root filesystem cannot persist writes")
	_, err := stoppedTuningBoot(t, in, func(j *journal.Journal) Journal {
		return &observedBootJournal{Journal: j, append: func(journal.Payload, []int) (journal.Event, error) {
			return journal.Event{}, failure
		}}
	})
	if !errors.Is(err, failure) || bl.writes[tuningboot.CountVariable] != 0 || bl.values[tuningboot.CountVariable] != "2" {
		t.Fatalf("failed append: count=%q writes=%d err=%v", bl.values[tuningboot.CountVariable], bl.writes[tuningboot.CountVariable], err)
	}
}

func TestTuningBootReasonReadFailureDoesNotAppend(t *testing.T) {
	t.Parallel()
	in := simInput(t.TempDir(), newSim(t, small()))
	failure := errors.New("cannot read ESP")
	bl := &fakeBootloader{}
	bl.values = map[string]string{tuningboot.CountVariable: "2"}
	bl.getErr = map[string]error{tuningboot.ReasonVariable: failure}
	in.Bootloader = bl
	appends := 0
	_, err := stoppedTuningBoot(t, in, func(j *journal.Journal) Journal {
		return &observedBootJournal{Journal: j, append: func(p journal.Payload, cause []int) (journal.Event, error) {
			appends++
			return j.Append(p, cause...)
		}}
	})
	if !errors.Is(err, failure) || appends != 0 || len(bl.writes) != 0 {
		t.Fatalf("reason read failure: err=%v appends=%d GRUB writes=%v", err, appends, bl.writes)
	}
}

func TestTuningBootReasonSurvivesEveryImportBoundary(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name         string
		kind         journal.Kind
		after        bool
		clearFailure bool
		archive      bool
	}{
		{name: "uninterrupted"},
		{name: "before reason append", kind: journal.KindBootLeaveReason},
		{name: "after durable reason before clear", kind: journal.KindBootLeaveReason, after: true},
		{name: "after durable reason then archive", kind: journal.KindBootLeaveReason, after: true, archive: true},
		{name: "GRUB clear failure after reason", clearFailure: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			in := simInput(t.TempDir(), newSim(t, small()))
			bl := &fakeBootloader{}
			want := tuningboot.Reason{ID: "prior-boot-reason", Count: 3, Reason: "service restart limit exhausted; returning to the normal system"}
			if err := tuningboot.WriteReason(bl, want); err != nil {
				t.Fatal(err)
			}
			in.Bootloader = bl
			if tc.clearFailure {
				bl.writeErr = map[string]error{tuningboot.ReasonVariable: errKilled}
			}
			wrap := func(j *journal.Journal) Journal { return j }
			if tc.kind != "" {
				wrap = func(j *journal.Journal) Journal {
					return &bootCleanupJournal{Journal: j, kind: tc.kind, after: tc.after}
				}
			}
			_, err := stoppedTuningBoot(t, in, wrap)
			interrupted := tc.kind != "" || tc.clearFailure
			if interrupted != errors.Is(err, errKilled) {
				t.Fatalf("first run: %v", err)
			}
			if interrupted && bl.values[tuningboot.ReasonVariable] == "" {
				t.Fatal("reason lost before successful journal-and-clear handoff")
			}
			var archivedEvents []journal.Event
			if tc.archive {
				archivedEvents = readEvents(t, in.Dir)
				j, err := journal.Open(in.Dir, journal.Options{Build: Build(), Sync: true})
				if err != nil {
					t.Fatal(err)
				}
				_, archiveErr := j.Archive("20261002T011407Z")
				closeErr := j.Close()
				if err := errors.Join(archiveErr, closeErr); err != nil {
					t.Fatal(err)
				}
			}
			bl.writeErr = nil
			if _, err := stoppedTuningBoot(t, in, func(j *journal.Journal) Journal { return j }); err != nil {
				t.Fatalf("resume: %v", err)
			}
			var reasons []*journal.BootLeaveReason
			for _, events := range [][]journal.Event{archivedEvents, readEvents(t, in.Dir)} {
				for _, e := range events {
					if p, ok := e.Data.(*journal.BootLeaveReason); ok {
						reasons = append(reasons, p)
					}
				}
			}
			if diff := cmp.Diff([]*journal.BootLeaveReason{{ReasonID: want.ID, RestartLimitCount: want.Count, Reason: want.Reason}}, reasons); diff != "" {
				t.Fatalf("imported reasons (-want +got):\n%s", diff)
			}
			if bl.values[tuningboot.ReasonVariable] != "" {
				t.Fatal("journaled reason was not cleared")
			}
		})
	}
}

func TestTuningBootGRUBProgressFailuresAreNotJournalFailures(t *testing.T) {
	t.Parallel()
	for _, name := range []string{tuningboot.CountVariable, tuningboot.ReasonVariable} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			r, _, closeJournal := checkedRunner(t, []int{-20, -20})
			defer closeJournal()
			failure := errors.New("ESP write failed")
			bl := &fakeBootloader{}
			if err := tuningboot.WriteReason(bl, tuningboot.Reason{ID: "pending", Count: 2, Reason: "service restart limit hit"}); err != nil {
				t.Fatal(err)
			}
			bl.writeErr = map[string]error{name: failure}
			r.in.Bootloader = bl
			var err error
			r.bootReason, err = tuningboot.ReadReason(bl)
			if err != nil {
				t.Fatal(err)
			}
			_, err = r.append(&journal.SessionNotice{Notice: "durable progress"})
			if !errors.Is(err, failure) || r.fatal != nil {
				t.Fatalf("GRUB failure misclassified: err=%v journal fatal=%v", err, r.fatal)
			}
			if bl.values[tuningboot.ReasonVariable] == "" {
				t.Fatal("GRUB failure discarded pending reason")
			}
		})
	}
}

func TestDeadEndWritesBoundedReasonAndReportsWriteFailure(t *testing.T) {
	t.Parallel()
	for _, fail := range []bool{false, true} {
		t.Run(fmt.Sprint(fail), func(t *testing.T) {
			t.Parallel()
			in := simInput(t.TempDir(), newSim(t, small()))
			in.Machine.FailCheck("root", "uid 1000\n"+strings.Repeat("x", 300))
			bl := &fakeBootloader{}
			if fail {
				bl.writeErr = map[string]error{tuningboot.ReasonVariable: errors.New("ESP is read-only")}
			}
			in.Bootloader = bl
			stop := simulate(t, in)
			if stop.Reason != StopDeadEnd || bl.calls != 1 {
				t.Fatalf("dead end failed to clear entry: stop=%+v calls=%d", stop, bl.calls)
			}
			events := readEvents(t, in.Dir)
			index := slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindBootSavedEntry })
			entry := events[index].Data.(*journal.BootSavedEntry)
			if (entry.ReasonError != "") != fail || entry.Error != "" {
				t.Fatalf("reason failure changed clear outcome: %+v", entry)
			}
			if !fail {
				reason, err := tuningboot.ReadReason(bl)
				if err != nil || reason == nil || !strings.HasPrefix(reason.Reason, "dead end preflight:") || strings.ContainsAny(reason.Reason, "\r\n") || len(reason.Reason) > 160 {
					t.Fatalf("saved dead-end reason: %+v %v", reason, err)
				}
			}
		})
	}
}

func TestRestartLimitSequenceDemo(t *testing.T) {
	in := simInput(t.TempDir(), newSim(t, small()))
	bl := &effectfulBootloader{saved: "togi"}
	for hit := 1; hit <= 3; hit++ {
		count, retry, err := tuningboot.RestartLimit(bl)
		if err != nil || count != hit || retry != (hit < 3) || (bl.saved == "togi") != retry {
			t.Fatalf("hit %d: count=%d retry=%v saved_entry=%q err=%v", hit, count, retry, bl.saved, err)
		}
		destination := "normal system"
		if retry {
			destination = "tuning boot"
		}
		t.Logf("restart-limit hit %d: reboot into %s (saved_entry=%q)", hit, destination, bl.saved)
	}
	in.Bootloader = bl
	if _, err := stoppedTuningBoot(t, in, func(j *journal.Journal) Journal { return j }); err != nil {
		t.Fatal(err)
	}
	for _, e := range readEvents(t, in.Dir) {
		if e.Kind == journal.KindBootLeaveReason {
			t.Log(string(e.Raw))
			return
		}
	}
	t.Fatal("next run did not journal leave reason")
}
