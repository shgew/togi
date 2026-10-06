package journal

import (
	"fmt"
	"slices"
	"strconv"
	"strings"

	"github.com/shgew/togi/internal/machine"
)

type Phase string

const (
	PhaseSearch    Phase = "search"
	PhaseHasRoom   Phase = "has_room"
	PhaseAtLimit   Phase = "at_limit"
	PhaseChecking  Phase = "checking"
	PhaseHunt      Phase = "hunt"
	PhaseDeepening Phase = "deepening"
)

// Word is the state word the dashboard and togi status show for a core in phase p.
// Activity phases and unknown values have no core word and come back as their journal value.
func (p Phase) Word() string {
	switch p {
	case PhaseSearch:
		return "SEARCH"
	case PhaseHasRoom:
		return "HAS ROOM"
	case PhaseAtLimit:
		return "AT LIMIT"
	case PhaseChecking, PhaseHunt, PhaseDeepening:
	}
	return string(p)
}

type Decision string

const (
	StepDeeper     Decision = "step_deeper"
	Backoff        Decision = "backoff"
	CheckSoloLimit Decision = "check_solo_limit"
	Deepen         Decision = "deepen"
	Yield          Decision = "yield"
)

type Outcome string

const (
	OutcomePass         Outcome = "pass"
	OutcomeFailure      Outcome = "failure"
	OutcomeInconclusive Outcome = "inconclusive"
)

type DeadEndCondition string

const (
	DeadEndFailureAtZero DeadEndCondition = "failure_at_zero"
	DeadEndSMU           DeadEndCondition = "smu"
	DeadEndNoEvidence    DeadEndCondition = "no_evidence"
	DeadEndBootLoop      DeadEndCondition = "boot_loop"
	DeadEndContainment   DeadEndCondition = "containment"
	DeadEndPreflight     DeadEndCondition = "preflight"
	DeadEndDefect        DeadEndCondition = "defect"
	DeadEndThermalTrip   DeadEndCondition = "thermal_trip"
)

type Notice string

const NoticeNonzeroBaseline Notice = "nonzero_baseline"

type SMUOp string

const (
	SMUSet    SMUOp = "set"
	SMUSetAll SMUOp = "set_all"
	SMURead   SMUOp = "read"
)

type Attribution string

const (
	Attributed   Attribution = "attributed"
	Unattributed Attribution = "unattributed"
)

type CycleEvent string

const (
	CycleStart CycleEvent = "start"
	CycleEnd   CycleEvent = "end"
)

type ShutdownReason string

const (
	ShutdownSignal  ShutdownReason = "signal"
	ShutdownDeadEnd ShutdownReason = "dead_end"
	ShutdownCycles  ShutdownReason = "cycles"
	ShutdownCommand ShutdownReason = "command"
)

func coreID(c int) string { return fmt.Sprintf("%02d", c) }

func coreList(cores []int) string {
	ids := make([]string, len(cores))
	for i, c := range cores {
		ids[i] = coreID(c)
	}
	return strings.Join(ids, ", ")
}

func cpuList(cpus []int) string {
	ids := make([]string, len(cpus))
	for i, c := range cpus {
		ids[i] = strconv.Itoa(c)
	}
	return strings.Join(ids, ",")
}

func shortBoot(b string) string { return b[:min(8, len(b))] }

type SessionWarning struct {
	Operation string `json:"operation"`
	Trial     string `json:"trial,omitempty"`
	Error     string `json:"error"`
}

func (*SessionWarning) Kind() Kind { return KindSessionWarning }
func (p *SessionWarning) Message() string {
	return fmt.Sprintf("%s: %s; continuing the session", p.Operation, p.Error)
}

type SessionStart struct {
	Build
	Session  string             `json:"session"`
	Cores    []machine.CoreInfo `json:"cores"`
	Evidence int                `json:"evidence,omitempty"`
}

func (*SessionStart) Kind() Kind { return KindSessionStart }
func (p *SessionStart) Message() string {
	return fmt.Sprintf("session %s started by %s (schema %d, ruleset %d, fixes %d, %d cores)", p.Session, p.name(), p.Schema, p.Ruleset, p.Fixes, len(p.Cores))
}

func (p *SessionStart) Epoch() int {
	return evidenceEpoch(p.Ruleset, p.Evidence)
}

type SessionContext struct {
	machine.BIOSContext
}

func (*SessionContext) Kind() Kind { return KindSessionContext }
func (p *SessionContext) Message() string {
	return fmt.Sprintf("BIOS %s on %s, %s, microcode %s, boost limit %d MHz", p.BIOSVersion, p.Board, p.CPUModel, p.Microcode, p.BoostLimitMHz)
}

type SessionBaseline struct {
	Offsets []int `json:"offsets"`
}

func (*SessionBaseline) Kind() Kind { return KindSessionBaseline }
func (p *SessionBaseline) Message() string {
	return fmt.Sprintf("baseline profile %v", p.Offsets)
}

