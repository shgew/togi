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
	"github.com/google/go-cmp/cmp/cmpopts"

	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

var (
	binary  = journal.Build{Schema: journal.Schema, Ruleset: 4}
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
	w.add(&journal.SessionStart{Schema: journal.Schema, Ruleset: ruleset, Session: session, Cores: cores})
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
	return journal.CarriedSource{Session: session, Path: filepath.Join("archive", session+".jsonl"), Schema: journal.Schema, Ruleset: ruleset}
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

func TestPrepareSeedsSoloLimitsAndFailurePoints(t *testing.T) {
	dir := t.TempDir()
	w := newJournal(t, dir, "X", 3, &context)
	w.pass(0, -30, machine.Alone)
	soloLimit0 := w.pass(0, -35, machine.Alone)
	failurePoint0 := w.fail(0, -36, machine.Alone, journal.Attributed)
	failurePoint1 := w.fail(1, -40, machine.Together, journal.Attributed)
	soloLimit1 := w.pass(1, -40, machine.Alone)
	w.pass(1, -45, machine.Together)
	w.fail(2, -20, machine.Together, journal.Unattributed)
	failurePoint6 := w.fail(6, 0, machine.Alone, journal.Attributed)
	w.close()

	got := prepare(t, dir, []defect.Entry{})
	want := &Carry{
		Sources: []journal.CarriedSource{src("X", 3)},
		Context: &context,
		Cores: []journal.CarriedCore{
			{Core: 0, CandidateSoloLimit: new(-35), CandidateSoloLimitSession: "X", CandidateSoloLimitSeq: soloLimit0, FailurePoint: new(-36), FailurePointSession: "X", FailurePointSeq: failurePoint0, FailurePointSignal: machine.UnexpectedExit},
			{Core: 1, CandidateSoloLimit: new(-40), CandidateSoloLimitSession: "X", CandidateSoloLimitSeq: soloLimit1, FailurePoint: new(-40), FailurePointSession: "X", FailurePointSeq: failurePoint1, FailurePointSignal: machine.UnexpectedExit},
			{Core: 6, FailurePoint: new(0), FailurePointSession: "X", FailurePointSeq: failurePoint6, FailurePointSignal: machine.UnexpectedExit},
		},
	}
	if diff := cmp.Diff(want, got, cmpopts.IgnoreUnexported(Carry{})); diff != "" {
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
			failurePoint := w.fail(0, -30, machine.Alone, journal.Attributed)
			w.close()
			got, err := prepareWithContext(dir, []defect.Entry{}, tc.current)
			if err != nil {
				t.Fatal(err)
			}
			var want *Carry
			if tc.archive {
				want = &Carry{Sources: []journal.CarriedSource{src("X", 4)}, Context: tc.recorded, Cores: []journal.CarriedCore{
					{Core: 0, FailurePoint: new(-30), FailurePointSession: "X", FailurePointSeq: failurePoint, FailurePointSignal: machine.UnexpectedExit},
				}}
			}
			if diff := cmp.Diff(want, got, cmpopts.IgnoreUnexported(Carry{})); diff != "" {
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
	failurePoint := w.fail(0, -30, machine.Alone, journal.Attributed)
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
		{Core: 0, FailurePoint: new(-30), FailurePointSession: "X", FailurePointSeq: failurePoint, FailurePointSignal: machine.UnexpectedExit},
	}}
	if diff := cmp.Diff(want, got, cmpopts.IgnoreUnexported(Carry{})); diff != "" {
		t.Fatalf("carry (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(got, prepare(t, dir, []defect.Entry{}), cmpopts.IgnoreUnexported(Carry{})); diff != "" {
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
			failure := w.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Regime: machine.R7, Condition: machine.Together})
			w.add(&journal.HuntStart{Hunt: 1, Failure: failure, Failing: []int{-10, -35, -20}, Candidates: []int{2}})
			end := w.add(&journal.HuntEnd{Hunt: 1, Result: "culprit", Cores: []int{2}})
			if tc.reset {
				w.add(&journal.CommandReset{Core: new(2)})
			}
			attributedSeq := 0
			if tc.attributed != 0 {
				attributedSeq = w.fail(2, tc.attributed, machine.Together, journal.Attributed)
			}
			w.close()
			got := prepare(t, dir, []defect.Entry{})
			var want []journal.CarriedCore
			if tc.wantOffset != 0 {
				seq, signal := end, machine.Crash
				if tc.attributed != 0 && (tc.reset || tc.attributed > -35) {
					seq, signal = attributedSeq, machine.UnexpectedExit
				}
				want = []journal.CarriedCore{{Core: 2, FailurePoint: new(tc.wantOffset), FailurePointSession: "X", FailurePointSeq: seq, FailurePointSignal: signal}}
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
	w.pass(2, -30, machine.Alone)
	w.fail(2, -31, machine.Alone, journal.Attributed)
	w.add(&journal.CommandReset{Core: new(2)})
	failurePoint := w.fail(2, -10, machine.Alone, journal.Attributed)
	w.close()

	got := prepare(t, dir, []defect.Entry{})
	want := []journal.CarriedCore{{Core: 2, FailurePoint: new(-10), FailurePointSession: "X", FailurePointSeq: failurePoint, FailurePointSignal: machine.UnexpectedExit}}
	if diff := cmp.Diff(want, got.Cores); diff != "" {
		t.Fatalf("cores (-want +got):\n%s", diff)
	}
}

func TestPrepareSkipsFailuresBehindADefect(t *testing.T) {
	dir := t.TempDir()
	w := newJournal(t, dir, "X", 3, &context)
	failure := w.fail(3, -25, machine.Alone, journal.Attributed)
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

func TestPrepareRecordsFailurePointForTrialInFlight(t *testing.T) {
	for _, tc := range []struct {
		name         string
		offset       int
		condition    machine.Condition
		failurePoint bool
		recordOnly   bool
	}{
		{"alone", -20, machine.Alone, true, false},
		{"at CO 0", 0, machine.Alone, false, false},
		{"together", -20, machine.Together, false, false},
		{"record-only alone", -20, machine.Alone, false, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			w := newJournal(t, dir, "X", 3, &context)
			w.add(&journal.TrialIntent{Trial: "0001", Core: new(4), Offset: new(tc.offset), Regime: machine.R6, Condition: tc.condition, RecordOnly: tc.recordOnly})
			w.close()

			var want []journal.CarriedCore
			if tc.failurePoint {
				want = []journal.CarriedCore{{Core: 4, FailurePoint: new(-20), FailurePointSession: "X", FailurePointSeq: 3, FailurePointSignal: machine.Crash}}
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
	w.intent(4, -20, machine.Alone)
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
			old.fail(7, -30, machine.Alone, journal.Attributed)
			old.archive(dir)
			w := newJournal(t, dir, "X", 3, &context)
			w.add(&journal.SessionCarried{Sources: []journal.CarriedSource{src("A", 2)}, FailurePoints: true, Carried: []journal.CarriedCore{
				{Core: 5, FailurePoint: new(-12), FailurePointSession: "A", FailurePointSeq: 7, FailurePointSignal: machine.Crash},
			}})
			if reset {
				w.add(&journal.CommandReset{Core: new(5)})
			}
			w.close()

			got := prepare(t, dir, []defect.Entry{})
			want := &Carry{Sources: []journal.CarriedSource{src("X", 3)}, Context: &context}
			if !reset {
				want.Cores = []journal.CarriedCore{{Core: 5, FailurePoint: new(-12), FailurePointSession: "A", FailurePointSeq: 7, FailurePointSignal: machine.Crash}}
			}
			if diff := cmp.Diff(want, got, cmpopts.IgnoreUnexported(Carry{})); diff != "" {
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
			b.fail(1, -33, machine.Alone, journal.Attributed)
			b.add(&journal.CommandReset{All: true})
			b.add(&journal.SessionArchived{Session: "B", Path: filepath.Join("archive", "B.jsonl")})
			b.archive(dir)
			x := newJournal(t, dir, "X", 3, &context)
			x.pass(2, -40, machine.Alone)
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
	w.fail(0, -28, machine.Alone, journal.Attributed)
	w.close()
	data, err := os.ReadFile(filepath.Join(scratch, "events.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	data = bytes.ReplaceAll(data, []byte(`"schema":3`), []byte(`"schema":1`))
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
	w.fail(0, -30, machine.Alone, journal.Attributed)
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

	if diff := cmp.Diff(first, prepare(t, dir, []defect.Entry{}), cmpopts.IgnoreUnexported(Carry{})); diff != "" {
		t.Fatalf("second Prepare (-first +second):\n%s", diff)
	}

	next := newJournal(t, dir, "Y", 4, &context)
	next.add(&journal.SessionCarried{Sources: first.Sources, FailurePoints: true, Carried: first.Cores})
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
	w.fail(0, -30, machine.Alone, journal.Attributed)
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
			w.fail(0, -30, machine.Alone, journal.Attributed)
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

func TestPrepareDoesNotSeedAfterCorePhases(t *testing.T) {
	dir := t.TempDir()
	a := newJournal(t, dir, "A", 3, &context, machine.CoreInfo{Core: 0})
	a.pass(0, -30, machine.Alone)
	a.close()
	prepare(t, dir, []defect.Entry{})
	b := newJournal(t, dir, "B", 4, &context, machine.CoreInfo{Core: 0})
	if settled, err := recorded(b.j, "A"); err != nil || settled {
		t.Fatalf("session metadata prematurely settled pending carry: %t, %v", settled, err)
	}
	b.add(&journal.CorePhase{Core: 0, To: journal.PhaseSearch, Offset: -5})
	b.close()
	if c := prepare(t, dir, []defect.Entry{}); c != nil {
		t.Fatalf("late carry overwrote started search: %+v", c)
	}
	if pending, err := journal.PendingCarry(dir); err != nil || pending != "" {
		t.Fatalf("late carry marker retained: %q, %v", pending, err)
	}
}

func TestPrepareWalkStopsAtOlderCarryCommitment(t *testing.T) {
	dir := t.TempDir()
	a := newJournal(t, dir, "A", 1, &context, machine.CoreInfo{Core: 0}, machine.CoreInfo{Core: 7})
	candidateSoloLimit := a.pass(0, -30, machine.Alone)
	a.fail(7, -20, machine.Alone, journal.Attributed)
	a.archive(dir)
	b := newJournal(t, dir, "B", 2, &context, machine.CoreInfo{Core: 0}, machine.CoreInfo{Core: 7})
	b.add(&journal.SessionCarried{Sources: []journal.CarriedSource{src("A", 1)}, FailurePoints: true, Carried: []journal.CarriedCore{
		{Core: 0, CandidateSoloLimit: new(-30), CandidateSoloLimitSession: "A", CandidateSoloLimitSeq: candidateSoloLimit},
	}})
	b.archive(dir)
	c := newJournal(t, dir, "C", 3, &context, machine.CoreInfo{Core: 0}, machine.CoreInfo{Core: 7})
	c.close()
	got := prepare(t, dir, []defect.Entry{})
	if diff := cmp.Diff([]journal.CarriedSource{src("C", 3), src("B", 2)}, got.Sources); diff != "" {
		t.Fatalf("committed source boundary (-want +got):\n%s", diff)
	}
	want := []journal.CarriedCore{{Core: 0, CandidateSoloLimit: new(-30), CandidateSoloLimitSession: "A", CandidateSoloLimitSeq: candidateSoloLimit}}
	if diff := cmp.Diff(want, got.Cores); diff != "" {
		t.Fatalf("walk revived omitted old failure point (-want +got):\n%s", diff)
	}
}

func TestPrepareRefusesBrokenArchiveLayoutWithoutChangingJournal(t *testing.T) {
	for _, layout := range []string{"archive is file", "archive target is directory", "invalid session header"} {
		t.Run(layout, func(t *testing.T) {
			dir := t.TempDir()
			w := newJournal(t, dir, "A", 3, &context, machine.CoreInfo{Core: 0})
			w.pass(0, -30, machine.Alone)
			w.close()
			path := filepath.Join(dir, "events.jsonl")
			switch layout {
			case "archive is file":
				if err := os.WriteFile(filepath.Join(dir, "archive"), nil, 0o600); err != nil {
					t.Fatal(err)
				}
				if c, err := compute(dir, "A", nil); err == nil || c != nil {
					t.Fatalf("broken reset-boundary scan accepted: carry %+v, error %v", c, err)
				}
				if fs, err := prepareFacts(dir, "A", nil, &context, 1); err == nil || fs != nil {
					t.Fatalf("broken reset-boundary scan accepted: facts %+v, error %v", fs, err)
				}
			case "archive target is directory":
				if err := os.MkdirAll(filepath.Join(dir, "archive", "A.jsonl"), 0o700); err != nil {
					t.Fatal(err)
				}
			case "invalid session header":
				if err := os.WriteFile(path, []byte("not JSON\n"), 0o600); err != nil {
					t.Fatal(err)
				}
			}
			before, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			if c, err := prepareWithContext(dir, []defect.Entry{}, nil); err == nil || c != nil {
				t.Fatalf("broken archive preparation accepted: carry %+v, error %v", c, err)
			}
			after, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(before, after); diff != "" {
				t.Fatalf("failed preparation changed source journal (-before +after):\n%s", diff)
			}
		})
	}
}
