package tuner

import (
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

// Limit identifies the constraint reached one count below a core's profile offset.
type Limit struct {
	Floor, Failure bool
	Combination    int
}

// TrialRequirement counts passing evidence; Trial is Passed+1, never advanced by failures.
type TrialRequirement struct{ Passed, Failed, Trial, Needed int }

// CyclePlan describes checking's recorded schedule and its current obligations.
type CyclePlan struct {
	Number  int
	Open    bool
	Steps   []CycleStep
	Current int
	Paused  bool
}

// CycleStep describes one workload occurrence in the checking schedule.
type CycleStep struct {
	Regime   machine.Regime
	Workload machine.Workload
	Parts    []CyclePart
	// ChainsComplete means every R7 CCD chain has a valid recorded ending; true outside R7.
	ChainsComplete bool
	Done           bool
}

// CyclePart describes a required load. R7 partials appear only once their chain is derived.
type CyclePart struct {
	Cores                      []int
	CCD                        int
	Full, Partial              bool
	Short, ShortS, Long, LongS int
	Passed, Failed             int
	Running, Done              bool
}

// HuntPlan describes a hunt's split, recorded groups and member probes.
type HuntPlan struct {
	Number     int
	Regime     machine.Regime
	FailureSeq int
	Trial      string
	Candidates []int
	// Cut numbers the split Parts belongs to, counting from 1 across the hunt: a failed part is cut finer into a new
	// split whose parts count again from 1. Zero when the plan is not a split.
	Cut    int
	Parts  []HuntPart
	Groups []HuntGroup
	Probes []MemberProbe
	Rerun  RerunPlan
}

// HuntPart describes a group within the current split.
type HuntPart struct {
	Failing, Parked          []int
	Trials, DurationS, Group int
	Outcome                  string
	Running                  bool
}

// HuntGroup describes a recorded group and the evidence answering it.
type HuntGroup struct {
	Number           int
	Cores, Profile   []int
	Stage            string
	Probe            *journal.CombinationMember
	Held             []journal.CombinationMember
	Outcome          string
	Passed, Needed   int
	Carried, Running bool
}

// MemberProbe describes a member's failed and passed offset ladder.
type MemberProbe struct {
	Member, Offset              int
	FailedAt, PassedAt, Carried []int
	Running, Done               bool
}

// SearchTurn describes a core's next search step or candidate confirmation.
type SearchTurn struct {
	Core                 int
	Confirm              bool
	Regimes              []machine.Regime
	Offset               int
	Workload             machine.Workload
	Step                 int
	Light, Heavy, Needed int
	Running              bool
}

// DeepeningPlan describes eligible cores in move order and the full-cycle prerequisite.
type DeepeningPlan struct {
	Round int
	Room  []int
	// Profile is the profile the round moves to, one count toward each moved core's solo limit.
	Profile []int
	Checks  []journal.CheckState
	Waiting bool
}

// Premise identifies the hypothetical ending used by a forecast branch.
type Premise string

const (
	// IfPass assumes the trial in flight passes.
	IfPass Premise = "pass"
	// IfAllPass assumes every remaining trial of its requirement passes; it is omitted when another requirement's
	// trial would run before them.
	IfAllPass Premise = "all_pass"
	// IfNamed assumes a failure names Core, selected to represent distinct offset/request-order outcomes.
	IfNamed Premise = "named"
	// IfUnnamed assumes a failure names no core.
	IfUnnamed Premise = "unnamed"
	// IfInconclusive assumes an ending without stability evidence.
	IfInconclusive Premise = "inconclusive"
)

// ForecastBranch contains the tuner's decisions and first subsequent trial.
type ForecastBranch struct {
	Premise      Premise
	Passes       int
	Core         *int
	Decisions    []journal.Payload
	Next         *Trial
	NeedsRanking bool
	// NeedsHistory marks a missing checking profile, a recurring hunt plan without new evidence, or decisions
	// that do not settle on a next trial.
	NeedsHistory bool
	// WithoutTelemetry marks R7 outcomes conditional on no new request or clock measurements in trial.end.
	// Recorded measurements, or offset order when absent, supply the tuner's fallback.
	WithoutTelemetry bool
	// OffsetOrder marks a WithoutTelemetry branch whose decisions take some loaded core's request from its offset
	// because no measurement covers it: a failure's backoff, or a partial chain step or ending derived without telemetry.
	OffsetOrder bool
	// AtZero marks a named branch whose core is at 0 in the trial's profile. TopRequester marks a named multi-core R7
	// branch whose core is among the load's top requesters in the order the forecast used.
	AtZero, TopRequester bool
	// NextStep is the checking step Next loads, counting from 1; zero outside a checking cycle.
	NextStep int
}

// ForecastPlan holds conditional branches, or the next trial between trials.
type ForecastPlan struct {
	Branches []ForecastBranch
	Next     *Trial
	NextStep int
	// Resume is the checking step, counting from 1, that the cycle on display runs next once its reruns pass, after the
	// decisions the tuner takes without a trial, such as R7 chain endings after a profile change; zero when the next
	// trial is not a trial of that cycle: not a cycle trial, or the first trial of the next cycle because passing the
	// reruns completes this one.
	Resume       int
	Decisions    []journal.Payload
	NeedsRanking bool
	NeedsHistory bool
}

// RerunPlan describes the first pending post-backoff requirement.
type RerunPlan struct {
	Regime                     machine.Regime
	Cores                      []int
	Short, ShortS, Long, LongS int
}