type SessionNotice struct {
	Notice Notice `json:"notice"`
	Cores  []int  `json:"cores,omitempty"`
}

func (*SessionNotice) Kind() Kind { return KindSessionNotice }
func (p *SessionNotice) Message() string {
	if p.Notice == NoticeNonzeroBaseline {
		return fmt.Sprintf("baseline has nonzero offsets on cores %s; BIOS CO 0 is recommended for tuning; starting from the baseline", coreList(p.Cores))
	}
	return fmt.Sprintf("notice %s", p.Notice)
}

type SessionArchived struct {
	Session string `json:"session"`
	Path    string `json:"path"`
}

func (*SessionArchived) Kind() Kind { return KindSessionArchived }
func (p *SessionArchived) Message() string {
	return fmt.Sprintf("session %s archived to %s", p.Session, p.Path)
}

type SessionCarried struct {
	Sources       []CarriedSource `json:"sources"`
	FailurePoints bool            `json:"failure_points"`
	Detail        string          `json:"detail,omitempty"`
	Carried       []CarriedCore   `json:"carried,omitempty"`
}

type CarriedSource struct {
	Session string `json:"session"`
	Path    string `json:"path"`
	Schema  int    `json:"schema"`
	Ruleset int    `json:"ruleset"`
}

type CarriedCore struct {
	Core                      int            `json:"core"`
	CandidateSoloLimit        *int           `json:"candidate_solo_limit,omitempty"`
	CandidateSoloLimitSession string         `json:"candidate_solo_limit_session,omitempty"`
	CandidateSoloLimitSeq     int            `json:"candidate_solo_limit_seq,omitempty"`
	FailurePoint              *int           `json:"failure_point,omitempty"`
	FailurePointSession       string         `json:"failure_point_session,omitempty"`
	FailurePointSeq           int            `json:"failure_point_seq,omitempty"`
	FailurePointSignal        machine.Signal `json:"failure_point_signal,omitempty"`
}

func (*SessionCarried) Kind() Kind { return KindSessionCarried }
func (p *SessionCarried) Message() string {
	sources := make([]string, len(p.Sources))
	for i, s := range p.Sources {
		sources[i] = fmt.Sprintf("session %s (schema %d, ruleset %d)", s.Session, s.Schema, s.Ruleset)
	}
	from := strings.Join(sources, ", ")
	var candidateSoloLimits, failurePoints int
	for _, c := range p.Carried {
		if c.CandidateSoloLimit != nil {
			candidateSoloLimits++
		}
		if c.FailurePoint != nil {
			failurePoints++
		}
	}
	if p.FailurePoints {
		return fmt.Sprintf("carried %d candidate solo limits and %d failure points from %s", candidateSoloLimits, failurePoints, from)
	}
	return fmt.Sprintf("carried %d candidate solo limits from %s; failure points stay behind: %s", candidateSoloLimits, from, p.Detail)
}

type KernelBoundary struct {
	KernelCursor string `json:"kernel_cursor,omitempty"`
	KernelError  string `json:"kernel_error,omitempty"`
}

type ConfigLoaded struct {
	Build
	KernelBoundary
	Path   string         `json:"path"`
	File   bool           `json:"file"`
	Config ConfigSnapshot `json:"config"`
}

func (*ConfigLoaded) Kind() Kind { return KindConfigLoaded }
func (p *ConfigLoaded) Message() string {
	if p.File {
		return fmt.Sprintf("config loaded from %s by %s (schema %d, ruleset %d, fixes %d)", p.Path, p.name(), p.Schema, p.Ruleset, p.Fixes)
	}
	return fmt.Sprintf("no config at %s; using defaults with %s (schema %d, ruleset %d, fixes %d)", p.Path, p.name(), p.Schema, p.Ruleset, p.Fixes)
}

type PreflightCheck struct {
	Check  string `json:"check"`
	Detail string `json:"detail"`
	OK     bool   `json:"ok"`
}

func (*PreflightCheck) Kind() Kind { return KindPreflightCheck }
func (p *PreflightCheck) Message() string {
	if p.OK {
		return fmt.Sprintf("preflight %s: ok (%s)", p.Check, p.Detail)
	}
	return fmt.Sprintf("preflight %s: FAILED (%s)", p.Check, p.Detail)
}

type SMUIntent struct {
	Op     SMUOp `json:"op"`
	Core   *int  `json:"core,omitempty"`
	Offset int   `json:"offset"`
}

func (*SMUIntent) Kind() Kind        { return KindSMUIntent }
func (p *SMUIntent) Message() string { return "SMU " + smuCommand(p.Op, p.Core, p.Offset) }

func smuCommand(op SMUOp, core *int, offset int) string {
	switch op {
	case SMUSet:
		if core != nil {
			return fmt.Sprintf("set core %s to CO %d", coreID(*core), offset)
		}
	case SMUSetAll:
		return fmt.Sprintf("set all cores to CO %d", offset)
	case SMURead:
		if core != nil {
			return fmt.Sprintf("read core %s", coreID(*core))
		}
	}
	return fmt.Sprintf("%s to CO %d", op, offset)
}

