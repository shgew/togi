package carry

import (
	"bytes"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

var (
	binary  = journal.Build{Schema: 2, Ruleset: 4}
	context = machine.BIOSContext{BIOSVersion: "3.10", Board: "X870E", CPUModel: "9950X", Microcode: "0xb404032", BoostLimitMHz: 5700}
)

func opts() journal.Options {
	now := time.Date(2026, 10, 1, 0, 0, 0, 0, time.UTC)
	return journal.Options{Boot: "boot", Now: func() time.Time { now = now.Add(time.Second); return now }}
}

type writer struct {
	t       *testing.T
	j       *journal.Journal
	session string
	trials  int
}

// newJournal starts events.jsonl in dir for session, stamped with ruleset, recording ctx when it is not nil.
func newJournal(t *testing.T, dir, session string, ruleset int, ctx *machine.BIOSContext, cores ...machine.CoreInfo) *writer {
	t.Helper()
	j, err := journal.Open(dir, opts())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = j.Close() })
	w := &writer{t: t, j: j, session: session}
	w.add(&journal.SessionStart{Schema: 2, Ruleset: ruleset, Session: session, Cores: cores})
	if ctx != nil {
		w.add(&journal.SessionContext{BIOSContext: *ctx})
	}
	return w
}

func (w *writer) add(p journal.Payload, cause ...int) int {
	w.t.Helper()
	e, err := w.j.Append(p, cause...)
	if err != nil {
		w.t.Fatal(err)
	}
	return e.Seq
}

func (w *writer) intent(core, offset int, condition machine.Condition) string {
	w.trials++
	id := fmt.Sprintf("%04d", w.trials)
	w.add(&journal.TrialIntent{Trial: id, Core: new(core), Offset: new(offset), Regime: machine.R6, Condition: condition})
	return id
}

// pass records a passing trial and returns the seq of its trial.end.
func (w *writer) pass(core, offset int, condition machine.Condition) int {
	id := w.intent(core, offset, condition)
	return w.add(&journal.TrialEnd{Trial: id, Outcome: journal.OutcomePass})
}

// fail records a failing trial and returns the seq of its failure.
func (w *writer) fail(core, offset int, condition machine.Condition, attribution journal.Attribution) int {
	id := w.intent(core, offset, condition)
	w.add(&journal.TrialEnd{Trial: id, Outcome: journal.OutcomeFailure, Signal: machine.UnexpectedExit, Core: new(core)})
	return w.add(&journal.Failure{Signal: machine.UnexpectedExit, Attribution: attribution, Core: new(core), Offset: new(offset), Trial: id, Regime: machine.R6, Condition: condition})
}

func (w *writer) close() {
	w.t.Helper()
	if err := w.j.Close(); err != nil {
		w.t.Fatal(err)
	}
}

// archive closes the journal and moves it to archive/<session>.jsonl.
func (w *writer) archive(dir string) {
	w.t.Helper()
	w.close()
	if err := os.MkdirAll(filepath.Join(dir, "archive"), 0o755); err != nil {
		w.t.Fatal(err)
	}
	if err := os.Rename(filepath.Join(dir, "events.jsonl"), filepath.Join(dir, "archive", w.session+".jsonl")); err != nil {
		w.t.Fatal(err)
	}
}

func src(session string, ruleset int) journal.CarriedSource {
	return journal.CarriedSource{Session: session, Path: filepath.Join("archive", session+".jsonl"), Schema: 2, Ruleset: ruleset}
}

func prepare(t *testing.T, dir string, entries []defect.Entry) *Carry {
	t.Helper()
	c, err := prepareWithContext(dir, entries, nil)
	if err != nil {
		t.Fatal(err)
	}
	return c
}

func prepareWithContext(dir string, entries []defect.Entry, current *machine.BIOSContext) (*Carry, error) {
	j, err := journal.Lock(dir, opts())
	if err != nil {
		return nil, err
	}
	defer j.Close()
	return Prepare(j, binary, entries, current)
}

