package defect

import (
	"slices"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/tuner"
)

func fixture(t *testing.T) []journal.Event {
	t.Helper()
	events, torn, err := journal.ReadFile("testdata/power-off.jsonl")
	if err != nil || len(torn) != 0 {
		t.Fatalf("read pre-fix journal: %v, %d torn bytes", err, len(torn))
	}
	return events
}

func TestPowerOffDefect(t *testing.T) {
	for _, tc := range []struct {
		name string
		edit func([]journal.Event)
		want bool
	}{
		{"pre-fix clean exit and unrelated crash", nil, true},
		{"fixed before decision", func(e []journal.Event) { e[1].Data.(*journal.ConfigLoaded).Fixes = 1 }, false},
		{"failure has different signal", func(e []journal.Event) {
			e[10].Data.(*journal.TrialEnd).Signal = "crash"
			e[11].Data.(*journal.Failure).Signal = "crash"
		}, false},
		{"shutdown outside five seconds", func(e []journal.Event) { e[13].Time = e[10].Time.Add(6 * time.Second) }, false},
		{"shutdown from another boot", func(e []journal.Event) { e[13].Boot = "boot-c" }, false},
		{"unrelated shutdown reason", func(e []journal.Event) { e[13].Data.(*journal.Shutdown).Reason = journal.ShutdownRotations }, false},
		{"decision does not cite the failure", func(e []journal.Event) { e[12].Cause = []int{11} }, false},
		{"no clean exit progress", func(e []journal.Event) {
			e[9].Data.(*journal.TrialProgress).Detail = "core 10 backend exited early: exit status 1"
		}, false},
		{"progress belongs to another trial", func(e []journal.Event) { e[9].Data.(*journal.TrialProgress).Trial = "0002" }, false},
		{"progress belongs to another core", func(e []journal.Event) {
			e[9].Data.(*journal.TrialProgress).Detail = "core 07 backend exited early: <nil>"
		}, false},
		{"clean exit progress after trial end", func(e []journal.Event) { e[9].Seq = 22 }, false},
		{"genuine crash then Ctrl-C within five seconds", func(e []journal.Event) {
			e[20].Boot = "boot-b"
			e[20].Time = e[17].Time.Add(200 * time.Millisecond)
			e[9].Data.(*journal.TrialProgress).Detail = "core 10 backend exited early: exit status 1"
		}, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			events := fixture(t)
			if tc.edit != nil {
				tc.edit(events)
			}
			got := FindWith(events, Entries())
			if !tc.want {
				if len(got) != 0 {
					t.Fatalf("unexpected findings: %+v", got)
				}
				return
			}
			if len(got) != 1 || got[0].Entry.ID != 1 || !slices.Equal(got[0].Cores, []int{10}) || !slices.Equal(got[0].Decisions, []int{13}) {
				t.Fatalf("findings %+v; want defect 1, only core 10 and decision #13", got)
			}
			events = append(events, journal.Event{Seq: 22, Kind: journal.KindDefectFound, Data: &journal.DefectFound{ID: 1}})
			if got := FindWith(events, Entries()); len(got) != 0 {
				t.Fatalf("second resume repeated finding: %+v", got)
			}
		})
	}
}

func TestPowerOffFixtureReplaysBackoff(t *testing.T) {
	events := fixture(t)
	var state journal.State
	engine := tuner.New()
	journal.Replay(events[:12], &state, engine)
	action := engine.Next()
	decision, ok := action.Payload.(*journal.TunerDecision)
	if !ok || decision.Core != 10 || decision.Decision != journal.Backoff || decision.FromOffset != -50 || decision.ToOffset != -45 || !slices.Equal(action.Cause, []int{12}) {
		t.Fatalf("tuner decision after attributed failure: %+v; want core 10 backoff -50 to -45 citing failure #12", action)
	}
	journal.Replay(events[12:], &state, engine)
	engine.Project(&state)
	for _, core := range state.Cores {
		if core.Core == 10 {
			if core.Offset != -45 || core.FailedMark == nil || *core.FailedMark != -50 || core.LastDecision == nil || core.LastDecision.Seq != 13 {
				t.Fatalf("replayed core 10: %+v", core)
			}
			return
		}
	}
	t.Fatal("replayed journal missing core 10")
}

func TestFailuresWithKeepsDefectEvidenceAfterFinding(t *testing.T) {
	events := fixture(t)
	events = append(events, journal.Event{Seq: 22, Boot: "boot-c", Kind: journal.KindDefectFound, Data: &journal.DefectFound{ID: 1}})
	if diff := cmp.Diff([]int{12}, FailuresWith(events, Entries())); diff != "" {
		t.Fatalf("defect failure exclusion (-want +got):\n%s", diff)
	}
	later := DecisionMatch{Kind: journal.KindTunerDecision, Decision: journal.Backoff, Cause: journal.KindFailure, Predicate: func(_ Evidence, _, cause journal.Event) bool { return cause.Seq == 19 }}
	earlier := DecisionMatch{Kind: journal.KindTunerDecision, Decision: journal.Backoff, Cause: journal.KindFailure, Predicate: func(_ Evidence, _, cause journal.Event) bool { return cause.Seq == 12 }}
	list := []Entry{
		{ID: 1, Decisions: []DecisionMatch{{Kind: journal.KindTunerDecision, Decision: journal.StepDeeper, Cause: journal.KindTrialEnd}, later}},
		{ID: 2, Decisions: []DecisionMatch{earlier}},
		{ID: 3, Decisions: []DecisionMatch{later}},
	}
	if diff := cmp.Diff([]int{12, 19}, FailuresWith(events, list)); diff != "" {
		t.Fatalf("unique sorted failure causes (-want +got):\n%s", diff)
	}
	events[1].Data.(*journal.ConfigLoaded).Fixes = 1
	if got := FailuresWith(events, Entries()); len(got) != 0 {
		t.Fatalf("fixed build failures excluded: %v", got)
	}
}

func TestUnansweredMatchesAnswersByDefectID(t *testing.T) {
	first := journal.DefectFound{ID: 1, Cores: []int{2}, Decisions: []int{10}}
	second := journal.DefectFound{ID: 2, Cores: []int{3}, Decisions: []int{20}}
	for _, answer := range []string{"yes", "no"} {
		t.Run(answer, func(t *testing.T) {
			events := []journal.Event{
				{Data: &first},
				{Data: &journal.DefectAnswered{ID: 1, Answer: answer}},
				{Data: &second},
				{Data: &journal.DefectAnswered{ID: 99, Answer: answer}},
			}
			if diff := cmp.Diff([]journal.DefectFound{second}, Unanswered(events)); diff != "" {
				t.Fatalf("pending operator findings (-want +got):\n%s", diff)
			}
		})
	}
}
