package watch

import (
	"testing"

	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func historySentences(s Snapshot) []string {
	var lines []string
	for _, h := range s.history {
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
	if s.crashes != 1 || s.history[1].reboot != events[3].Seq || s.history[1].tone != badTone {
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
		{"ordinary crash", machine.Crash, nil, "crash: heavy vector load on core 00 at -20, rebooted"},
		{"retained computation failure", machine.ComputationError, &journal.TrialProgress{Trial: "trial", Signal: machine.ComputationError, Core: new(0)}, "fail: heavy vector load on core 00 at -20, wrong result"},
		{"retained corrected MCE", machine.CorrectedMCE, &journal.MCE{Trial: "trial", Core: 0, Corrected: true, FromBoot: "boot"}, "fail: heavy vector load on core 00 at -20, corrected hardware error"},
		{"retained uncorrected MCE", machine.UncorrectedMCE, &journal.MCE{Trial: "trial", Core: 0, FromBoot: "boot"}, "fail: heavy vector load on core 00 at -20, uncorrected hardware error"},
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
			if h := s.history[1]; h.reboot != events[crashAt].Seq || !h.at.Equal(events[crashAt].Time) {
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
		"crash: heavy vector load on core 00 at -20, rebooted",
	}, historySentences(s)); diff != "" {
		t.Fatalf("closing one crashed trial must not consume another boot's reboot (-want +got):\n%s", diff)
	}
	if s.crashes != 2 || s.history[1].reboot != events[2].Seq || s.history[2].reboot != events[6].Seq {
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
		"crash: trial unrelated, rebooted",
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
			"combo: core 00 at -20 and core 02 at -10 kept as an unresolved, conservative limit",
			"hunt: #3 unresolved: conservative limit over cores 00, 02",
		}},
		{"proven combination", "combination", false, []string{
			"start: session started on 3 cores",
			"combo: core 00 at -20 and core 02 at -10 fail together",
			"hunt: #3 found a combination",
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
			if s.history[1].tone != warnTone || s.history[2].tone != warnTone {
				t.Fatalf("combination history severity changed: %+v", s.history)
			}
		})
	}
}

func TestProjectLapHistory(t *testing.T) {
	t.Parallel()
	s := Project(dashboardEvents(dashboardSession(),
		&journal.CheckingLap{Lap: 1, Event: journal.LapStart, Steps: []machine.Regime{machine.R1}},
		&journal.CheckingLap{Lap: 1, Event: journal.LapEnd, Passed: true, Missing: []string{"R2: 3 more steps"}},
		&journal.CheckingLap{Lap: 2, Event: journal.LapStart, Steps: []machine.Regime{machine.R1}},
		&journal.CheckingLap{Lap: 2, Event: journal.LapEnd, Passed: true, Full: true},
		&journal.CheckingLap{Lap: 3, Event: journal.LapStart, Steps: []machine.Regime{machine.R1}},
		&journal.CheckingLap{Lap: 3, Event: journal.LapEnd, Reason: "failure"}))
	if diff := cmp.Diff([]string{
		"start: session started on 3 cores",
		"lap: #1 started, 1 steps",
		"lap: #1 passed but missing R2: 3 more steps",
		"lap: #2 started, 1 steps",
		"lap: #2 passed, a full lap of every kind of test",
		"lap: #3 started, 1 steps",
		"lap: #3 ended early: failure",
	}, historySentences(s)); diff != "" {
		t.Fatalf("lap history must call a lap passed, never clean, which needs every core at its limit (-want +got):\n%s", diff)
	}
}

func TestProjectOrphanTrialIDIsEscaped(t *testing.T) {
	t.Parallel()
	id := "orphan\x1b]52;c;payload\a\n\r\t\x7f\u009b2J\u009dtitle\u2028\u2029\u202e\xff"
	s := Project(dashboardEvents(dashboardSession(),
		&journal.TrialEnd{Trial: id, Outcome: journal.OutcomeFailure, Signal: machine.ComputationError}))
	if diff := cmp.Diff([]string{
		"start: session started on 3 cores",
		`fail: trial orphan\x1b]52;c;payload\x07\n\r\t\x7f\u009b2J\u009dtitle\u2028\u2029\u202e\xff, wrong result`,
	}, historySentences(s)); diff != "" {
		t.Fatalf("orphan trial identifiers must not send controls to main history (-want +got):\n%s", diff)
	}
}