func TestPrepareSeedsEdgesAndMarks(t *testing.T) {
	dir := t.TempDir()
	w := newJournal(t, dir, "X", 3, &context)
	w.pass(0, -30, machine.Isolated)
	edge0 := w.pass(0, -35, machine.Isolated)
	mark0 := w.fail(0, -36, machine.Isolated, journal.Attributed)
	mark1 := w.fail(1, -40, machine.Resident, journal.Attributed)
	edge1 := w.pass(1, -40, machine.Isolated)
	w.pass(1, -45, machine.Resident)
	w.fail(2, -20, machine.Resident, journal.Unattributed)
	mark6 := w.fail(6, 0, machine.Isolated, journal.Attributed)
	w.close()

	got := prepare(t, dir, []defect.Entry{})
	want := &Carry{
		Sources: []journal.CarriedSource{src("X", 3)},
		Context: &context,
		Cores: []journal.CarriedCore{
			{Core: 0, Edge: new(-35), EdgeSession: "X", EdgeSeq: edge0, FailedMark: new(-36), MarkSession: "X", MarkSeq: mark0, MarkSignal: machine.UnexpectedExit},
			{Core: 1, Edge: new(-40), EdgeSession: "X", EdgeSeq: edge1, FailedMark: new(-40), MarkSession: "X", MarkSeq: mark1, MarkSignal: machine.UnexpectedExit},
			{Core: 6, FailedMark: new(0), MarkSession: "X", MarkSeq: mark6, MarkSignal: machine.UnexpectedExit},
		},
	}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("carry (-want +got):\n%s", diff)
	}
}

func TestPrepareBIOSChange(t *testing.T) {
	changed := context
	changed.BIOSVersion = "3.20"
	for _, tc := range []struct {
		name     string
		recorded *machine.BIOSContext
		current  *machine.BIOSContext
		archive  bool
	}{
		{"changed", &context, &changed, true},
		{"unchanged", &context, &context, false},
		{"no current context", &context, nil, false},
		{"no recorded context", nil, &changed, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			w := newJournal(t, dir, "X", 4, tc.recorded)
			mark := w.fail(0, -30, machine.Isolated, journal.Attributed)
			w.close()
			got, err := prepareWithContext(dir, []defect.Entry{}, tc.current)
			if err != nil {
				t.Fatal(err)
			}
			var want *Carry
			if tc.archive {
				want = &Carry{Sources: []journal.CarriedSource{src("X", 4)}, Context: tc.recorded, Cores: []journal.CarriedCore{
					{Core: 0, FailedMark: new(-30), MarkSession: "X", MarkSeq: mark, MarkSignal: machine.UnexpectedExit},
				}}
			}
			if diff := cmp.Diff(want, got); diff != "" {
				t.Fatalf("carry (-want +got):\n%s", diff)
			}
			_, err = os.Stat(filepath.Join(dir, "events.jsonl"))
			if tc.archive && !errors.Is(err, fs.ErrNotExist) || !tc.archive && err != nil {
				t.Fatalf("journal after Prepare: %v", err)
			}
		})
	}
}

func TestPrepareResumesInterruptedBIOSArchive(t *testing.T) {
	dir := t.TempDir()
	w := newJournal(t, dir, "X", 4, &context)
	mark := w.fail(0, -30, machine.Isolated, journal.Attributed)
	if err := os.MkdirAll(filepath.Join(dir, "archive"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "archive", "X-carry-pending"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
	w.add(&journal.SessionArchived{Session: "X", Path: filepath.Join("archive", "X.jsonl")})
	w.close()
	changed := context
	changed.BIOSVersion = "3.20"
	got, err := prepareWithContext(dir, []defect.Entry{}, &changed)
	if err != nil {
		t.Fatal(err)
	}
	want := &Carry{Sources: []journal.CarriedSource{src("X", 4)}, Context: &context, Cores: []journal.CarriedCore{
		{Core: 0, FailedMark: new(-30), MarkSession: "X", MarkSeq: mark, MarkSignal: machine.UnexpectedExit},
	}}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("carry (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(got, prepare(t, dir, []defect.Entry{})); diff != "" {
		t.Fatalf("resumed carry (-want +got):\n%s", diff)
	}
}

func TestPrepareCarriesHuntCulprit(t *testing.T) {
	for _, tc := range []struct {
		name       string
		reset      bool
		attributed int
		wantOffset int
	}{
		{"culprit", false, 0, -35},
		{"reset culprit", true, 0, 0},
		{"attributed failure is shallower", false, -28, -28},
		{"culprit is shallower", false, -40, -35},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			sparse := []machine.CoreInfo{{Core: 0, CPUs: []int{0}}, {Core: 2, CPUs: []int{2}}, {Core: 5, CPUs: []int{5}}}
			w := newJournal(t, dir, "X", 3, &context, sparse...)
			failure := w.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Regime: machine.R7, Condition: machine.Resident})
			w.add(&journal.HuntStart{Hunt: 1, Failure: failure, Failing: []int{-10, -35, -20}, Candidates: []int{2}})
			end := w.add(&journal.HuntEnd{Hunt: 1, Result: "culprit", Cores: []int{2}})
			if tc.reset {
				w.add(&journal.CommandReset{Core: new(2)})
			}
			attributedSeq := 0
			if tc.attributed != 0 {
				attributedSeq = w.fail(2, tc.attributed, machine.Resident, journal.Attributed)
			}
			w.close()
			got := prepare(t, dir, []defect.Entry{})
			var want []journal.CarriedCore
			if tc.wantOffset != 0 {
				seq, signal := end, machine.Crash
				if tc.attributed != 0 && (tc.reset || tc.attributed > -35) {
					seq, signal = attributedSeq, machine.UnexpectedExit
				}
				want = []journal.CarriedCore{{Core: 2, FailedMark: new(tc.wantOffset), MarkSession: "X", MarkSeq: seq, MarkSignal: signal}}
			}
			if diff := cmp.Diff(want, got.Cores); diff != "" {
				t.Fatalf("cores (-want +got):\n%s", diff)
			}
		})
	}
}

