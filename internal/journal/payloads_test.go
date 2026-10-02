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
		{"carry excludes marks", &SessionCarried{Sources: []CarriedSource{{Session: "old", Schema: 1, Ruleset: 3}}, Detail: "BIOS context changed", Carried: []CarriedCore{{Core: 7, Edge: new(-30)}}}, "carried 1 candidate edges from session old (schema 1, ruleset 3); failed marks stay behind: BIOS context changed"},
		{"preflight refusal", &PreflightCheck{Check: "driver", Detail: "unsupported CPU", OK: false}, "preflight driver: FAILED (unsupported CPU)"},
		{"read intent", &SMUIntent{Op: SMURead, Core: new(7)}, "SMU read core 07"},
		{"write all", &SMUWrite{Op: SMUSetAll, Offset: 0}, "SMU wrote all cores CO 0"},
		{"masked profile", &ProfileApplied{Condition: machine.Masked, Offsets: []int{-30, 0}}, "mask profile applied: [-30 0]"},
		{"resident profile", &ProfileApplied{Condition: machine.Resident, Offsets: []int{-30, -20}}, "profile applied: [-30 -20]"},
		{"equal profile", &ProfileChange{From: []int{-30, -20}, To: []int{-30, -20}}, "profile unchanged at [-30 -20]"},
		{"hunt trial", &TrialIntent{Trial: "0012", Cores: []int{3, 7}, Profile: []int{-30, -20}, Regime: machine.R7, Workload: "custom", DurationS: 120, Condition: machine.Masked, Phase: PhaseHunt, Hunt: 3, Mask: 4, Rerun: true}, "trial 0012 cores 03, 07 R7 custom 120s masked hunt 3 mask 4 rerun"},
		{"unseeded schedule", &TrialSignal{Trial: "0012", Schedule: "fixed periods"}, "trial 0012 load steps: fixed periods"},
		{"worker stall evidence", &TrialEnd{Trial: "0012", Outcome: OutcomeFailure, Signal: machine.Stall, DurationS: 41, StalledCore: new(7), WorkerStalledMS: new(int64(1200))}, "trial 0012 FAIL stall after 41s, core 07 worker CPU time stopped advancing at 1200ms after start (evidence only)"},
		{"known failure skip", &Failure{KnownFailure: 12, Reason: "known failure #12 answers this scheduled class"}, "known failure #12 answers this scheduled class"},
		{"attributed idle failure", &Failure{Signal: machine.Crash, Attribution: Attributed, Core: new(7), Offset: new(-30), Condition: machine.Resident, Regime: machine.R6, Profile: []int{0, -30}}, "core 07 failure at CO -30: crash with the resident profile applied and no trial in flight, the only nonzero core"},
		{"unattributed masked trial", &Failure{Signal: machine.Crash, Attribution: Unattributed, Condition: machine.Masked, Regime: machine.R7, Trial: "0012"}, "unattributed crash failure in masked R7 trial 0012: the mask fails, no evidence names a single core"},
		{"unattributed resident trial", &Failure{Signal: machine.Stall, Attribution: Unattributed, Condition: machine.Resident, Regime: machine.R7, Trial: "0012"}, "unattributed stall failure in resident R7 trial 0012: no evidence names a single core"},
		{"between trial MCE", &MCE{CPU: 7, Core: 7, Bank: 5, BankType: machine.LoadStore, BetweenTrials: true}, "uncorrected MCE on cpu 7 (core 07), bank 5 load_store (core-local) between trials (recorded only)"},
		{"typed reset reason", &CrashDetected{PreviousBoot: "previous-boot", ResetReason: machine.ResetPowerLoss, Inconclusive: true}, "crash: boot previous ended without a clean shutdown; nothing in flight; reset reason: power_loss; inconclusive: reset does not establish a tuning failure"},
		{"raw reset reason", &CrashDetected{PreviousBoot: "previous-boot", ResetReason: machine.ResetThermalTrip, ResetReasonRaw: "thermal trip", Inconclusive: true}, "crash: boot previous ended without a clean shutdown; nothing in flight; reset reason: thermal trip; inconclusive: reset does not establish a tuning failure"},
		{"refinement deepens", &TunerDecision{Core: 7, Phase: PhaseRefine, Decision: Deepen, FromOffset: -30, ToOffset: -31, Reason: "checks passed"}, "core 07 deepened at -30; next -31 (checks passed)"},
		{"search fails", &TunerDecision{Core: 7, Phase: PhaseSearch, Decision: Backoff, FromOffset: -30, ToOffset: -29, Reason: "failure"}, "core 07 failed at -30; next -29 (failure)"},
		{"defect diagnosis", &DefectFound{ID: 2, Title: "misattribution", PR: 12, Direction: "too_cautious", Cores: []int{3, 7}, Decisions: []int{12, 18}, Detail: "stale evidence"}, "defect 2 (misattribution, fixed in pull request #12): too_cautious decisions [12 18] affected cores 03, 07; stale evidence"},
		{"defect without detail", &DefectFound{ID: 2, Title: "misattribution", PR: 12, Direction: "too_cautious", Cores: []int{7}, Decisions: []int{12}}, "defect 2 (misattribution, fixed in pull request #12): too_cautious decisions [12] affected cores 07"},
		{"defect answer", &DefectAnswered{ID: 2, Cores: []int{3, 7}, Answer: "yes"}, "defect 2: answered yes to reset cores 03, 07"},
		{"clear saved entry", &DeadEnd{Condition: DeadEndFailureAtZero, Detail: "failed at zero", Action: ActionClearSavedEntry}, "dead end failure_at_zero: failed at zero; clearing GRUB's saved entry"},
		{"clear and reboot", &DeadEnd{Condition: DeadEndFailureAtZero, Detail: "failed at zero", Action: ActionClearSavedEntryAndReboot}, "dead end failure_at_zero: failed at zero; clearing GRUB's saved entry and rebooting"},
		{"already cleared", &BootSavedEntry{}, "GRUB saved entry was already unset"},
		{"select saved entry", &BootSavedEntry{Before: "normal", After: "tuning"}, "GRUB saved entry normal -> tuning"},
		{"dead end shutdown", &Shutdown{Reason: ShutdownDeadEnd}, "stopped at a dead end"},
		{"one rotation shutdown", &Shutdown{Reason: ShutdownRotations, Rotations: 1}, "every core is done and the profile passed the requested clean qualifying rotation; stopping"},
		{"ranking unavailable", &HostRanking{Detail: "firmware unavailable"}, "preferred-core ranking unavailable (firmware unavailable); core-id order"},
		{"hunt frozen anchor", &HuntStart{Hunt: 3, Trial: "0012", Regime: machine.R7, AnchorSeq: 18, Candidates: []int{3, 7}, Starts: 5, StartS: 120, Reason: "known failure #12"}, "hunt 3: unattributed failure in resident R7 trial 0012; anchor from rotation end #18; candidates 03, 07; masks of 5 × 120s; known failure #12"},
		{"edge mask", &HuntMask{Hunt: 3, Mask: 4, Cores: []int{3, 7}, Edge: &JointMember{Core: 7, Offset: -22}, Held: []JointMember{{Core: 3, Offset: -40}}, DurationS: 120, Reason: "probe edge"}, "hunt 3 mask 4: core 07 at -22 with core 03 -40, the rest at the anchor; starts of 120s; probe edge"},
		{"skipped mask", &HuntMask{Hunt: 3, Mask: 4, Cores: []int{7}, Skipped: true, Reason: "already checked"}, "hunt 3 mask 4: cores 07 skipped: already checked"},
		{"inferred mask", &HuntMask{Hunt: 3, Mask: 4, Cores: []int{7}, Inferred: "failure", Reason: "complement passed"}, "hunt 3 mask 4: cores 07 failure inferred: complement passed"},
		{"direct hunt attribution", &HuntEnd{Hunt: 3, Result: "direct", Cores: []int{7}, Masks: 1, Reason: "worker stalled"}, "hunt 3 ended after 1 mask: core 07 was attributed directly (worker stalled)"},
		{"hunt culprit reason", &HuntEnd{Hunt: 3, Result: "culprit", Cores: []int{7}, Masks: 4, Reason: "edge isolated"}, "hunt 3 found core 07 after 4 masks (edge isolated)"},
		{"hunt culprit", &HuntEnd{Hunt: 3, Result: "culprit", Cores: []int{7}, Masks: 4}, "hunt 3 found core 07 after 4 masks"},
		{"fallback mark", &MarkJoint{Mark: 2, Hunt: 3, Members: []JointMember{{Core: 3, Offset: -40}, {Core: 7, Offset: -30}}, Fallback: true}, "joint mark J2: core 03 -40 + core 07 -30, fallback over every candidate of hunt 3"},
		{"single core refinement", &RefineRound{Round: 2, Event: RotationStart, Cores: []int{7}, Target: []int{-30, -31}, Reason: "; edge check"}, "refine round 2 start: core 07 toward [-30 -31]; edge check"},
		{"passed refinement", &RefineRound{Round: 2, Event: RotationEnd, Passed: true, Reason: "; checks complete"}, "refine round 2 end: passed; checks complete"},
		{"failed refinement", &RefineRound{Round: 2, Event: RotationEnd, Reason: "failure"}, "refine round 2 end: failure"},
		{"detailed monotonicity", &TunerWarning{Warning: "monotonicity", Detail: "failure contradicts carried pass #12"}, "monotonicity: failure contradicts carried pass #12"},
		{"trial monotonicity", &TunerWarning{Warning: "monotonicity", Trial: "0012", Passes: []int{4, 8}}, "monotonicity: trial 0012 failed on a profile at least as shallow as 2 passes in its class"},
		{"refinement trial", &TrialIntent{Trial: "0013", Cores: []int{3, 7}, Profile: []int{-30, -31}, Regime: machine.R7, Workload: "custom", DurationS: 120, Condition: machine.Resident, Phase: PhaseRefine, Round: 2, Rerun: true}, "trial 0013 cores 03, 07 R7 custom 120s resident round 2 rerun"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if diff := cmp.Diff(tc.want, tc.payload.Message()); diff != "" {
				t.Fatalf("operator message (-want +got):\n%s", diff)
			}
		})
	}
}