type SMUWrite struct {
	Op     SMUOp `json:"op"`
	Core   *int  `json:"core,omitempty"`
	Offset int   `json:"offset"`
}

func (*SMUWrite) Kind() Kind { return KindSMUWrite }
func (p *SMUWrite) Message() string {
	switch p.Op {
	case SMUSet:
		if p.Core != nil {
			return fmt.Sprintf("SMU wrote core %s CO %d", coreID(*p.Core), p.Offset)
		}
	case SMUSetAll:
		return fmt.Sprintf("SMU wrote all cores CO %d", p.Offset)
	case SMURead:
	}
	return fmt.Sprintf("SMU wrote %s CO %d", p.Op, p.Offset)
}

type SMUReadback struct {
	Core     int  `json:"core"`
	Offset   int  `json:"offset"`
	Expected *int `json:"expected,omitempty"`
}

func (*SMUReadback) Kind() Kind { return KindSMUReadback }
func (p *SMUReadback) Message() string {
	if p.Expected != nil && *p.Expected != p.Offset {
		return fmt.Sprintf("SMU core %s reads CO %d, expected %d", coreID(p.Core), p.Offset, *p.Expected)
	}
	return fmt.Sprintf("SMU core %s reads CO %d", coreID(p.Core), p.Offset)
}

type SMUError struct {
	Op     SMUOp  `json:"op"`
	Core   *int   `json:"core,omitempty"`
	Offset int    `json:"offset"`
	Error  string `json:"error"`
}

func (*SMUError) Kind() Kind { return KindSMUError }
func (p *SMUError) Message() string {
	return fmt.Sprintf("SMU %s failed: %s", smuCommand(p.Op, p.Core, p.Offset), p.Error)
}

type ProfileApplied struct {
	Offsets   []int             `json:"offsets"`
	Condition machine.Condition `json:"condition"`
}

func (*ProfileApplied) Kind() Kind { return KindProfileApplied }
func (p *ProfileApplied) Message() string {
	switch p.Condition {
	case machine.Alone:
		return "profile applied for alone trials: every core at CO 0"
	case machine.Parked:
		return fmt.Sprintf("group profile applied: %v", p.Offsets)
	case machine.Together:
	}
	return fmt.Sprintf("profile applied: %v", p.Offsets)
}

type ProfileChange struct {
	From []int `json:"from"`
	To   []int `json:"to"`
}

func (*ProfileChange) Kind() Kind { return KindProfileChange }
func (p *ProfileChange) Message() string {
	switch {
	case p.From == nil:
		return fmt.Sprintf("profile for checking: %v", p.To)
	case slices.Equal(p.From, p.To):
		return fmt.Sprintf("profile unchanged at %v", p.To)
	}
	return fmt.Sprintf("profile changed %v -> %v", p.From, p.To)
}

type ProfileRestored struct {
	Offsets []int `json:"offsets"`
}

func (*ProfileRestored) Kind() Kind { return KindProfileRestored }
func (p *ProfileRestored) Message() string {
	return fmt.Sprintf("offsets restored before stopping: %v (each core's baseline, or its current offset where that is shallower)", p.Offsets)
}

type TrialIntent struct {
	Trial string `json:"trial"`
	KernelBoundary
	Profile    []int             `json:"profile"`
	Core       *int              `json:"core,omitempty"`
	Cores      []int             `json:"cores,omitempty"`
	Offset     *int              `json:"offset,omitempty"`
	Regime     machine.Regime    `json:"regime"`
	Workload   string            `json:"workload"`
	DurationS  int               `json:"duration_s"`
	Condition  machine.Condition `json:"condition"`
	Phase      Phase             `json:"phase,omitempty"`
	Retry      bool              `json:"retry,omitempty"`
	Cycle      int               `json:"cycle,omitempty"`
	Step       int               `json:"step,omitempty"`
	Hunt       int               `json:"hunt,omitempty"`
	Group      int               `json:"group,omitempty"`
	Round      int               `json:"round,omitempty"`
	Rerun      bool              `json:"rerun,omitempty"`
	RecordOnly bool              `json:"record_only,omitempty"`
}