func TestPrepareDropsWhatAResetCleared(t *testing.T) {
	dir := t.TempDir()
	w := newJournal(t, dir, "X", 3, &context)
	w.pass(2, -30, machine.Isolated)
	w.fail(2, -31, machine.Isolated, journal.Attributed)
	w.add(&journal.CommandReset{Core: new(2)})
	mark := w.fail(2, -10, machine.Isolated, journal.Attributed)
	w.close()

	got := prepare(t, dir, []defect.Entry{})
	want := []journal.CarriedCore{{Core: 2, FailedMark: new(-10), MarkSession: "X", MarkSeq: mark, MarkSignal: machine.UnexpectedExit}}
	if diff := cmp.Diff(want, got.Cores); diff != "" {
		t.Fatalf("cores (-want +got):\n%s", diff)
	}
}

func TestPrepareSkipsFailuresBehindADefect(t *testing.T) {
	dir := t.TempDir()
	w := newJournal(t, dir, "X", 3, &context)
	failure := w.fail(3, -25, machine.Isolated, journal.Attributed)
	w.add(&journal.TunerDecision{Core: 3, Phase: journal.PhaseSearch, Decision: journal.Backoff, FromOffset: -25, ToOffset: -24}, failure)
	w.close()

	entries := []defect.Entry{{ID: 99, Decisions: []defect.DecisionMatch{{
		Kind: journal.KindTunerDecision, Decision: journal.Backoff, Cause: journal.KindFailure,
		Predicate: func(_ defect.Evidence, _, cause journal.Event) bool { return cause.Seq == failure },
	}}}}
	if got := prepare(t, dir, entries); len(got.Cores) != 0 {
		t.Fatalf("cores: %+v, want none", got.Cores)
	}
}

func TestPrepareMarksATrialInFlight(t *testing.T) {
	for _, tc := range []struct {
		name      string
		offset    int
		condition machine.Condition
		mark      bool
	}{
		{"isolated", -20, machine.Isolated, true},
		{"at CO 0", 0, machine.Isolated, false},
		{"resident", -20, machine.Resident, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			w := newJournal(t, dir, "X", 3, &context)
			w.intent(4, tc.offset, tc.condition)
			w.close()

			var want []journal.CarriedCore
			if tc.mark {
				want = []journal.CarriedCore{{Core: 4, FailedMark: new(-20), MarkSession: "X", MarkSeq: 3, MarkSignal: machine.Crash}}
			}
			if diff := cmp.Diff(want, prepare(t, dir, []defect.Entry{}).Cores); diff != "" {
				t.Fatalf("cores (-want +got):\n%s", diff)
			}
		})
	}
}

func TestPrepareIgnoresATrialStoppedByAShutdown(t *testing.T) {
	dir := t.TempDir()
	w := newJournal(t, dir, "X", 3, &context)
	w.intent(4, -20, machine.Isolated)
	w.add(&journal.Shutdown{Reason: journal.ShutdownSignal})
	w.close()
	if got := prepare(t, dir, []defect.Entry{}); len(got.Cores) != 0 {
		t.Fatalf("cores: %+v, want none", got.Cores)
	}
}

