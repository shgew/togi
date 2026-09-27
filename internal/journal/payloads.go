package journal

import (
	"fmt"
	"slices"
	"strconv"
	"strings"

	"github.com/shgew/shycler/internal/config"
	"github.com/shgew/shycler/internal/machine"
)

type Phase string

const (
	PhaseSearch       Phase = "search"
	PhaseConfirmation Phase = "confirmation"
	PhaseConfirmed    Phase = "confirmed"
	PhaseGuard        Phase = "guard"
)

type Decision string

const (
	StepDeeper     Decision = "step_deeper"
	Backoff        Decision = "backoff"
	SuspectBackoff Decision = "suspect_backoff"
	Regain         Decision = "regain"
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

type RotationEvent string

const (
	RotationStart RotationEvent = "start"
	RotationEnd   RotationEvent = "end"
)

type ShutdownReason string

const (
	ShutdownSignal    ShutdownReason = "signal"
	ShutdownDeadEnd   ShutdownReason = "dead_end"
	ShutdownRotations ShutdownReason = "rotations"
	ShutdownCommand   ShutdownReason = "command"
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

type SessionStart struct {
	Build
	Session string             `json:"session"`
	Cores   []machine.CoreInfo `json:"cores"`
}

func (*SessionStart) Kind() Kind { return KindSessionStart }
func (p *SessionStart) Message() string {
	return fmt.Sprintf("session %s started by %s (schema %d, ruleset %d, fixes %d, %d cores)", p.Session, p.name(), p.Schema, p.Ruleset, p.Fixes, len(p.Cores))
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

type ConfigLoaded struct {
	Build
	Path   string        `json:"path"`
	File   bool          `json:"file"`
	Config config.Config `json:"config"`
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
	case machine.Isolated:
		return "profile applied for isolated trials: every core at CO 0"
	case machine.Resident:
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
		return fmt.Sprintf("profile for guard: %v", p.To)
	case slices.Equal(p.From, p.To):
		return fmt.Sprintf("profile unchanged at %v; guard restarts", p.To)
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
	Trial     string            `json:"trial"`
	Core      *int              `json:"core,omitempty"`
	Cores     []int             `json:"cores,omitempty"`
	Offset    *int              `json:"offset,omitempty"`
	Regime    machine.Regime    `json:"regime"`
	Workload  string            `json:"workload"`
	DurationS int               `json:"duration_s"`
	Condition machine.Condition `json:"condition"`
	Phase     Phase             `json:"phase,omitempty"`
	Retry     bool              `json:"retry,omitempty"`
	Rotation  int               `json:"rotation,omitempty"`
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
	if p.Rotation > 0 {
		fmt.Fprintf(&b, " rotation %d", p.Rotation)
	}
	return b.String()
}

type TrialStart struct {
	Trial     string          `json:"trial"`
	Scope     string          `json:"scope"`
	PID       int             `json:"pid"`
	CPUs      []int           `json:"cpus"`
	Argv      []string        `json:"argv"`
	Files     []string        `json:"files,omitempty"`
	Instances []TrialInstance `json:"instances,omitempty"`
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
	Trial  string `json:"trial"`
	Detail string `json:"detail"`
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
	Trial       string         `json:"trial"`
	Outcome     Outcome        `json:"outcome"`
	Signal      machine.Signal `json:"signal,omitempty"`
	Core        *int           `json:"core,omitempty"`
	DurationS   int            `json:"duration_s"`
	TctlMaxC    *int           `json:"tctl_max_c,omitempty"`
	Reason      string         `json:"reason,omitempty"`
	Interrupted bool           `json:"interrupted,omitempty"`
	Escaped     []int          `json:"escaped,omitempty"`
}

func (*TrialEnd) Kind() Kind { return KindTrialEnd }
func (p *TrialEnd) Message() string {
	tctl := ""
	if p.TctlMaxC != nil {
		tctl = fmt.Sprintf(" | Tctl max %d°C", *p.TctlMaxC)
	}
	duration := fmt.Sprintf(" after %ds", p.DurationS)
	if p.Signal == machine.Crash || p.Interrupted && strings.HasPrefix(p.Reason, "shycler stopped during the trial") {
		duration = fmt.Sprintf(", last evidence %ds after start", p.DurationS)
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
		return fmt.Sprintf("trial %s INCONCLUSIVE%s: %s", p.Trial, duration, p.Reason)
	}
	return fmt.Sprintf("trial %s %s%s", p.Trial, p.Outcome, duration)
}

type Failure struct {
	Signal      machine.Signal    `json:"signal"`
	Attribution Attribution       `json:"attribution"`
	Core        *int              `json:"core,omitempty"`
	Offset      *int              `json:"offset,omitempty"`
	Trial       string            `json:"trial,omitempty"`
	Regime      machine.Regime    `json:"regime,omitempty"`
	Condition   machine.Condition `json:"condition,omitempty"`
}

func (*Failure) Kind() Kind { return KindFailure }
func (p *Failure) Message() string {
	resident := p.Condition == machine.Resident
	switch p.Attribution {
	case Attributed:
		if p.Core == nil || p.Offset == nil {
			break
		}
		if resident {
			return fmt.Sprintf("core %s failure at CO %d: %s in resident %s trial %s", coreID(*p.Core), *p.Offset, p.Signal, p.Regime, p.Trial)
		}
		return fmt.Sprintf("core %s failure at CO %d: %s in trial %s (isolated: attributed to the target)", coreID(*p.Core), *p.Offset, p.Signal, p.Trial)
	case Unattributed:
		switch {
		case resident && p.Trial != "":
			return fmt.Sprintf("unattributed %s failure in resident %s trial %s: no evidence names a single core", p.Signal, p.Regime, p.Trial)
		case resident:
			return fmt.Sprintf("unattributed %s failure with the profile applied and no trial in flight: counted as an %s failure", p.Signal, p.Regime)
		}
		return fmt.Sprintf("unattributed %s failure: no trial was in flight", p.Signal)
	}
	return fmt.Sprintf("%s %s failure", p.Attribution, p.Signal)
}

type MCE struct {
	CPU       int              `json:"cpu"`
	Core      int              `json:"core"`
	Bank      int              `json:"bank"`
	BankType  machine.BankType `json:"bank_type"`
	Corrected bool             `json:"corrected"`
	FromBoot  string           `json:"from_boot,omitempty"`
	Lines     []string         `json:"lines"`
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
	return msg
}

type CrashDetected struct {
	PreviousBoot string            `json:"previous_boot"`
	InFlight     *int              `json:"in_flight,omitempty"`
	Stray        bool              `json:"stray,omitempty"`
	Condition    machine.Condition `json:"condition,omitempty"`
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
	if p.Condition == machine.Resident {
		msg += "; the resident profile was applied"
	}
	return msg
}

type TunerDecision struct {
	Core          int      `json:"core"`
	Phase         Phase    `json:"phase"`
	Decision      Decision `json:"decision"`
	FromOffset    int      `json:"from_offset"`
	ToOffset      int      `json:"to_offset"`
	Pass          *int     `json:"pass"`
	FailedMark    *int     `json:"failed_mark"`
	UnprovenDepth int      `json:"unproven_depth,omitempty"`
	SettledMark   *int     `json:"settled_mark,omitempty"`
	SpentSteps    []int    `json:"spent_steps,omitempty"`
	Reason        string   `json:"reason"`
}

func (*TunerDecision) Kind() Kind { return KindTunerDecision }
func (p *TunerDecision) Message() string {
	verb := string(p.Decision)
	switch p.Decision {
	case StepDeeper:
		verb = "passed R1+R2"
	case Backoff:
		switch p.Phase {
		case PhaseSearch:
			verb = "failed"
		case PhaseConfirmation, PhaseConfirmed:
			verb = "failed confirmation"
		case PhaseGuard:
			verb = "failed in guard"
		}
	case SuspectBackoff:
		verb = "backed off on suspicion"
	case Regain:
		verb = "regained one count"
	}
	return fmt.Sprintf("core %s %s at %d; next %d (%s)", coreID(p.Core), verb, p.FromOffset, p.ToOffset, p.Reason)
}

type CorePhase struct {
	Core          int    `json:"core"`
	From          Phase  `json:"from"`
	To            Phase  `json:"to"`
	Offset        int    `json:"offset"`
	Pass          *int   `json:"pass"`
	FailedMark    *int   `json:"failed_mark"`
	UnprovenDepth int    `json:"unproven_depth,omitempty"`
	Reason        string `json:"reason"`
}

func (*CorePhase) Kind() Kind { return KindCorePhase }
func (p *CorePhase) Message() string {
	if p.From == "" {
		return fmt.Sprintf("core %s starts %s at %d (%s)", coreID(p.Core), p.To, p.Offset, p.Reason)
	}
	return fmt.Sprintf("core %s %s -> %s at %d (%s)", coreID(p.Core), p.From, p.To, p.Offset, p.Reason)
}

type GuardRotation struct {
	Rotation int              `json:"rotation"`
	Event    RotationEvent    `json:"event"`
	Clean    bool             `json:"clean,omitempty"`
	Steps    []machine.Regime `json:"steps,omitempty"`
	Reason   string           `json:"reason,omitempty"`
}

func (*GuardRotation) Kind() Kind { return KindGuardRotation }
func (p *GuardRotation) Message() string {
	switch p.Event {
	case RotationStart:
		steps := make([]string, len(p.Steps))
		for i, r := range p.Steps {
			steps[i] = string(r)
		}
		return fmt.Sprintf("guard rotation %d start: %s", p.Rotation, strings.Join(steps, " "))
	case RotationEnd:
		if p.Clean {
			return fmt.Sprintf("guard rotation %d end clean", p.Rotation)
		}
		return fmt.Sprintf("guard rotation %d end, not clean: %s", p.Rotation, p.Reason)
	}
	return fmt.Sprintf("guard rotation %d %s", p.Rotation, p.Event)
}

type Tier string

const (
	TierNone     Tier = "none"
	TierBronze   Tier = "bronze"
	TierSilver   Tier = "silver"
	TierGold     Tier = "gold"
	TierPlatinum Tier = "platinum"
)

type TierChange struct {
	From   Tier   `json:"from"`
	To     Tier   `json:"to"`
	Reason string `json:"reason"`
}

func (*TierChange) Kind() Kind { return KindTierChange }
func (p *TierChange) Message() string {
	return fmt.Sprintf("tier %s -> %s: %s", p.From, p.To, p.Reason)
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
	Before string `json:"before"`
	After  string `json:"after"`
	Error  string `json:"error,omitempty"`
}

func (*BootSavedEntry) Kind() Kind { return KindBootSavedEntry }
func (p *BootSavedEntry) Message() string {
	switch {
	case p.Error != "":
		return "GRUB saved entry not cleared: " + p.Error
	case p.After == "" && p.Before == "":
		return "GRUB saved entry was already unset"
	case p.After == "":
		return fmt.Sprintf("GRUB saved entry %s cleared; the next boot selects the first menu entry", p.Before)
	}
	return fmt.Sprintf("GRUB saved entry %s -> %s", p.Before, p.After)
}

type Shutdown struct {
	Reason    ShutdownReason `json:"reason"`
	Rotations int            `json:"rotations,omitempty"`
}

func (*Shutdown) Kind() Kind { return KindShutdown }
func (p *Shutdown) Message() string {
	switch p.Reason {
	case ShutdownSignal:
		return "stopped by signal"
	case ShutdownDeadEnd:
		return "stopped at a dead end"
	case ShutdownRotations:
		return fmt.Sprintf("the profile survived the requested %d clean rotation(s); stopping", p.Rotations)
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
