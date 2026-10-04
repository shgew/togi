package watch

import (
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

// Snapshot is everything one frame shows, projected from the journal. It holds facts and the tuner's own projections;
// every word the screen says about them is the renderer's.
type Snapshot struct {
	problem error
	session bool
	start   time.Time
	phase   journal.Phase

	failures    int        // failure decisions, skips of trials that already failed included
	crashes     int        // boots that ended without a clean shutdown
	lastFailure *time.Time // the last failure this session that changed a decision; skips and record-only results excluded
	cleanCycles int        // clean cycles counted for the current profile

	cores  []coreView // sorted by core ID
	combos []comboView

	trial    *trialView   // the trial in flight: its intent is recorded and it has not ended
	last     *trialEnd    // the last trial that ended
	outcomes []outcome    // what each outcome of the trial in flight leads to, as the tuner would decide it
	next     *tuner.Trial // between trials: the trial the tuner runs next, when it can say without a hardware read

	cycle   *cycleView
	hunt    *huntView
	turns   []turnView // the search's turns, in the order the tuner takes them; empty once every core has a solo limit
	deepen  *deepenView
	recover *recoveryView // a crash was detected on this boot and no trial has started since
	stopped *stopView
	deadEnd *deadEndView

	history []entry // what happened, newest first
	log     []entry // the journal's own lines, oldest first
}

type coreView struct {
	id, ccd int
	state   coreState
	queued  string // why the core waits for a new search, or empty

	applied int  // the last hardware readback, distinct from the saved profile even after restoration
	profile int  // the offset the tuner's profile holds for the core
	solo    *int // solo limit, once confirmed or carried
	pass    *int // the deepest offset that passed the search
	next    *int // the offset the search tries next
	fail    *int // failure point

	holder     *limit // what stops the core one count deeper than its profile offset, when anything does
	gaveBack   int    // the combination whose backoff moved the core shallower than its solo limit, or 0
	groupFails []int  // offsets at which a group trial of the running hunt failed with this core loaded
	returnsTo  *int   // the offset a parked core returns to when the hunt ends
	confirm    *confirmView

	loaded bool // carries load in the trial in flight
	judged bool // the trial in flight can name this core
}

type coreState int

const (
	coreWaiting coreState = iota // at 0, waiting for its first search turn
	coreSearch
	coreConfirm
	coreFound // solo limit confirmed, waiting at 0 until every core has one
	coreAtLimit
	coreHasRoom
	coreSuspect // a hunt candidate at its failing offset
	coreMember  // a member of a group the hunt keeps together, at its failing offset
	coreProbe   // the member a probe moves shallower
	coreParked  // held shallower by a hunt
)

// limit is what reaching one count deeper would hit: the floor at -50, the core's failure point or a combination.
type limit struct {
	floor       bool
	failure     bool
	combination int
}

// confirmView counts the passes of the candidate solo limit being confirmed, light then heavy, each needing needed in
// a row.
type confirmView struct {
	offset       int
	light, heavy int
	needed       int
}

// comboView is a recorded combination: offsets at which these cores failed together.
type comboView struct {
	id       int
	members  []journal.CombinationMember
	hunt     int
	probed   bool // member probes ran; otherwise no smaller group was shown to fail
	fallback bool
	holds    *journal.CombinationMember // the member it holds at its limit now, at its profile offset
	clear    []int                      // members shallower than their offset in the combination now
}

type trialView struct {
	id         string
	regime     machine.Regime
	workload   machine.Workload
	cores      []int // cores under load
	parked     []int // cores a hunt holds shallower in this trial
	idle       []int // cores neither loaded nor parked
	condition  machine.Condition
	phase      journal.Phase
	profile    []int
	started    time.Time
	hasStarted bool
	duration   time.Duration
	recordOnly bool
	rerun      bool
	retry      bool

	cycle, step, part, parts int // checking position; part and parts count from 1, zero outside R7 parts
	hunt, group              int
	huntPart, huntParts      int // the part of the hunt's current split, counting from 1
	probe                    *journal.CombinationMember
	core, offset             int // search and confirm: the core judged and its offset
	round                    int // deepening round

	passed, index, of int // trials of the current requirement: passed so far, the running one counting from 1, needed
}

type trialEnd struct {
	id       string
	at       time.Time
	regime   machine.Regime
	cores    []int
	outcome  journal.Outcome
	signal   machine.Signal
	core     *int
	duration time.Duration
	tctlMaxC *int
	voltageV *float64 // median voltage request
}

// premise is the outcome of the trial in flight an outcome line assumes.
type premise int

const (
	ifPasses       premise = iota // this trial passes
	ifAllPass                     // every remaining trial of the requirement passes
	ifFails                       // it fails; a trial with one core loaded always names it
	ifNamed                       // it fails and names a core
	ifUnnamed                     // it fails and names none
	ifInconclusive                // it ends without a verdict
)

// outcome is what the tuner records and runs after the trial in flight ends as premise assumes, computed by folding
// that ending into a copy of the tuner's state. needsRanking is set when the tuner would read the host's core ranking
// before deciding further.
type outcome struct {
	premise      premise
	passes       int               // with ifAllPass: how many trials pass
	decisions    []journal.Payload // the decisions it records, in order
	next         *tuner.Trial      // the trial it runs next, nil when it stops
	needsRanking bool
	needsMCE     bool
	needsHistory bool
}

type cycleView struct {
	number  int
	open    bool
	steps   []cycleStep
	current int  // index of the step running or next; len(steps) once every step is done
	paused  bool // a hunt or a rerun holds the cycle at current
}

type cycleStep struct {
	regime   machine.Regime
	workload machine.Workload
	parts    []cyclePart // one per load the step runs; per-core steps have one part per core
	done     bool
	hunts    []int // hunts this step started
}

type cyclePart struct {
	cores      []int // cores under load
	ccd        int   // -1 when the part loads every CCD
	full       bool  // every core of its CCD is loaded
	recordOnly bool
	short      int // trials of short length
	shortLen   time.Duration
	long       int // trials of the step's original length, when longer
	longLen    time.Duration
	passed     int
	failed     int
	running    bool
	done       bool
}

type huntView struct {
	id         int
	started    time.Time
	regime     machine.Regime
	cause      huntCause
	evidence   []string // what the journal recorded about the failure that falls short of naming a core
	candidates []int
	plan       []huntPart  // the current split: run, running and to come
	groups     []groupView // groups so far, oldest first
	probes     []probeView
	rerun      rerunPlan    // what reruns once the hunt resolves
	resume     *tuner.Trial // the trial the paused stage returns to, when known
}

type huntCause struct {
	at       time.Time
	trial    trialView
	signal   machine.Signal
	core     *int
	carried  bool // the failing profile was already recorded in a carried trial; no trial ran
	rerunOf  bool // the failure came in a rerun after a backoff
	trialNum int  // which trial of its requirement failed, counting from 1
}

type huntPart struct {
	failing []int // cores at their failing offsets
	parked  []int
	trials  int
	length  time.Duration
	group   int // the group this part ran as, 0 when to come
	outcome string
	running bool
}

type groupView struct {
	id       int
	cores    []int
	profile  []int
	stage    string
	probe    *journal.CombinationMember
	held     []journal.CombinationMember
	outcome  string
	passes   int
	needed   int
	inferred bool // answered by carried trials; no trial ran
}

type probeView struct {
	member   int
	now      int   // its offset in the probe running or next
	failedAt []int // offsets at which the group failed with it there
	passedAt []int // offsets at which the group passed with it there
	carried  []int // of passedAt, those answered by carried trials
	running  bool
	done     bool
}

type rerunPlan struct {
	regime   machine.Regime
	cores    []int
	short    int
	shortLen time.Duration
	long     int
	longLen  time.Duration
}

// turnView is one core's next search turn.
type turnView struct {
	core     int
	confirm  bool // confirming a candidate solo limit rather than stepping deeper
	regimes  []machine.Regime
	offset   int
	workload machine.Workload
	step     int // counts the next search step moves, 1 near a failure point
	running  bool
}

type deepenView struct {
	round   int
	room    []int // cores with room, in the order deepening tries them
	target  []int
	checks  []journal.CheckState
	waiting bool // deepening waits for a passed full cycle
}

type recoveryView struct {
	crashAt time.Time
	bootAt  time.Time
	trial   *trialView // the trial the crash ended, when one was in flight
}

type stopView struct {
	at     time.Time
	reason journal.ShutdownReason
	saved  bool // the applied offsets were restored; the core rows show the saved profile
}

type deadEndView struct {
	at        time.Time
	condition journal.DeadEndCondition
	detail    string
}

// entry is one line of what happened, in plain words or as the journal recorded it.
type entry struct {
	at                          time.Time
	tag                         string
	text                        string
	tone                        tone
	runs                        int
	each                        time.Duration
	peak                        *int
	reboot                      int
	hunt, firstGroup, lastGroup int
}

type tone int

const (
	plainTone tone = iota
	goodTone
	badTone
	warnTone
	huntTone
	comboTone
)