func (*TrialIntent) Kind() Kind { return KindTrialIntent }
func (p *TrialIntent) Message() string {
	var b strings.Builder
	fmt.Fprintf(&b, "trial %s", p.Trial)
	if p.Core != nil {
		fmt.Fprintf(&b, " core %s", coreID(*p.Core))
	} else if len(p.Cores) > 0 {
		fmt.Fprintf(&b, " cores %s", coreList(p.Cores))
	}
	if p.Offset != nil {
		fmt.Fprintf(&b, " CO %d", *p.Offset)
	}
	label := p.Workload
	if w, ok := machine.WorkloadByID(p.Workload); ok {
		label = w.Label
	}
	fmt.Fprintf(&b, " %s %s %ds %s", p.Regime, label, p.DurationS, p.Condition)
	if p.Retry {
		b.WriteString(" (retry)")
	}
	if p.Cycle > 0 {
		fmt.Fprintf(&b, " cycle %d", p.Cycle)
	}
	if p.Step > 0 {
		fmt.Fprintf(&b, " step %d", p.Step)
	}
	if p.Hunt > 0 {
		fmt.Fprintf(&b, " hunt %d group %d", p.Hunt, p.Group)
	}
	if p.Round > 0 {
		fmt.Fprintf(&b, " round %d", p.Round)
	}
	switch {
	case p.Rerun && p.Condition == machine.Parked && p.Hunt == 0:
		b.WriteString(" rerun with every core at CO 0")
	case p.Rerun:
		b.WriteString(" rerun")
	}
	if p.RecordOnly {
		b.WriteString(" record-only")
	}
	return b.String()
}

type TrialStart struct {
	Trial         string          `json:"trial"`
	WindowStartNS *int64          `json:"window_start_ns,omitempty"`
	Scope         string          `json:"scope"`
	PID           int             `json:"pid"`
	CPUs          []int           `json:"cpus"`
	Argv          []string        `json:"argv"`
	Files         []string        `json:"files,omitempty"`
	Instances     []TrialInstance `json:"instances,omitempty"`
}

type TrialInstance struct {
	Core  int    `json:"core"`
	CPUs  []int  `json:"cpus"`
	PID   int    `json:"pid"`
	Scope string `json:"scope"`
}

func (*TrialStart) Kind() Kind { return KindTrialStart }
func (p *TrialStart) Message() string {
	if len(p.Instances) > 1 {
		return fmt.Sprintf("trial %s started %d instances in scopes %s-cNN on cpus %s", p.Trial, len(p.Instances), p.Scope, cpuList(p.CPUs))
	}
	where := "cpu"
	if len(p.CPUs) > 1 {
		where = "cpus"
	}
	return fmt.Sprintf("trial %s started pid %d in scope %s on %s %s", p.Trial, p.PID, p.Scope, where, cpuList(p.CPUs))
}

type TrialProgress struct {
	Trial  string         `json:"trial"`
	Detail string         `json:"detail"`
	Signal machine.Signal `json:"signal,omitempty"`
	Core   *int           `json:"core,omitempty"`
}

func (*TrialProgress) Kind() Kind        { return KindTrialProgress }
func (p *TrialProgress) Message() string { return fmt.Sprintf("trial %s %s", p.Trial, p.Detail) }

type TrialSignal struct {
	Trial    string `json:"trial"`
	Schedule string `json:"schedule,omitempty"`
	Seed     uint64 `json:"seed,omitempty"`
	Stops    int    `json:"stops,omitempty"`
	Conts    int    `json:"conts,omitempty"`
}

func (*TrialSignal) Kind() Kind { return KindTrialSignal }
func (p *TrialSignal) Message() string {
	if p.Schedule == "" {
		return fmt.Sprintf("trial %s load steps: %d stops, %d continues", p.Trial, p.Stops, p.Conts)
	}
	if p.Seed != 0 {
		return fmt.Sprintf("trial %s load steps: %s, seed %d", p.Trial, p.Schedule, p.Seed)
	}
	return fmt.Sprintf("trial %s load steps: %s", p.Trial, p.Schedule)
}

type TrialSample struct {
	Trial   string `json:"trial"`
	Warning string `json:"warning"`
	PID     int    `json:"pid"`
	TID     int    `json:"tid"`
	CPU     int    `json:"cpu"`
}

func (*TrialSample) Kind() Kind { return KindTrialSample }
func (p *TrialSample) Message() string {
	return fmt.Sprintf("trial %s thread %d %s on cpu %d", p.Trial, p.TID, p.Warning, p.CPU)
}

type TrialEnd struct {
	Trial string `json:"trial"`
	KernelBoundary
	Outcome               Outcome         `json:"outcome"`
	Signal                machine.Signal  `json:"signal,omitempty"`
	Core                  *int            `json:"core,omitempty"`
	DurationS             int             `json:"duration_s"`
	TctlMaxC              *int            `json:"tctl_max_c,omitempty"`
	VoltageRequestMedianV *float64        `json:"voltage_request_median_v,omitempty"`
	VoltageRequestMinV    *float64        `json:"voltage_request_min_v,omitempty"`
	VoltageRequestsV      map[int]float64 `json:"voltage_requests_v,omitempty"`
	TopRequesters         []int           `json:"top_requesters,omitempty"`
	CCDMHz                map[int]int     `json:"ccd_mhz,omitempty"`
	LastSampleS           *int            `json:"last_sample_s,omitempty"`
	LastSampleTctlC       *int            `json:"last_sample_tctl_c,omitempty"`
	LastSampleMinMHz      *int            `json:"last_sample_min_mhz,omitempty"`
	LastSampleMaxMHz      *int            `json:"last_sample_max_mhz,omitempty"`
	StalledCore           *int            `json:"stalled_core,omitempty"`
	WorkerStalledMS       *int64          `json:"worker_stalled_ms,omitempty"`
	Reason                string          `json:"reason,omitempty"`
	Interrupted           bool            `json:"interrupted,omitempty"`
	Escaped               []int           `json:"escaped,omitempty"`
	BackendMissing        bool            `json:"backend_missing,omitempty"`
	ContainmentError      string          `json:"containment_error,omitempty"`
	// LastEvidence marks DurationS as the last evidence of a trial togi stopped
	// watching rather than a measured run. Only the message carries it.
	LastEvidence bool `json:"-"`
}

