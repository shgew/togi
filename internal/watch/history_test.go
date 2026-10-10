package watch

import (
	"fmt"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

func historySentences(s Snapshot) []string {
	var lines []string
	for _, h := range slices.Backward(s.history) {
		lines = append(lines, h.tag+": "+h.sentence())
	}
	return lines
}

func TestProjectRebootHistoryDuringSMUApplication(t *testing.T) {
	t.Parallel()
	events := dashboardEvents(dashboardSession(),
		&journal.ProfileApplied{Offsets: []int{-10, -10, -10}, Condition: machine.Together},
		&journal.SMUIntent{Op: journal.SMUSet, Core: new(0), Offset: -20},
		&journal.CrashDetected{PreviousBoot: "boot", InFlight: new(3), Condition: machine.Together})
	events[3].Boot = "next"
	s := Project(events)
	if diff := cmp.Diff([]string{
		"start: session started on 3 cores",
		"crash: with the offsets applied, rebooted",
	}, historySentences(s)); diff != "" {
		t.Fatalf("an in-flight SMU write must retain the reboot without inventing a trial (-want +got):\n%s", diff)
	}
	if s.crashes != 1 || s.history[0].reboot != events[3].Seq || s.history[0].tone != badTone {
		t.Fatalf("SMU reboot provenance or severity lost: crashes=%d, history=%+v", s.crashes, s.history)
	}
}

func TestProjectRebootHistoryDuringTrial(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name     string
		signal   machine.Signal
		evidence journal.Payload
		wantEnd  string
	}{
		{"ordinary crash", machine.Crash, nil, "crash: R2 heavy vector on core 00 at -20 · rebooted · no core named"},
		{"retained computation failure", machine.ComputationError, &journal.TrialProgress{Trial: "trial", Signal: machine.ComputationError, Core: new(0)}, "fail: R2 heavy vector on core 00 at -20 · wrong result · no core named"},
		{"retained corrected MCE", machine.CorrectedMCE, &journal.MCE{Trial: "trial", Core: 0, Corrected: true, FromBoot: "boot"}, "fail: R2 heavy vector on core 00 at -20 · corrected hardware error · no core named"},
		{"retained uncorrected MCE", machine.UncorrectedMCE, &journal.MCE{Trial: "trial", Core: 0, FromBoot: "boot"}, "fail: R2 heavy vector on core 00 at -20 · uncorrected hardware error · no core named"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			payloads := []journal.Payload{dashboardSession(),
				&journal.TrialIntent{Trial: "trial", Core: new(0), Condition: machine.Together, Phase: journal.PhaseChecking, Regime: machine.R2, Workload: "mprime-avx2-36k-248k", Profile: []int{-20, -10, -10}, DurationS: 120},
				&journal.ProfileApplied{Offsets: []int{-20, -10, -10}, Condition: machine.Together},
				&journal.TrialStart{Trial: "trial"}}
			if tc.evidence != nil {
				payloads = append(payloads, tc.evidence)
			}
			crashAt := len(payloads)
			payloads = append(payloads,
				&journal.CrashDetected{PreviousBoot: "boot", InFlight: new(2), Condition: machine.Together},
				&journal.TrialEnd{Trial: "trial", Outcome: journal.OutcomeFailure, Signal: tc.signal, Interrupted: tc.evidence != nil},
				&journal.Failure{Trial: "trial", Signal: tc.signal, Attribution: journal.Unattributed, Condition: machine.Together, Regime: machine.R2, Profile: []int{-20, -10, -10}})
			events := dashboardEvents(payloads...)
			for i := crashAt; i < len(events); i++ {
				events[i].Boot = "next"
			}
			end := &events[crashAt+1]
			end.Cause = []int{2}
			if tc.evidence == nil {
				end.Cause = append(end.Cause, events[crashAt].Seq)
			} else {
				end.Cause = append(end.Cause, events[crashAt-1].Seq)
			}
			pending := Project(events[:crashAt+1])
			if diff := cmp.Diff([]string{
				"start: session started on 3 cores",
				"crash: with the offsets applied, rebooted",
			}, historySentences(pending)); diff != "" {
				t.Fatalf("a reboot must be visible before the trial end is appended (-want +got):\n%s", diff)
			}
			want := []string{"start: session started on 3 cores"}
			if tc.evidence != nil {
				want = append(want, "crash: with the offsets applied, rebooted")
			}
			want = append(want, tc.wantEnd)
			s := Project(events)
			if diff := cmp.Diff(want, historySentences(s)); diff != "" {
				t.Fatalf("only a crash outcome represents the reboot; durable failure evidence must coexist with it (-want +got):\n%s", diff)
			}
			index := 0
			if tc.evidence != nil {
				index = 1
			}
			if h := s.history[index]; h.reboot != events[crashAt].Seq || !h.at.Equal(events[crashAt].Time) {
				t.Fatalf("enriching reboot history must retain its source and original time: %+v", h)
			}
		})
	}
}

