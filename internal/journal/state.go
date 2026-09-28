package journal

import (
	"bytes"
	"encoding/json"
	"fmt"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"time"

	"github.com/shgew/togi/internal/machine"
)

const (
	stateFile    = "state.json"
	stateTmpFile = ".state.json.tmp"
)

type State struct {
	Schema     int              `json:"schema"`
	Session    *SessionInfo     `json:"session"`
	LastSeq    int              `json:"last_seq"`
	Phase      string           `json:"phase"`
	Cores      []CoreState      `json:"cores"`
	InFlight   *InFlight        `json:"in_flight"`
	DeadEnd    *DeadEndRef      `json:"dead_end"`
	Guard      *GuardState      `json:"guard"`
	JointMarks []JointMarkState `json:"joint_marks"`
	Hunt       *HuntState       `json:"hunt"`
	Refine     *RefineState     `json:"refine"`
	Tier       Tier             `json:"tier"`
	TierSeq    int              `json:"tier_seq"`

	open []openIntent
}

type SessionInfo struct {
	ID          string               `json:"id"`
	Start       time.Time            `json:"start"`
	BIOSContext *machine.BIOSContext `json:"bios_context"`
	Baseline    []int                `json:"baseline"`
}

type CoreState struct {
	Core         int          `json:"core"`
	CCD          int          `json:"ccd"`
	CPUs         []int        `json:"cpus"`
	Baseline     *int         `json:"baseline"`
	Offset       int          `json:"offset"`
	Phase        Phase        `json:"phase"`
	Pass         *int         `json:"pass"`
	FailedMark   *int         `json:"failed_mark"`
	JointMarks   []int        `json:"joint_marks,omitempty"`
	Queued       string       `json:"queued,omitempty"`
	LastDecision *DecisionRef `json:"last_decision"`
}

type GuardState struct {
	Rotation       int              `json:"rotation"`
	RotationOpen   bool             `json:"rotation_open"`
	Steps          []machine.Regime `json:"steps"`
	StepsDone      int              `json:"steps_done"`
	Profile        []int            `json:"profile"`
	ProfileSeq     int              `json:"profile_seq"`
	Qualifying     bool             `json:"qualifying"`
	Missing        []string         `json:"missing"`
	TierClockSeq   int              `json:"tier_clock_seq"`
	Exposure       []ExposureRow    `json:"exposure"`
	CleanRotations int              `json:"clean_rotations"`
	CleanS         int              `json:"clean_s"`
	Regimes        []RegimeClean    `json:"regimes"`
	RateBoundPerH  *float64         `json:"rate_bound_per_h"`
	TctlMaxC       *int             `json:"tctl_max_c"`
	TctlMaxSeq     int              `json:"tctl_max_seq"`
}

type JointMarkState struct {
	Mark     int           `json:"mark"`
	Members  []JointMember `json:"members"`
	Fallback bool          `json:"fallback"`
	Hunt     int           `json:"hunt"`
	Seq      int           `json:"seq"`
}

type HuntState struct {
	Hunt       int            `json:"hunt"`
	Seq        int            `json:"seq"`
	Failure    int            `json:"failure"`
	Regime     machine.Regime `json:"regime"`
	Trial      string         `json:"trial"`
	Anchor     []int          `json:"anchor"`
	AnchorSeq  int            `json:"anchor_seq"`
	Candidates []int          `json:"candidates"`
	Escalated  bool           `json:"escalated"`
	Masks      []MaskState    `json:"masks"`
}

type MaskState struct {
	Mask    int    `json:"mask"`
	Seq     int    `json:"seq"`
	Cores   []int  `json:"cores"`
	Outcome string `json:"outcome"`
	Passes  int    `json:"passes"`
	Needed  int    `json:"needed"`
}

type RefineState struct {
	Round   int          `json:"round"`
	Seq     int          `json:"seq"`
	Target  []int        `json:"target"`
	Profile []int        `json:"profile"`
	Cores   []int        `json:"cores"`
	Checks  []CheckState `json:"checks"`
}

type CheckState struct {
	Regime   machine.Regime `json:"regime"`
	Workload string         `json:"workload"`
	Cores    []int          `json:"cores"`
	Passes   int            `json:"passes"`
	Needed   int            `json:"needed"`
}

type ExposureRow struct {
	Regime        machine.Regime `json:"regime"`
	Workload      string         `json:"workload"`
	Starts        int            `json:"starts"`
	CleanS        int            `json:"clean_s"`
	RateBoundPerH *float64       `json:"rate_bound_per_h"`
}

type RegimeClean struct {
	Regime        machine.Regime `json:"regime"`
	CleanS        int            `json:"clean_s"`
	RateBoundPerH *float64       `json:"rate_bound_per_h"`
}

type DecisionRef struct {
	Seq int    `json:"seq"`
	Msg string `json:"msg"`
}

type InFlight struct {
	Seq  int    `json:"seq"`
	Kind Kind   `json:"kind"`
	Msg  string `json:"msg"`
}

type DeadEndRef struct {
	Condition DeadEndCondition `json:"condition"`
	Seq       int              `json:"seq"`
}