func (*TrialEnd) Kind() Kind { return KindTrialEnd }
func (p *TrialEnd) Message() string {
	tctl := ""
	if p.TctlMaxC != nil {
		tctl = fmt.Sprintf(" | Tctl max %d°C", *p.TctlMaxC)
	}
	voltage := ""
	if p.VoltageRequestMedianV != nil && p.VoltageRequestMinV != nil {
		voltage = fmt.Sprintf(" | loaded voltage request median %.3f V, min %.3f V", *p.VoltageRequestMedianV, *p.VoltageRequestMinV)
	}
	if len(p.TopRequesters) > 0 {
		top := make([]string, len(p.TopRequesters))
		for i, core := range p.TopRequesters {
			top[i] = fmt.Sprintf("%02d %.3f V", core, p.VoltageRequestsV[core])
		}
		voltage += " | top requester " + strings.Join(top, ", ")
	}
	tctl += voltage
	duration := fmt.Sprintf(" after %ds", p.DurationS)
	if p.Signal == machine.Crash || p.LastEvidence {
		duration = fmt.Sprintf(", last evidence %ds after start", p.DurationS)
	}
	if p.LastSampleS != nil {
		duration += fmt.Sprintf(", last sample %ds", *p.LastSampleS)
		var conditions []string
		if p.LastSampleTctlC != nil {
			conditions = append(conditions, fmt.Sprintf("Tctl %d°C", *p.LastSampleTctlC))
		}
		if p.LastSampleMinMHz != nil && p.LastSampleMaxMHz != nil {
			conditions = append(conditions, fmt.Sprintf("%d-%d MHz", *p.LastSampleMinMHz, *p.LastSampleMaxMHz))
		}
		if len(conditions) > 0 {
			duration += ": " + strings.Join(conditions, ", ")
		}
	}
	if p.StalledCore != nil && p.WorkerStalledMS != nil {
		duration += fmt.Sprintf(", core %s worker CPU time stopped advancing at %dms after start (evidence only)", coreID(*p.StalledCore), *p.WorkerStalledMS)
	}
	switch p.Outcome {
	case OutcomePass:
		return fmt.Sprintf("trial %s PASS %ds%s", p.Trial, p.DurationS, tctl)
	case OutcomeFailure:
		if p.Core != nil {
			return fmt.Sprintf("trial %s FAIL %s on core %s%s%s", p.Trial, p.Signal, coreID(*p.Core), duration, tctl)
		}
		return fmt.Sprintf("trial %s FAIL %s%s%s", p.Trial, p.Signal, duration, tctl)
	case OutcomeInconclusive:
		return fmt.Sprintf("trial %s INCONCLUSIVE%s: %s%s", p.Trial, duration, p.Reason, voltage)
	}
	return fmt.Sprintf("trial %s %s%s", p.Trial, p.Outcome, duration)
}

type Failure struct {
	Signal       machine.Signal    `json:"signal"`
	Attribution  Attribution       `json:"attribution"`
	Core         *int              `json:"core,omitempty"`
	Offset       *int              `json:"offset,omitempty"`
	Trial        string            `json:"trial,omitempty"`
	Regime       machine.Regime    `json:"regime,omitempty"`
	Condition    machine.Condition `json:"condition,omitempty"`
	Profile      []int             `json:"profile,omitempty"`
	KnownFailure int               `json:"known_failure,omitempty"`
	Reason       string            `json:"reason,omitempty"`
	Round        int               `json:"round,omitempty"`
}