func TestProjectMultipleRebootsStayDistinct(t *testing.T) {
	t.Parallel()
	events := dashboardEvents(dashboardSession(),
		&journal.SMUIntent{Op: journal.SMUSetAll, Offset: -10},
		&journal.CrashDetected{PreviousBoot: "boot", InFlight: new(2), Condition: machine.Together},
		&journal.TrialIntent{Trial: "trial", Core: new(0), Condition: machine.Together, Phase: journal.PhaseChecking, Regime: machine.R2, Workload: "mprime-avx2-36k-248k", Profile: []int{-20, -10, -10}, DurationS: 120},
		&journal.ProfileApplied{Offsets: []int{-20, -10, -10}, Condition: machine.Together},
		&journal.TrialStart{Trial: "trial"},
		&journal.CrashDetected{PreviousBoot: "next", InFlight: new(4), Condition: machine.Together},
		&journal.TrialEnd{Trial: "trial", Outcome: journal.OutcomeFailure, Signal: machine.Crash})
	for i := 2; i < 6; i++ {
		events[i].Boot = "next"
	}
	for i := 6; i < len(events); i++ {
		events[i].Boot = "last"
	}
	events[7].Cause = []int{events[6].Seq}
	s := Project(events)
	if diff := cmp.Diff([]string{
		"start: session started on 3 cores",
		"crash: with the offsets applied, rebooted",
		"crash: R2 heavy vector on core 00 at -20 · rebooted · no core named",
	}, historySentences(s)); diff != "" {
		t.Fatalf("closing one crashed trial must not consume another boot's reboot (-want +got):\n%s", diff)
	}
	if s.crashes != 2 || s.history[1].reboot != events[2].Seq || s.history[0].reboot != events[6].Seq {
		t.Fatalf("distinct reboot identities lost: crashes=%d, history=%+v", s.crashes, s.history)
	}
}

func TestProjectRebootHistoryRequiresMatchingCause(t *testing.T) {
	t.Parallel()
	events := dashboardEvents(dashboardSession(),
		&journal.CrashDetected{PreviousBoot: "boot", Condition: machine.Together},
		&journal.TrialEnd{Trial: "unrelated", Outcome: journal.OutcomeFailure, Signal: machine.Crash})
	events[1].Boot, events[2].Boot = "next", "next"
	events[2].Cause = []int{events[0].Seq}
	if diff := cmp.Diff([]string{
		"start: session started on 3 cores",
		"crash: idle with the offsets applied, rebooted",
		"crash: trial unrelated · rebooted",
	}, historySentences(Project(events))); diff != "" {
		t.Fatalf("a crash trial end without the reboot in its causes must not hide that reboot (-want +got):\n%s", diff)
	}
}

func TestProjectFallbackHistory(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name     string
		result   string
		fallback bool
		want     []string
	}{
		{"unresolved fallback", "fallback", true, []string{
			"start: session started on 3 cores",
			"combo: C1 over 00 02 · hunt 3 unresolved",
			"hunt: #3 done · 00 02 unresolved",
		}},
		{"proven combination", "combination", false, []string{
			"start: session started on 3 cores",
			"combo: C1: 00 -20  02 -10",
			"hunt: #3 done · 00 02 kept together",
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			s := Project(dashboardEvents(dashboardSession(),
				&journal.Combination{Combination: 1, Hunt: 3, Members: []journal.CombinationMember{{Core: 0, Offset: -20}, {Core: 2, Offset: -10}}, Fallback: tc.fallback},
				&journal.HuntEnd{Hunt: 3, Result: tc.result, Cores: []int{0, 2}}))
			if diff := cmp.Diff(tc.want, historySentences(s)); diff != "" {
				t.Fatalf("history must distinguish a conservative fallback from an observed combination (-want +got):\n%s", diff)
			}
			if s.history[1].tone != comboTone || s.history[0].tone != warnTone {
				t.Fatalf("combination history severity changed: %+v", s.history)
			}
		})
	}
}

