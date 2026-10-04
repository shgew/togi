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

// TrialRequirement counts valid outcomes of the requirement containing a trial.
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
	Done     bool
}

// CyclePart describes one load, including frozen record-only partial loads.
type CyclePart struct {
	Cores                      []int
	CCD                        int
	Full, RecordOnly           bool
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
	Parts      []HuntPart
	Groups     []HuntGroup
	Probes     []MemberProbe
	Rerun      RerunPlan
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
	// Profile is the profile the round moves to, halfway toward its target.
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
	// IfNamed assumes a failure names the branch's Core: a judged core away from 0 standing in for every such
	// core, or the first judged core at 0.
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
	// NextStep is the checking step Next loads, counting from 1; zero outside a checking cycle.
	NextStep int
}

// ForecastPlan holds conditional branches, or the next trial between trials.
type ForecastPlan struct {
	Branches     []ForecastBranch
	Next         *Trial
	NextStep     int
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