func (*Failure) Kind() Kind { return KindFailure }
func (p *Failure) Message() string {
	if p.KnownFailure != 0 {
		return p.Reason
	}
	loaded := p.Condition == machine.Together || p.Condition == machine.Parked
	profile := "current"
	if p.Condition == machine.Parked {
		profile = "group"
	}
	switch p.Attribution {
	case Attributed:
		if p.Core == nil || p.Offset == nil {
			break
		}
		switch {
		case loaded && p.Trial == "":
			return fmt.Sprintf("core %s failure at CO %d: %s with the %s profile applied and no trial in flight, the only nonzero core", coreID(*p.Core), *p.Offset, p.Signal, profile)
		case loaded:
			return fmt.Sprintf("core %s failure at CO %d: %s in %s %s trial %s", coreID(*p.Core), *p.Offset, p.Signal, p.Condition, p.Regime, p.Trial)
		}
		return fmt.Sprintf("core %s failure at CO %d: %s in trial %s (alone: attributed to the target)", coreID(*p.Core), *p.Offset, p.Signal, p.Trial)
	case Unattributed:
		switch {
		case p.Condition == machine.Parked && p.Trial != "":
			return fmt.Sprintf("unattributed %s failure in parked %s trial %s: the group fails, no evidence names a single core", p.Signal, p.Regime, p.Trial)
		case loaded && p.Trial != "":
			return fmt.Sprintf("unattributed %s failure in together %s trial %s: no evidence names a single core", p.Signal, p.Regime, p.Trial)
		case loaded:
			return fmt.Sprintf("unattributed %s failure with the %s profile applied and no trial in flight: counted as an %s failure", p.Signal, profile, p.Regime)
		}
		return fmt.Sprintf("unattributed %s failure: no trial was in flight", p.Signal)
	}
	return fmt.Sprintf("%s %s failure", p.Attribution, p.Signal)
}

type MCE struct {
	CPU           int              `json:"cpu"`
	Core          int              `json:"core"`
	Bank          int              `json:"bank"`
	BankType      machine.BankType `json:"bank_type"`
	Corrected     bool             `json:"corrected"`
	MonotonicNS   *int64           `json:"monotonic_ns,omitempty"`
	FromBoot      string           `json:"from_boot,omitempty"`
	Trial         string           `json:"trial,omitempty"`
	BetweenTrials bool             `json:"between_trials,omitempty"`
	Lines         []string         `json:"lines"`
}

func (*MCE) Kind() Kind { return KindMCE }
func (p *MCE) Message() string {
	kind := "uncorrected"
	if p.Corrected {
		kind = "corrected"
	}
	scope := "shared"
	if p.BankType.CoreLocal() {
		scope = "core-local"
	}
	msg := fmt.Sprintf("%s MCE on cpu %d (core %s), bank %d %s (%s)", kind, p.CPU, coreID(p.Core), p.Bank, p.BankType, scope)
	if p.FromBoot != "" {
		msg += " from boot " + shortBoot(p.FromBoot)
	}
	if p.BetweenTrials {
		msg += " between trials (recorded only)"
	}
	return msg
}

type CrashDetected struct {
	PreviousBoot   string                `json:"previous_boot"`
	InFlight       *int                  `json:"in_flight,omitempty"`
	Stray          bool                  `json:"stray,omitempty"`
	Condition      machine.Condition     `json:"condition,omitempty"`
	ResetReason    machine.ResetKind     `json:"reset_reason,omitempty"`
	ResetReasonRaw string                `json:"reset_reason_raw,omitempty"`
	Inconclusive   bool                  `json:"inconclusive,omitempty"`
	Pstore         *machine.PstoreRecord `json:"pstore,omitempty"`
}

func (*CrashDetected) Kind() Kind { return KindCrashDetected }
func (p *CrashDetected) Message() string {
	msg := fmt.Sprintf("crash: boot %s ended without a clean shutdown; ", shortBoot(p.PreviousBoot))
	if p.InFlight != nil {
		msg += fmt.Sprintf("in flight: seq %d", *p.InFlight)
	} else {
		msg += "nothing in flight"
	}
	if p.Stray {
		msg += "; stray: the profile was never applied in that boot"
	}
	if p.Condition == machine.Together {
		msg += "; the current profile was applied"
	}
	if p.ResetReason != "" || p.ResetReasonRaw != "" {
		reason := p.ResetReasonRaw
		if reason == "" {
			reason = string(p.ResetReason)
		}
		msg += "; reset reason: " + reason
	}
	if p.Inconclusive {
		msg += "; inconclusive: reset does not establish a tuning failure"
	}
	if p.Pstore != nil {
		msg += "; pstore: " + p.Pstore.Path
	}
	return msg
}

type TunerDecision struct {
	Core         int      `json:"core"`
	Phase        Phase    `json:"phase"`
	Decision     Decision `json:"decision"`
	FromOffset   int      `json:"from_offset"`
	ToOffset     int      `json:"to_offset"`
	Pass         *int     `json:"pass"`
	FailurePoint *int     `json:"failure_point"`
	Workloads    []string `json:"workloads,omitempty"`
	Reason       string   `json:"reason"`
}

func (*TunerDecision) Kind() Kind { return KindTunerDecision }
func (p *TunerDecision) Message() string {
	verb := string(p.Decision)
	switch p.Decision {
	case StepDeeper:
		verb = "passed R1+R2"
	case CheckSoloLimit:
		verb = "checks its solo limit"
	case Deepen:
		verb = "deepened"
	case Yield:
		verb = "yielded"
	case Backoff:
		if p.Phase == PhaseSearch {
			verb = "failed"
		} else {
			verb = "backed off"
		}
	}
	return fmt.Sprintf("core %s %s at %d; next %d (%s)", coreID(p.Core), verb, p.FromOffset, p.ToOffset, p.Reason)
}