func TestProjectCycleHistory(t *testing.T) {
	t.Parallel()
	s := Project(dashboardEvents(dashboardSession(),
		&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R1}},
		&journal.CheckingCycle{Cycle: 1, Event: journal.CycleEnd, Passed: true, Missing: []string{"R2: 3 more steps"}},
		&journal.CheckingCycle{Cycle: 2, Event: journal.CycleStart, Steps: []machine.Regime{machine.R1}},
		&journal.CheckingCycle{Cycle: 2, Event: journal.CycleEnd, Passed: true, Full: true},
		&journal.CheckingCycle{Cycle: 3, Event: journal.CycleStart, Steps: []machine.Regime{machine.R1}},
		&journal.CheckingCycle{Cycle: 3, Event: journal.CycleEnd, Reason: "failure"}))
	if diff := cmp.Diff([]string{
		"start: session started on 3 cores",
		"cycle: cycle 1 started · 1 step",
		"cycle: cycle 1 passed but missing R2: 3 more steps",
		"cycle: cycle 2 started · 1 step",
		"cycle: cycle 2 passed, a full cycle of every kind of test",
		"cycle: cycle 3 started · 1 step",
		"cycle: cycle 3 ended early: failure",
	}, historySentences(s)); diff != "" {
		t.Fatalf("cycle history must call a cycle passed, never clean, which needs every core at its limit (-want +got):\n%s", diff)
	}
}

func TestProjectOrphanTrialIDIsEscaped(t *testing.T) {
	t.Parallel()
	id := "orphan\x1b]52;c;payload\a\n\r\t\x7f\u009b2J\u009dtitle\u2028\u2029\u202e\xff"
	s := Project(dashboardEvents(dashboardSession(),
		&journal.TrialEnd{Trial: id, Outcome: journal.OutcomeFailure, Signal: machine.ComputationError}))
	if diff := cmp.Diff([]string{
		"start: session started on 3 cores",
		`fail: trial orphan\x1b]52;c;payload\x07\n\r\t\x7f\u009b2J\u009dtitle\u2028\u2029\u202e\xff · wrong result`,
	}, historySentences(s)); diff != "" {
		t.Fatalf("orphan trial identifiers must not send controls to main history (-want +got):\n%s", diff)
	}
}

func TestProjectDeepeningHistorySeparatesDeeperAndYieldedMembers(t *testing.T) {
	t.Parallel()
	events := dashboardEvents(dashboardSession(),
		&journal.CorePhase{Core: 0, From: journal.PhaseSearch, To: journal.PhaseHasRoom, Offset: -20},
		&journal.CorePhase{Core: 1, From: journal.PhaseSearch, To: journal.PhaseAtLimit, Offset: -30},
		&journal.CorePhase{Core: 2, From: journal.PhaseSearch, To: journal.PhaseAtLimit, Offset: -10},
		&journal.DeepeningRound{Round: 2, Event: journal.CycleStart, Target: []int{-22, -29, -10}, Profile: []int{-21, -29, -10}, Cores: []int{0, 1}},
		&journal.TunerDecision{Core: 1, Phase: journal.PhaseDeepening, Decision: journal.Yield, FromOffset: -30, ToOffset: -29},
		&journal.TunerDecision{Core: 0, Phase: journal.PhaseDeepening, Decision: journal.Deepen, FromOffset: -20, ToOffset: -21})
	s := Project(events)
	if diff := cmp.Diff([]string{
		"start: session started on 3 cores",
		"limit: core 00 solo limit -20",
		"limit: core 01 solo limit -30",
		"limit: core 02 solo limit -10",
		"round: #2: core 00 goes deeper to -21, core 01 yields to -29",
		"room: core 01 -30 → -29 so others go deeper",
		"room: core 00 -20 → -21",
	}, historySentences(s)); diff != "" {
		t.Fatalf("round history must describe each member's direction (-want +got):\n%s", diff)
	}
}