type openIntent struct {
	InFlight
	boot  string
	trial string
}

func (s *State) Fold(e Event) {
	s.Schema = Schema
	s.LastSeq = e.Seq
	switch p := e.Data.(type) {
	case *SessionStart:
		s.Session = &SessionInfo{ID: p.Session, Start: e.Time}
		cores := slices.SortedFunc(slices.Values(p.Cores), func(a, b machine.CoreInfo) int { return a.Core - b.Core })
		s.Cores = make([]CoreState, len(cores))
		for i, c := range cores {
			s.Cores[i] = CoreState{Core: c.Core, CCD: c.CCD, CPUs: c.CPUs}
		}
	case *SessionContext:
		ctx := p.BIOSContext
		s.Session.BIOSContext = &ctx
	case *SessionBaseline:
		s.Session.Baseline = p.Offsets
		for i := range s.Cores {
			if i < len(p.Offsets) {
				s.Cores[i].Baseline = new(p.Offsets[i])
			}
		}
	case *TunerDecision:
		s.decided(p.Core, e)
	case *CorePhase:
		s.decided(p.Core, e)
	case *DeadEnd:
		if p.Core != nil {
			s.decided(*p.Core, e)
		}
		s.DeadEnd = &DeadEndRef{Condition: p.Condition, Seq: e.Seq}
	case *ConfigLoaded:
		s.DeadEnd = nil
	case *SMUIntent:
		s.open = slices.DeleteFunc(s.open, func(o openIntent) bool { return o.Kind == KindSMUIntent })
		s.open = append(s.open, openIntent{Seq: e.Seq, Kind: e.Kind, Msg: e.Msg, boot: e.Boot})
	case *SMUWrite, *SMUError:
		s.open = slices.DeleteFunc(s.open, func(o openIntent) bool { return o.Kind == KindSMUIntent })
	case *TrialIntent:
		s.open = append(s.open, openIntent{Seq: e.Seq, Kind: e.Kind, Msg: e.Msg, boot: e.Boot, trial: p.Trial})
	case *TrialEnd:
		s.open = slices.DeleteFunc(s.open, func(o openIntent) bool { return o.Kind == KindTrialIntent && o.trial == p.Trial })
	case *CrashDetected:
		s.open = slices.DeleteFunc(s.open, func(o openIntent) bool { return o.boot == p.PreviousBoot })
	}
	s.InFlight = nil
	if n := len(s.open); n > 0 {
		last := s.open[n-1].InFlight
		s.InFlight = &last
	}
}

func (s *State) decided(core int, e Event) {
	for i := range s.Cores {
		if s.Cores[i].Core == core {
			s.Cores[i].LastDecision = &DecisionRef{Seq: e.Seq, Msg: e.Msg}
		}
	}
}

func (j *Journal) WriteState(s State) error {
	if err := writeState(j.dir, s, j.opts.Sync); err != nil {
		return fmt.Errorf("write state %s: %w", j.dir, err)
	}
	return nil
}

func writeState(dir string, s State, sync bool) error {
	data, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return err
	}
	data = append(data, '\n')
	tmp := filepath.Join(dir, stateTmpFile)
	f, err := os.OpenFile(tmp, os.O_CREATE|os.O_TRUNC|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	if _, err := f.Write(data); err != nil {
		f.Close()
		return err
	}
	if sync {
		if err := f.Sync(); err != nil {
			f.Close()
			return err
		}
	}
	if err := f.Close(); err != nil {
		return err
	}
	if err := os.Rename(tmp, filepath.Join(dir, stateFile)); err != nil {
		return err
	}
	if !sync {
		return nil
	}
	return syncDir(dir)
}

func syncDir(dir string) error {
	d, err := os.Open(dir)
	if err != nil {
		return err
	}
	defer d.Close()
	return d.Sync()
}

func ReadState(dir string) (State, error) {
	data, err := os.ReadFile(filepath.Join(dir, stateFile))
	if err != nil {
		return State{}, fmt.Errorf("read state %s: %w", dir, err)
	}
	var s State
	if err := json.Unmarshal(data, &s); err != nil {
		return State{}, fmt.Errorf("read state %s: %w", dir, err)
	}
	return s, nil
}

func (j *Journal) ReadState() (State, error) {
	return ReadState(j.dir)
}

func StateFields() []string {
	return slices.Sorted(maps.Keys(fields(State{})))
}

func DiffFields(a, b State) []string {
	ma, mb := fields(a), fields(b)
	var diff []string
	for _, k := range slices.Sorted(maps.Keys(ma)) {
		if !bytes.Equal(ma[k], mb[k]) {
			diff = append(diff, k)
		}
	}
	return diff
}

func fields(s State) map[string]json.RawMessage {
	data, err := json.Marshal(s)
	if err != nil {
		panic(fmt.Sprintf("marshal state: %v", err))
	}
	var m map[string]json.RawMessage
	if err := json.Unmarshal(data, &m); err != nil {
		panic(fmt.Sprintf("unmarshal state: %v", err))
	}
	return m
}