type CorePhase struct {
	Core               int      `json:"core"`
	From               Phase    `json:"from"`
	To                 Phase    `json:"to"`
	Offset             int      `json:"offset"`
	Pass               *int     `json:"pass"`
	FailurePoint       *int     `json:"failure_point"`
	CheckSoloLimit     bool     `json:"check_solo_limit,omitempty"`
	Workloads          []string `json:"workloads,omitempty"`
	ClearedCombination []int    `json:"cleared_combination,omitempty"`
	Reason             string   `json:"reason"`
}

func (*CorePhase) Kind() Kind { return KindCorePhase }
func (p *CorePhase) Message() string {
	if p.From == "" {
		return fmt.Sprintf("core %s starts %s at %d (%s)", coreID(p.Core), phaseText(p.To), p.Offset, p.Reason)
	}
	return fmt.Sprintf("core %s %s -> %s at %d (%s)", coreID(p.Core), phaseText(p.From), phaseText(p.To), p.Offset, p.Reason)
}

func phaseText(phase Phase) string {
	switch phase {
	case PhaseHasRoom:
		return "has room"
	case PhaseAtLimit:
		return "at its limit"
	case PhaseSearch, PhaseChecking, PhaseHunt, PhaseDeepening:
	}
	return string(phase)
}

type CheckingCycle struct {
	Cycle   int              `json:"cycle"`
	Event   CycleEvent       `json:"event"`
	Passed  bool             `json:"passed,omitempty"`
	Full    bool             `json:"full,omitempty"`
	Missing []string         `json:"missing,omitempty"`
	Steps   []machine.Regime `json:"steps,omitempty"`
	Reason  string           `json:"reason,omitempty"`
}

func (*CheckingCycle) Kind() Kind { return KindCheckingCycle }
func (p *CheckingCycle) Message() string {
	switch p.Event {
	case CycleStart:
		steps := make([]string, len(p.Steps))
		for i, r := range p.Steps {
			steps[i] = string(r)
		}
		return fmt.Sprintf("checking cycle %d start: %s%s", p.Cycle, strings.Join(steps, " "), p.Reason)
	case CycleEnd:
		if p.Passed {
			return fmt.Sprintf("checking cycle %d end passed%s", p.Cycle, p.Reason)
		}
		return fmt.Sprintf("checking cycle %d end, not passed: %s", p.Cycle, p.Reason)
	}
	return fmt.Sprintf("checking cycle %d %s", p.Cycle, p.Event)
}

type CheckingPartial struct {
	CCD    int    `json:"ccd"`
	Cores  []int  `json:"cores"`
	Reason string `json:"reason,omitempty"`
}

type CheckingStep struct {
	Cycle    int               `json:"cycle"`
	Step     int               `json:"step"`
	Profile  []int             `json:"profile"`
	Partials []CheckingPartial `json:"partials"`
}

func (*CheckingStep) Kind() Kind { return KindCheckingStep }
func (p *CheckingStep) Message() string {
	return fmt.Sprintf("checking cycle %d R7 step %d starts at current profile %v", p.Cycle, p.Step, p.Profile)
}

type CheckingChain struct {
	Cycle      int     `json:"cycle"`
	Step       int     `json:"step"`
	CCD        int     `json:"ccd"`
	Workload   string  `json:"workload"`
	Groups     [][]int `json:"groups"`
	Cores      []int   `json:"cores"`
	SourceSeqs []int   `json:"source_seqs"`
	Profile    []int   `json:"profile"`
	Part       string  `json:"part"`
	Msg        string  `json:"-"`
}

func (*CheckingChain) Kind() Kind { return KindCheckingChain }
func (p *CheckingChain) Message() string {
	if p.Msg != "" {
		return p.Msg
	}
	source := fmt.Sprintf("request measurements %v", p.SourceSeqs)
	if len(p.SourceSeqs) == 0 {
		source = "offset fallback (no request telemetry)"
	}
	if len(p.Cores) == 0 {
		return fmt.Sprintf("checking cycle %d R7 step %d CCD %d %s: request groups %v from %s; partial chain ends because idling the next top-requester group leaves fewer than two loaded cores", p.Cycle, p.Step, p.CCD, p.Workload, p.Groups, source)
	}
	return fmt.Sprintf("checking cycle %d R7 step %d CCD %d %s: request groups %v from %s; next %s loads cores %s at profile %v", p.Cycle, p.Step, p.CCD, p.Workload, p.Groups, source, p.Part, coreList(p.Cores), p.Profile)
}

type CommandReset struct {
	Core *int `json:"core,omitempty"`
	All  bool `json:"all,omitempty"`
}

func (*CommandReset) Kind() Kind { return KindCommandReset }
func (p *CommandReset) Message() string {
	if p.Core != nil {
		return fmt.Sprintf("reset core %s", coreID(*p.Core))
	}
	return "reset all: new session on next run"
}