func TestHistoryGroupsInferredAnswersWithoutHidingOutcomeChanges(t *testing.T) {
	t.Parallel()
	events := dashboardEvents(dashboardSession(),
		&journal.HuntStart{Hunt: 4, Candidates: []int{0, 1, 2}},
		&journal.HuntGroup{Hunt: 4, Group: 2, Cores: []int{0}, Inferred: "pass"},
		&journal.HuntGroup{Hunt: 4, Group: 3, Cores: []int{1}, Inferred: "pass"},
		&journal.HuntGroup{Hunt: 4, Group: 4, Cores: []int{0, 1}, Inferred: "failure"},
		&journal.HuntStart{Hunt: 5, Candidates: []int{0, 1}},
		&journal.HuntGroup{Hunt: 5, Group: 5, Cores: []int{1}, Inferred: "pass"})
	want := []string{
		"start: session started on 3 cores",
		"hunt: #4 started · part 1: 00 at failing offsets, 01 02 parked",
		"group: hunt 4 groups 2-3 · parts of 00 01 · all passed on existing evidence",
		"group: hunt 4 group 4 · 00 01 · failed on existing evidence",
		"hunt: #5 started · part 1: 01 at failing offsets, 00 parked",
		"group: hunt 5 group 5 · 01 · passed on existing evidence",
	}
	if diff := cmp.Diff(want, historySentences(Project(events))); diff != "" {
		t.Fatalf("inferred group answers (-want +got):\n%s", diff)
	}
}

func TestTallyNeverCountsPastTheRequirement(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		first, runs, of int
		want            string
	}{
		{1, 4, 4, "4 of 4"},
		{1, 3, 4, "trials 1-3 of 4"},
		{2, 1, 4, "trial 2 of 4"},
		{2, 4, 5, "trials 2-5 of 5"},
		{2, 5, 5, "5"},
		{1, 1, 1, "1"},
	} {
		if got := (entry{first: tc.first, runs: tc.runs, of: tc.of}).tally(); got != tc.want {
			t.Errorf("first %d, runs %d, of %d: %q, want %q", tc.first, tc.runs, tc.of, got, tc.want)
		}
	}
}

// unboundedHistory folds the fixture's descriptions without Project's retention step.
func unboundedHistory(events []journal.Event) []entry {
	p := historyFixtureProjector(events)
	for _, e := range events {
		if d, ok := e.Data.(*journal.HuntGroup); ok && d.Probe != nil {
			p.probes[d.Hunt] = true
		}
		if line, ok := p.describe(e); ok {
			p.s.history = foldEntry(p.s.history, line)
		}
	}
	return mergeProbePasses(p.s.history)
}

func historyFixtureProjector(events []journal.Event) projector {
	var st journal.State
	r := requirementRecorder{t: tuner.New(), intents: map[string]*journal.TrialIntent{}, failed: map[string]tuner.TrialRequirement{}, counts: map[string]trialCount{}, steps: map[string]int{}}
	journal.Replay(events, &st, &r, r.t)
	return projector{
		s: &Snapshot{carried: map[int]bool{}, shapes: map[int]huntShape{}}, st: &st,
		intents: r.intents, counts: r.counts, ends: map[string]*trialEnd{}, groupSignals: map[[2]int]machine.Signal{},
		huntStarts: map[int]huntStartView{}, groups: map[[2]int]*journal.HuntGroup{}, probes: map[int]bool{},
	}
}