func TestPrepareChainsACarryAndWalksNoFurther(t *testing.T) {
	for _, reset := range []bool{false, true} {
		t.Run(fmt.Sprintf("reset %v", reset), func(t *testing.T) {
			dir := t.TempDir()
			old := newJournal(t, dir, "A", 2, &context)
			old.fail(7, -30, machine.Isolated, journal.Attributed)
			old.archive(dir)
			w := newJournal(t, dir, "X", 3, &context)
			w.add(&journal.SessionCarried{Sources: []journal.CarriedSource{src("A", 2)}, Marks: true, Carried: []journal.CarriedCore{
				{Core: 5, FailedMark: new(-12), MarkSession: "A", MarkSeq: 7, MarkSignal: machine.Crash},
			}})
			if reset {
				w.add(&journal.CommandReset{Core: new(5)})
			}
			w.close()

			got := prepare(t, dir, []defect.Entry{})
			want := &Carry{Sources: []journal.CarriedSource{src("X", 3)}, Context: &context}
			if !reset {
				want.Cores = []journal.CarriedCore{{Core: 5, FailedMark: new(-12), MarkSession: "A", MarkSeq: 7, MarkSignal: machine.Crash}}
			}
			if diff := cmp.Diff(want, got); diff != "" {
				t.Fatalf("carry (-want +got):\n%s", diff)
			}
		})
	}
}

func TestPrepareWalksBackThroughUnseededTransitions(t *testing.T) {
	other := context
	other.BIOSVersion = "3.20"
	for _, tc := range []struct {
		name     string
		aContext *machine.BIOSContext
		bRuleset int
		want     []journal.CarriedSource
		cores    []int
	}{
		{"every transition", &context, 2, []journal.CarriedSource{src("X", 3), src("B", 2), {Session: "A", Path: filepath.Join("archive", "A.jsonl"), Schema: 1, Ruleset: 1}}, []int{0, 1, 2}},
		{"BIOS changed before B", &other, 2, []journal.CarriedSource{src("X", 3), src("B", 2)}, []int{1, 2}},
		{"B ran the same ruleset", &context, 3, []journal.CarriedSource{src("X", 3)}, []int{2}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			writeSchema1(t, dir, "A", tc.aContext)
			b := newJournal(t, dir, "B", tc.bRuleset, &context)
			b.fail(1, -33, machine.Isolated, journal.Attributed)
			b.add(&journal.CommandReset{All: true})
			b.add(&journal.SessionArchived{Session: "B", Path: filepath.Join("archive", "B.jsonl")})
			b.archive(dir)
			x := newJournal(t, dir, "X", 3, &context)
			x.pass(2, -40, machine.Isolated)
			x.close()

			got := prepare(t, dir, []defect.Entry{})
			if diff := cmp.Diff(tc.want, got.Sources); diff != "" {
				t.Fatalf("sources (-want +got):\n%s", diff)
			}
			var cores []int
			for _, c := range got.Cores {
				cores = append(cores, c.Core)
			}
			if diff := cmp.Diff(tc.cores, cores); diff != "" {
				t.Fatalf("carried cores (-want +got):\n%s", diff)
			}
		})
	}
}