type DefectFound struct {
	ID        int    `json:"id"`
	Title     string `json:"title"`
	Detail    string `json:"detail"`
	PR        int    `json:"pr"`
	Direction string `json:"direction"`
	Cores     []int  `json:"cores"`
	Decisions []int  `json:"decisions"`
}

func (*DefectFound) Kind() Kind { return KindDefectFound }
func (p *DefectFound) Message() string {
	msg := fmt.Sprintf("defect %d (%s, fixed in pull request #%d): %s decisions %v affected cores %s", p.ID, p.Title, p.PR, p.Direction, p.Decisions, coreList(p.Cores))
	if p.Detail != "" {
		msg += "; " + p.Detail
	}
	return msg
}

type DefectAnswered struct {
	ID     int    `json:"id"`
	Cores  []int  `json:"cores"`
	Answer string `json:"answer"`
}

func (*DefectAnswered) Kind() Kind { return KindDefectAnswered }
func (p *DefectAnswered) Message() string {
	return fmt.Sprintf("defect %d: answered %s to reset cores %s", p.ID, p.Answer, coreList(p.Cores))
}

type DeadEnd struct {
	Condition DeadEndCondition `json:"condition"`
	Core      *int             `json:"core,omitempty"`
	Detail    string           `json:"detail"`
	Action    DeadEndAction    `json:"action,omitempty"`
}

type DeadEndAction string

const (
	ActionExit                     DeadEndAction = "exit"
	ActionClearSavedEntry          DeadEndAction = "clear_saved_entry"
	ActionClearSavedEntryAndReboot DeadEndAction = "clear_saved_entry_and_reboot"
)

func (*DeadEnd) Kind() Kind { return KindDeadEnd }
func (p *DeadEnd) Message() string {
	msg := fmt.Sprintf("dead end %s: %s", p.Condition, p.Detail)
	switch p.Action {
	case ActionClearSavedEntry:
		msg += "; clearing GRUB's saved entry"
	case ActionClearSavedEntryAndReboot:
		msg += "; clearing GRUB's saved entry and rebooting"
	case ActionExit:
	}
	return msg
}

type BootSavedEntry struct {
	Before      string `json:"before"`
	After       string `json:"after"`
	Error       string `json:"error,omitempty"`
	ReasonError string `json:"reason_error,omitempty"`
}

func (*BootSavedEntry) Kind() Kind { return KindBootSavedEntry }
func (p *BootSavedEntry) Message() string {
	var msg string
	switch {
	case p.Error != "":
		msg = "GRUB saved entry not cleared: " + p.Error
	case p.After == "" && p.Before == "":
		msg = "GRUB saved entry was already unset"
	case p.After == "":
		msg = fmt.Sprintf("GRUB saved entry %s cleared; the next boot selects the first menu entry", p.Before)
	default:
		msg = fmt.Sprintf("GRUB saved entry %s -> %s", p.Before, p.After)
	}
	if p.ReasonError != "" {
		msg += "; leave reason not saved: " + p.ReasonError
	}
	return msg
}

type BootLeaveReason struct {
	ReasonID          string `json:"reason_id"`
	RestartLimitCount int    `json:"restart_limit_count"`
	Reason            string `json:"reason"`
}

func (*BootLeaveReason) Kind() Kind { return KindBootLeaveReason }
func (p *BootLeaveReason) Message() string {
	return fmt.Sprintf("previous tuning boot ended: %s (consecutive restart-limit boots: %d)", p.Reason, p.RestartLimitCount)
}

type Shutdown struct {
	Reason ShutdownReason `json:"reason"`
	KernelBoundary
	Cycles int `json:"cycles,omitempty"`
}

func (*Shutdown) Kind() Kind { return KindShutdown }
func (p *Shutdown) Message() string {
	switch p.Reason {
	case ShutdownSignal:
		return "stopped by signal"
	case ShutdownDeadEnd:
		return "stopped at a dead end"
	case ShutdownCycles:
		if p.Cycles == 1 {
			return "every core is at its limit and the profile passed the requested clean cycle; stopping"
		}
		return fmt.Sprintf("every core is at its limit and the profile passed the requested %d clean cycles; stopping", p.Cycles)
	case ShutdownCommand:
		return "command finished"
	}
	return fmt.Sprintf("stopped: %s", p.Reason)
}

type JournalTorn struct {
	Offset   int64  `json:"offset"`
	BytesHex string `json:"bytes_hex"`
}

func (*JournalTorn) Kind() Kind { return KindJournalTorn }
func (p *JournalTorn) Message() string {
	return fmt.Sprintf("journal had %d torn bytes at offset %d; discarded", len(p.BytesHex)/2, p.Offset)
}

type StateRebuilt struct {
	Fields []string `json:"fields"`
}

func (*StateRebuilt) Kind() Kind { return KindStateRebuilt }
func (p *StateRebuilt) Message() string {
	return fmt.Sprintf("state.json rebuilt from the journal; differed in %s", strings.Join(p.Fields, ", "))
}
