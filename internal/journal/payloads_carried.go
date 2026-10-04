package journal

import (
	"fmt"
	"time"

	"github.com/shgew/togi/internal/machine"
)

type FactSource struct {
	Session  string    `json:"session"`
	Seq      int       `json:"seq"`
	Build    Build     `json:"build"`
	Trial    string    `json:"trial,omitempty"`
	Time     time.Time `json:"time"`
	Boot     string    `json:"boot"`
	Evidence int       `json:"evidence"`
}

type TrialClass struct {
	Regime    machine.Regime `json:"regime"`
	Workload  string         `json:"workload"`
	Cores     []int          `json:"cores"`
	DurationS int            `json:"duration_s"`
}

type TrialCarried struct {
	Source           FactSource        `json:"source"`
	Class            TrialClass        `json:"class"`
	Condition        machine.Condition `json:"condition"`
	Phase            Phase             `json:"phase,omitempty"`
	Rerun            bool              `json:"rerun,omitempty"`
	RecordOnly       bool              `json:"record_only,omitempty"`
	Profile          []int             `json:"profile"`
	Outcome          Outcome           `json:"outcome"`
	Signal           machine.Signal    `json:"signal,omitempty"`
	DurationS        int               `json:"duration_s"`
	Core             *int              `json:"core,omitempty"`
	StalledCore      *int              `json:"stalled_core,omitempty"`
	VoltageRequestsV map[int]float64   `json:"voltage_requests_v,omitempty"`
	TopRequesters    []int             `json:"top_requesters,omitempty"`
	CCDMHz           map[int]int       `json:"ccd_mhz,omitempty"`
}

func (*TrialCarried) Kind() Kind { return KindTrialCarried }
func (p *TrialCarried) Message() string {
	result := "passed"
	if p.Outcome == OutcomeFailure {
		result = "failed"
		if p.Signal != "" {
			result += " (" + string(p.Signal) + ")"
		}
	}
	if p.RecordOnly {
		result += " record-only"
	}
	return fmt.Sprintf("carried %s of trial %s from session %s (%s): %s %s %s on cores %s %s after %d s at %v (intended %d s, evidence epoch %d)", p.Outcome, p.Source.Trial, p.Source.Session, p.Source.Build.name(), p.Condition, p.Class.Regime, p.Class.Workload, coreList(p.Class.Cores), result, p.DurationS, p.Profile, p.Class.DurationS, p.Source.Evidence)
}

type FailureCarried struct {
	Source FactSource `json:"source"`
	Class  TrialClass `json:"class"`
	Failure
}

func (*FailureCarried) Kind() Kind { return KindFailureCarried }
func (p *FailureCarried) Message() string {
	return fmt.Sprintf("carried idle failure from session %s (%s), source #%d: %s at %v (evidence epoch %d)", p.Source.Session, p.Source.Build.name(), p.Source.Seq, p.Failure.Message(), p.Profile, p.Source.Evidence)
}
