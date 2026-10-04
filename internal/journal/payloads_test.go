package journal

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestOperatorRecoveryAndDecisionMessages(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		payload Payload
		want    string
	}{
		{"carry excludes failure points", &SessionCarried{Sources: []CarriedSource{{Session: "old", Schema: 1, Ruleset: 3}}, Detail: "BIOS context changed", Carried: []CarriedCore{{Core: 7, CandidateSoloLimit: new(-30)}}}, "carried 1 candidate solo limits from session old (schema 1, ruleset 3); failure points stay behind: BIOS context changed"},
		{"preflight refusal", &PreflightCheck{Check: "driver", Detail: "unsupported CPU", OK: false}, "preflight driver: FAILED (unsupported CPU)"},
		{"read error", &SMUError{Op: SMURead, Core: new(7), Error: "mailbox unavailable"}, "SMU read core 07 failed: mailbox unavailable"},
		{"write all", &SMUWrite{Op: SMUSetAll, Offset: 0}, "SMU wrote all cores CO 0"},
		{"parked profile", &ProfileApplied{Condition: machine.Parked, Offsets: []int{-30, 0}}, "group profile applied: [-30 0]"},
		{"current profile", &ProfileApplied{Condition: machine.Together, Offsets: []int{-30, -20}}, "profile applied: [-30 -20]"},
		{"equal profile", &ProfileChange{From: []int{-30, -20}, To: []int{-30, -20}}, "profile unchanged at [-30 -20]"},
		{"hunt trial", &TrialIntent{Trial: "0012", Cores: []int{3, 7}, Profile: []int{-30, -20}, Regime: machine.R7, Workload: "custom", DurationS: 120, Condition: machine.Parked, Phase: PhaseHunt, Hunt: 3, Group: 4, Rerun: true}, "trial 0012 cores 03, 07 R7 custom 120s parked hunt 3 group 4 rerun"},
		{"partial trial", &TrialIntent{Trial: "0012", Cores: []int{3}, Regime: machine.R7, Workload: "custom", DurationS: 120, Condition: machine.Together, Phase: PhaseChecking, Cycle: 2, Step: 4, RecordOnly: true}, "trial 0012 cores 03 R7 custom 120s together cycle 2 step 4 record-only"},
		{"checking partial snapshot", &CheckingStep{Cycle: 2, Step: 4, Profile: []int{-20, -50}, Partials: []CheckingPartial{{CCD: 1, Cores: []int{3}}, {CCD: 5, Cores: []int{}, Reason: "all CCD cores are at their limits"}}}, "checking cycle 2 R7 step 4 starts at current profile [-20 -50]"},
		{"carried partial", &TrialCarried{Source: FactSource{Session: "old", Trial: "0012", Build: Build{Version: "fixture"}}, Class: TrialClass{Regime: machine.R7, Workload: "custom", Cores: []int{3}, DurationS: 120}, Condition: machine.Together, Profile: []int{-20, -50}, Outcome: OutcomeFailure, Signal: machine.Crash, DurationS: 7, RecordOnly: true}, "carried failure of trial 0012 from session old (togi fixture): together R7 custom on cores 03 failed (crash) record-only after 7 s at [-20 -50] (intended 120 s, evidence epoch 0)"},
		{"carried top requester", &TrialCarried{Source: FactSource{Session: "old", Trial: "0012", Build: Build{Version: "fixture"}}, Class: TrialClass{Regime: machine.R7, Workload: "custom", Cores: []int{3, 7}, DurationS: 120}, Condition: machine.Together, Profile: []int{-20, -50}, Outcome: OutcomePass, DurationS: 120, TopRequesters: []int{7}}, "carried pass of trial 0012 from session old (togi fixture): together R7 custom on cores 03, 07 passed; top requester cores 07 after 120 s at [-20 -50] (intended 120 s, evidence epoch 0)"},
		{"unseeded schedule", &TrialSignal{Trial: "0012", Schedule: "fixed periods"}, "trial 0012 load steps: fixed periods"},
		{"worker stall evidence", &TrialEnd{Trial: "0012", Outcome: OutcomeFailure, Signal: machine.Stall, DurationS: 41, StalledCore: new(7), WorkerStalledMS: new(int64(1200))}, "trial 0012 FAIL stall after 41s, core 07 worker CPU time stopped advancing at 1200ms after start (evidence only)"},
		{"requested voltage pass", &TrialEnd{Trial: "0012", Outcome: OutcomePass, DurationS: 41, TctlMaxC: new(71), VoltageRequestMedianV: new(1.125), VoltageRequestMinV: new(1.0625)}, "trial 0012 PASS 41s | Tctl max 71°C | loaded voltage request median 1.125 V, min 1.062 V"},
		{"requested voltage failure", &TrialEnd{Trial: "0012", Outcome: OutcomeFailure, Signal: machine.Crash, VoltageRequestMedianV: new(1.125), VoltageRequestMinV: new(1.0)}, "trial 0012 FAIL crash, last evidence 0s after start | loaded voltage request median 1.125 V, min 1.000 V"},
		{"requested voltage inconclusive", &TrialEnd{Trial: "0012", Outcome: OutcomeInconclusive, Reason: "interrupted", VoltageRequestMedianV: new(1.125), VoltageRequestMinV: new(1.0)}, "trial 0012 INCONCLUSIVE after 0s: interrupted | loaded voltage request median 1.125 V, min 1.000 V"},
		{"top requester pass", &TrialEnd{Trial: "0012", Outcome: OutcomePass, DurationS: 41, VoltageRequestsV: map[int]float64{4: 1.096}, TopRequesters: []int{4}}, "trial 0012 PASS 41s | top requester 04 1.096 V"},
		{"tied requesters failure", &TrialEnd{Trial: "0012", Outcome: OutcomeFailure, Signal: machine.Crash, VoltageRequestsV: map[int]float64{4: 1.096, 7: 1.0955}, TopRequesters: []int{4, 7}}, "trial 0012 FAIL crash, last evidence 0s after start | top requester 04 1.096 V, 07 1.095 V"},
		{"top requester inconclusive", &TrialEnd{Trial: "0012", Outcome: OutcomeInconclusive, Reason: "interrupted", VoltageRequestsV: map[int]float64{4: 1.096}, TopRequesters: []int{4}}, "trial 0012 INCONCLUSIVE after 0s: interrupted | top requester 04 1.096 V"},
		{"known failure skip", &Failure{KnownFailure: 12, Reason: "known failure #12 answers this scheduled class"}, "known failure #12 answers this scheduled class"},
		{"attributed idle failure", &Failure{Signal: machine.Crash, Attribution: Attributed, Core: new(7), Offset: new(-30), Condition: machine.Together, Regime: machine.R6, Profile: []int{0, -30}}, "core 07 failure at CO -30: crash with the current profile applied and no trial in flight, the only nonzero core"},
		{"unattributed parked trial", &Failure{Signal: machine.Crash, Attribution: Unattributed, Condition: machine.Parked, Regime: machine.R7, Trial: "0012"}, "unattributed crash failure in parked R7 trial 0012: the group fails, no evidence names a single core"},
		{"unattributed together trial", &Failure{Signal: machine.Stall, Attribution: Unattributed, Condition: machine.Together, Regime: machine.R7, Trial: "0012"}, "unattributed stall failure in together R7 trial 0012: no evidence names a single core"},
		{"between trial MCE", &MCE{CPU: 7, Core: 7, Bank: 5, BankType: machine.LoadStore, BetweenTrials: true}, "uncorrected MCE on cpu 7 (core 07), bank 5 load_store (core-local) between trials (recorded only)"},
		{"typed reset reason", &CrashDetected{PreviousBoot: "previous-boot", ResetReason: machine.ResetPowerLoss, Inconclusive: true}, "crash: boot previous ended without a clean shutdown; nothing in flight; reset reason: power_loss; inconclusive: reset does not establish a tuning failure"},
		{"raw reset reason", &CrashDetected{PreviousBoot: "previous-boot", ResetReason: machine.ResetThermalTrip, ResetReasonRaw: "thermal trip", Inconclusive: true}, "crash: boot previous ended without a clean shutdown; nothing in flight; reset reason: thermal trip; inconclusive: reset does not establish a tuning failure"},
		{"deepening deepens", &TunerDecision{Core: 7, Phase: PhaseDeepening, Decision: Deepen, FromOffset: -30, ToOffset: -31, Reason: "checks passed"}, "core 07 deepened at -30; next -31 (checks passed)"},
		{"search fails", &TunerDecision{Core: 7, Phase: PhaseSearch, Decision: Backoff, FromOffset: -30, ToOffset: -29, Reason: "failure"}, "core 07 failed at -30; next -29 (failure)"},
		{"tolerated failure", &TunerDecision{Core: 7, Decision: Tolerate, FromOffset: -30, ToOffset: -30, Reason: "1 failure in 10 starts"}, "core 07 tolerates R7 failure at -30; offset unchanged (1 failure in 10 starts)"},
		{"chain offset fallback", &CheckingChain{Lap: 2, Step: 4, CCD: 1, Workload: "custom", Groups: [][]int{{3}, {7}}, Cores: []int{7}, Profile: []int{-20, -50}, Part: "partial 1"}, "checking lap 2 R7 step 4 CCD 1 custom: request groups [[3] [7]] from offset fallback (no request telemetry); next partial 1 loads cores 07 at profile [-20 -50]"},
		{"defect diagnosis", &DefectFound{ID: 2, Title: "misattribution", PR: 12, Direction: "too_cautious", Cores: []int{3, 7}, Decisions: []int{12, 18}, Detail: "stale evidence"}, "defect 2 (misattribution, fixed in pull request #12): too_cautious decisions [12 18] affected cores 03, 07; stale evidence"},
		{"defect without detail", &DefectFound{ID: 2, Title: "misattribution", PR: 12, Direction: "too_cautious", Cores: []int{7}, Decisions: []int{12}}, "defect 2 (misattribution, fixed in pull request #12): too_cautious decisions [12] affected cores 07"},
		{"defect answer", &DefectAnswered{ID: 2, Cores: []int{3, 7}, Answer: "yes"}, "defect 2: answered yes to reset cores 03, 07"},
		{"clear saved entry", &DeadEnd{Condition: DeadEndFailureAtZero, Detail: "failed at zero", Action: ActionClearSavedEntry}, "dead end failure_at_zero: failed at zero; clearing GRUB's saved entry"},
		{"clear and reboot", &DeadEnd{Condition: DeadEndFailureAtZero, Detail: "failed at zero", Action: ActionClearSavedEntryAndReboot}, "dead end failure_at_zero: failed at zero; clearing GRUB's saved entry and rebooting"},
		{"already cleared", &BootSavedEntry{}, "GRUB saved entry was already unset"},
		{"reason write failed", &BootSavedEntry{Before: "togi", ReasonError: "ESP is read-only"}, "GRUB saved entry togi cleared; the next boot selects the first menu entry; leave reason not saved: ESP is read-only"},
		{"previous leave reason", &BootLeaveReason{ReasonID: "previous-boot", RestartLimitCount: 3, Reason: "service restart limit exhausted; returning to the normal system"}, "previous tuning boot ended: service restart limit exhausted; returning to the normal system (consecutive restart-limit boots: 3)"},
		{"dead end shutdown", &Shutdown{Reason: ShutdownDeadEnd}, "stopped at a dead end"},
		{"one cycle shutdown", &Shutdown{Reason: ShutdownCycles, Cycles: 1}, "every core is at its limit and the profile passed the requested clean cycle; stopping"},
		{"ranking unavailable", &HostRanking{Detail: "firmware unavailable"}, "preferred-core ranking unavailable (firmware unavailable); core-id order"},
		{"hunt frozen parked", &HuntStart{Hunt: 3, Trial: "0012", Regime: machine.R7, ParkedSeq: 18, Candidates: []int{3, 7}, Trials: 5, TrialS: 120, Reason: "known failure #12"}, "hunt 3: unattributed failure in together R7 trial 0012; parked offsets from cycle end #18; candidates 03, 07; groups of 5 × 120s; known failure #12"},
		{"solo limit group", &HuntGroup{Hunt: 3, Group: 4, Cores: []int{3, 7}, Probe: &CombinationMember{Core: 7, Offset: -22}, Held: []CombinationMember{{Core: 3, Offset: -40}}, DurationS: 120, Reason: "member probe"}, "hunt 3 group 4: core 07 at -22 with core 03 -40, the rest parked; trials of 120s; member probe"},
		{"skipped group", &HuntGroup{Hunt: 3, Group: 4, Cores: []int{7}, Skipped: true, Reason: "already checked"}, "hunt 3 group 4: cores 07 skipped: already checked"},
		{"inferred group", &HuntGroup{Hunt: 3, Group: 4, Cores: []int{7}, Inferred: "failure", Reason: "complement passed"}, "hunt 3 group 4: cores 07 failure inferred: complement passed"},
		{"direct hunt attribution", &HuntEnd{Hunt: 3, Result: "direct", Cores: []int{7}, Groups: 1, Reason: "worker stalled"}, "hunt 3 ended after 1 group: core 07 was attributed directly (worker stalled)"},
		{"hunt culprit reason", &HuntEnd{Hunt: 3, Result: "culprit", Cores: []int{7}, Groups: 4, Reason: "solo limit alone"}, "hunt 3 found core 07 after 4 groups (solo limit alone)"},
		{"fallback combination", &Combination{Combination: 2, Hunt: 3, Members: []CombinationMember{{Core: 3, Offset: -40}, {Core: 7, Offset: -30}}, Fallback: true}, "combination C2: core 03 -40 + core 07 -30, fallback over every candidate of hunt 3"},
		{"single core deepening", &DeepeningRound{Round: 2, Event: CycleStart, Cores: []int{7}, Target: []int{-30, -31}, Reason: "; solo limit check"}, "deepening round 2 start: core 07 toward [-30 -31]; solo limit check"},
		{"passed deepening", &DeepeningRound{Round: 2, Event: CycleEnd, Passed: true, Reason: "; checks complete"}, "deepening round 2 end: passed; checks complete"},
		{"failed deepening", &DeepeningRound{Round: 2, Event: CycleEnd, Reason: "failure"}, "deepening round 2 end: failure"},
		{"detailed monotonicity", &TunerWarning{Warning: "monotonicity", Detail: "failure contradicts carried pass #12"}, "monotonicity: failure contradicts carried pass #12"},
		{"trial monotonicity", &TunerWarning{Warning: "monotonicity", Trial: "0012", Passes: []int{4, 8}}, "monotonicity: trial 0012 failed on a profile at least as shallow as 2 passes in its class"},
		{"deepening trial", &TrialIntent{Trial: "0013", Cores: []int{3, 7}, Profile: []int{-30, -31}, Regime: machine.R7, Workload: "custom", DurationS: 120, Condition: machine.Together, Phase: PhaseDeepening, Round: 2, Rerun: true}, "trial 0013 cores 03, 07 R7 custom 120s together round 2 rerun"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if diff := cmp.Diff(tc.want, tc.payload.Message()); diff != "" {
				t.Fatalf("operator message (-want +got):\n%s", diff)
			}
		})
	}
}