// writeSchema1 archives a session in the shape schema 1 wrote: no ruleset stamp, and a kind later schemas dropped.
func writeSchema1(t *testing.T, dir, session string, ctx *machine.BIOSContext) {
	t.Helper()
	scratch := t.TempDir()
	w := newJournal(t, scratch, session, 0, ctx)
	w.fail(0, -28, machine.Isolated, journal.Attributed)
	w.close()
	data, err := os.ReadFile(filepath.Join(scratch, "events.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	data = bytes.ReplaceAll(data, []byte(`"schema":2`), []byte(`"schema":1`))
	data = append(data, `{"seq":99,"time":"2026-09-24T20:43:52Z","boot":"boot","kind":"escalation.window","msg":"escalation window","window_s":600}`+"\n"...)
	if err := os.MkdirAll(filepath.Join(dir, "archive"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "archive", session+".jsonl"), data, 0o644); err != nil {
		t.Fatal(err)
	}
}

func TestPrepareLifecycle(t *testing.T) {
	dir := t.TempDir()
	w := newJournal(t, dir, "X", 3, &context)
	w.fail(0, -30, machine.Isolated, journal.Attributed)
	w.close()
	before, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
	if err != nil {
		t.Fatal(err)
	}

	first := prepare(t, dir, []defect.Entry{})
	if _, err := os.Stat(filepath.Join(dir, "events.jsonl")); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("events.jsonl after the transition: %v", err)
	}
	if _, err := os.Stat(filepath.Join(dir, "archive", "X-carry-pending")); err != nil {
		t.Fatalf("carry marker: %v", err)
	}
	if after, err := os.ReadFile(filepath.Join(dir, "archive", "X.jsonl")); err != nil || !bytes.Equal(after, before) {
		t.Fatalf("archived journal changed: %v", err)
	}

	if diff := cmp.Diff(first, prepare(t, dir, []defect.Entry{})); diff != "" {
		t.Fatalf("second Prepare (-first +second):\n%s", diff)
	}

	next := newJournal(t, dir, "Y", 4, &context)
	next.add(&journal.SessionCarried{Sources: first.Sources, Marks: true, Carried: first.Cores})
	next.close()
	if got := prepare(t, dir, []defect.Entry{}); got != nil {
		t.Fatalf("carry after it was recorded: %+v", got)
	}
	if _, err := os.Stat(filepath.Join(dir, "archive", "X-carry-pending")); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("carry marker after it was recorded: %v", err)
	}
}

func TestPrepareLeavesANewerJournal(t *testing.T) {
	dir := t.TempDir()
	w := newJournal(t, dir, "X", 5, &context)
	w.fail(0, -30, machine.Isolated, journal.Attributed)
	w.close()
	before, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	if got := prepare(t, dir, []defect.Entry{}); got != nil {
		t.Fatalf("carry from a newer journal: %+v", got)
	}
	if after, err := os.ReadFile(filepath.Join(dir, "events.jsonl")); err != nil || !bytes.Equal(after, before) {
		t.Fatalf("newer journal changed: %v", err)
	}
}

func TestPrepareCarriesNothingFromAnArchivedJournal(t *testing.T) {
	for _, torn := range []bool{false, true} {
		t.Run(fmt.Sprintf("torn tail %v", torn), func(t *testing.T) {
			dir := t.TempDir()
			w := newJournal(t, dir, "X", 3, &context)
			w.fail(0, -30, machine.Isolated, journal.Attributed)
			w.add(&journal.SessionArchived{Session: "X", Path: filepath.Join("archive", "X.jsonl")})
			w.close()
			if torn {
				f, err := os.OpenFile(filepath.Join(dir, "events.jsonl"), os.O_APPEND|os.O_WRONLY, 0)
				if err != nil {
					t.Fatal(err)
				}
				if _, err := f.WriteString(`{"seq":9,"kind":"tri`); err != nil {
					t.Fatal(err)
				}
				if err := f.Close(); err != nil {
					t.Fatal(err)
				}
			}
			before, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
			if err != nil {
				t.Fatal(err)
			}
			if got := prepare(t, dir, []defect.Entry{}); got != nil {
				t.Fatalf("carry from an archived journal: %+v", got)
			}
			if after, err := os.ReadFile(filepath.Join(dir, "archive", "X.jsonl")); err != nil || !bytes.Equal(after, before) {
				t.Fatalf("archived journal changed: %v", err)
			}
			if _, err := os.Stat(filepath.Join(dir, "archive", "X-carry-pending")); !errors.Is(err, fs.ErrNotExist) {
				t.Fatalf("carry marker: %v", err)
			}
		})
	}
}

func TestPrepareRefusesALineWithoutAKind(t *testing.T) {
	dir := t.TempDir()
	w := newJournal(t, dir, "X", 3, &context)
	w.archive(dir)
	if err := os.WriteFile(filepath.Join(dir, "archive", "X-carry-pending"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
	f, err := os.OpenFile(filepath.Join(dir, "archive", "X.jsonl"), os.O_APPEND|os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := f.WriteString(`{"seq":3,"attribution":"attributed","core":0,"offset":-30}` + "\n"); err != nil {
		t.Fatal(err)
	}
	if err := f.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := prepareWithContext(dir, []defect.Entry{}, nil); err == nil || !strings.Contains(err.Error(), "no kind") {
		t.Fatalf("Prepare: %v, want a refusal of the line without a kind", err)
	}
}