func assertHistoryReference(t *testing.T, events []journal.Event) Snapshot {
	t.Helper()
	want := unboundedHistory(events)
	s := Project(events)
	if diff := cmp.Diff(len(want), s.historyDropped+len(s.history)); diff != "" {
		t.Fatalf("kept and dropped history must count folded display lines (-want +got):\n%s", diff)
	}
	if len(s.history) > historyLimit {
		t.Fatalf("visible history exceeds its retention limit: %d entries", len(s.history))
	}
	want = slices.Clone(want[len(want)-len(s.history):])
	slices.Reverse(want)
	if diff := cmp.Diff(want, s.history, cmp.AllowUnexported(entry{})); diff != "" {
		t.Fatalf("retention must keep the unbounded reference's newest folded lines (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(historySentences(Snapshot{history: want}), historySentences(s)); diff != "" {
		t.Fatalf("visible history sentences changed at retention (-want +got):\n%s", diff)
	}
	return s
}

func historyWarnings(count int) []journal.Payload {
	var payloads []journal.Payload
	for i := range count {
		payloads = append(payloads, &journal.SessionWarning{Operation: "projection", Error: fmt.Sprint(i)})
	}
	return payloads
}

func historyProbeStart(trials int) *journal.HuntStart {
	return &journal.HuntStart{Hunt: 1, Regime: machine.R2, Workload: "mprime-avx2-36k-248k", Cores: []int{0, 1}, Candidates: []int{0, 1}, Failing: []int{-20, -20, 0}, Parked: []int{-10, -10, 0}, Trials: trials, TrialS: 90, DurationS: 90}
}

func historyProbeGroup(group, trials int) []journal.Payload {
	core := (group - 1) % 2
	profile := []int{-19, -19, 0}
	held := journal.CombinationMember{Core: 1 - core, Offset: -19}
	payloads := []journal.Payload{
		&journal.HuntGroup{Hunt: 1, Group: group, Stage: "probe", Cores: []int{0, 1}, Set: []int{0, 1}, Profile: profile, Probe: &journal.CombinationMember{Core: core, Offset: -19}, Held: []journal.CombinationMember{held}, DurationS: 90},
	}
	for trial := range trials {
		id := fmt.Sprintf("probe-%d-%d", group, trial+1)
		payloads = append(payloads,
			&journal.TrialIntent{Trial: id, Condition: machine.Parked, Phase: journal.PhaseHunt, Regime: machine.R2, Workload: "mprime-avx2-36k-248k", Cores: []int{0, 1}, Profile: profile, DurationS: 90, Hunt: 1, Group: group},
			&journal.TrialEnd{Trial: id, Outcome: journal.OutcomePass, TctlMaxC: new(60 + group%20)})
	}
	return payloads
}

func TestProjectProbeHistoryBeforeRetention(t *testing.T) {
	t.Parallel()
	payloads := []journal.Payload{dashboardSession(), historyProbeStart(1)}
	payloads = append(payloads, historyProbeGroup(1, 1)...)
	payloads = append(payloads, historyProbeGroup(2, 1)...)
	payloads = append(payloads, &journal.HuntEnd{Hunt: 1, Result: "combination", Cores: []int{0, 1}})
	prefix := unboundedHistory(dashboardEvents(payloads...))
	slices.Reverse(prefix)
	if diff := cmp.Diff([]string{
		"start: session started on 3 cores",
		"hunt: #1 started · candidates 00 01",
		"pass: hunt 1 groups 1-2 · member probes · each 1 passed",
		"hunt: #1 done · 00 01 kept together, probed",
	}, historySentences(Snapshot{history: prefix})); diff != "" {
		t.Fatalf("probe fixture must fold into four display lines (-want +got):\n%s", diff)
	}
	payloads = append(payloads, historyWarnings(396)...)
	s := assertHistoryReference(t, dashboardEvents(payloads...))
	if s.historyDropped != 200 || len(s.history) != 200 {
		t.Fatalf("400 folded lines must keep 200 and omit 200: dropped=%d kept=%d", s.historyDropped, len(s.history))
	}
}

func TestProjectProbeHistoryAcrossRetentionBoundary(t *testing.T) {
	t.Parallel()
	payloads := append([]journal.Payload{dashboardSession()}, historyWarnings(198)...)
	payloads = append(payloads, historyProbeStart(1))
	payloads = append(payloads, historyProbeGroup(1, 1)...)
	payloads = append(payloads, historyProbeGroup(2, 1)...)
	payloads = append(payloads, historyWarnings(199)...)
	s := assertHistoryReference(t, dashboardEvents(payloads...))
	if got := s.history[len(s.history)-1].sentence(); got != "hunt 1 groups 1-2 · member probes · each 1 passed" {
		t.Fatalf("a probe run straddling the raw trim boundary lost its first group: %q", got)
	}
}

func TestProjectProbeHistoryLongRun(t *testing.T) {
	t.Parallel()
	payloads := []journal.Payload{dashboardSession(), historyProbeStart(1)}
	for group := 1; group <= 1205; group++ {
		payloads = append(payloads, historyProbeGroup(group, 1)...)
	}
	payloads = append(payloads, historyWarnings(150)...)
	events := dashboardEvents(payloads...)
	s := assertHistoryReference(t, events)
	if s.historyDropped != 0 || len(s.history) != 153 {
		t.Fatalf("a long contiguous probe run is one line, not a dropped journal prefix: dropped=%d kept=%d", s.historyDropped, len(s.history))
	}
	if got := s.history[150].sentence(); got != "hunt 1 groups 1-1205 · member probes · each 1 passed" {
		t.Fatalf("long probe summary changed: %q", got)
	}
	p := historyFixtureProjector(events)
	for _, e := range events {
		p.fold(e)
		if len(p.s.history) > 2*historyLimit {
			t.Fatalf("periodic compaction retained an unbounded probe history at event %d: %d entries", e.Seq, len(p.s.history))
		}
	}
}

func TestProjectProbeHistoryCompletesAfterRetention(t *testing.T) {
	t.Parallel()
	payloads := append([]journal.Payload{dashboardSession()}, historyWarnings(396)...)
	payloads = append(payloads, historyProbeStart(2))
	payloads = append(payloads, historyProbeGroup(1, 2)...)
	payloads = append(payloads, historyProbeGroup(2, 2)...)
	last := historyProbeGroup(3, 2)
	payloads = append(payloads, last[:3]...)
	pending := assertHistoryReference(t, dashboardEvents(payloads...))
	if pending.history[0].runs != 1 || pending.history[0].of != 2 || pending.history[1].lastGroup != 2 {
		t.Fatalf("an incomplete probe must remain separate from the completed prefix: %+v", pending.history[:2])
	}
	payloads = append(payloads, last[3:]...)
	complete := assertHistoryReference(t, dashboardEvents(payloads...))
	if got := complete.history[0].sentence(); got != "hunt 1 groups 1-3 · member probes · each 2 of 2 passed" {
		t.Fatalf("completing a retained probe did not finish the full run: %q", got)
	}
	if complete.historyDropped != pending.historyDropped || len(complete.history) != len(pending.history)-1 {
		t.Fatalf("completing the tail must merge retained lines, not adjust the omitted prefix: before=%d+%d after=%d+%d", pending.historyDropped, len(pending.history), complete.historyDropped, len(complete.history))
	}
}

func TestProjectProbeHistoryCompleteTailRemainsMutable(t *testing.T) {
	t.Parallel()
	payloads := append([]journal.Payload{dashboardSession()}, historyWarnings(397)...)
	payloads = append(payloads, historyProbeStart(1))
	payloads = append(payloads, historyProbeGroup(1, 1)...)
	last := historyProbeGroup(2, 2)
	payloads = append(payloads, last[:3]...)
	complete := assertHistoryReference(t, dashboardEvents(payloads...))
	if complete.historyDropped != 200 || len(complete.history) != 200 {
		t.Fatalf("a complete tail must be counted before choosing the retained window: dropped=%d kept=%d", complete.historyDropped, len(complete.history))
	}
	if got := complete.history[0].sentence(); got != "hunt 1 groups 1-2 · member probes · each 1 passed" {
		t.Fatalf("a complete tail must count as part of its preceding summary: %q", got)
	}
	payloads = append(payloads, last[3:]...)
	extra := assertHistoryReference(t, dashboardEvents(payloads...))
	if extra.history[0].firstGroup != 2 || extra.history[0].lastGroup != 2 || extra.history[0].runs != 2 || extra.history[1].lastGroup != 1 {
		t.Fatalf("a later trial of the same group must not change the preceding group's tally: %+v", extra.history[:2])
	}
}

func TestProjectRecordCrashHistoryShrinksAfterRetention(t *testing.T) {
	t.Parallel()
	payloads := append([]journal.Payload{dashboardSession()}, historyWarnings(398)...)
	for i := 1; i <= 2; i++ {
		id := fmt.Sprintf("record-%d", i)
		intent := len(payloads) + 1
		payloads = append(payloads,
			&journal.TrialIntent{Trial: id, Condition: machine.Together, Phase: journal.PhaseChecking, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", Cores: []int{0, 2}, Profile: []int{-20, -20, -20}, DurationS: 120, RecordOnly: true},
			&journal.CrashDetected{PreviousBoot: "boot", Condition: machine.Together, InFlight: new(intent)},
			&journal.TrialEnd{Trial: id, Outcome: journal.OutcomeFailure, Signal: machine.Crash})
	}
	events := dashboardEvents(payloads...)
	for i := 400; i < len(events); i++ {
		events[i].Boot = "next"
	}
	for i := 403; i < len(events); i++ {
		events[i].Boot = "last"
	}
	events[403].Data.(*journal.CrashDetected).PreviousBoot = "next"
	events[401].Cause = []int{events[400].Seq}
	events[404].Cause = []int{events[403].Seq}
	pending := assertHistoryReference(t, events[:404])
	complete := assertHistoryReference(t, events)
	if pending.historyDropped != 201 || len(pending.history) != 200 || complete.historyDropped != 201 || len(complete.history) != 199 {
		t.Fatalf("enriching and merging retained crashes must leave the omitted prefix alone: before=%d+%d after=%d+%d", pending.historyDropped, len(pending.history), complete.historyDropped, len(complete.history))
	}
	if got := complete.history[0].sentence(); got != "R7 all-core on 00 02 · 2 crashes · record only" {
		t.Fatalf("retained crash summarization changed: %q", got)
	}
}
