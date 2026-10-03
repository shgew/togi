package journal

import (
	"fmt"
	"strings"

	"github.com/shgew/togi/internal/machine"
)

type HostRanking struct {
	Ranking []int  `json:"ranking"`
	Values  []int  `json:"values,omitempty"`
	Detail  string `json:"detail,omitempty"`
}

func (*HostRanking) Kind() Kind { return KindHostRanking }
func (p *HostRanking) Message() string {
	if p.Detail != "" {
		return fmt.Sprintf("preferred-core ranking unavailable (%s); core-id order", p.Detail)
	}
	ids := make([]string, len(p.Ranking))
	for i, core := range p.Ranking {
		ids[i] = coreID(core)
	}
	return "preferred cores " + strings.Join(ids, " ")
}

type HuntStart struct {
	Hunt       int            `json:"hunt"`
	Failure    int            `json:"failure"`
	Trial      string         `json:"trial,omitempty"`
	Regime     machine.Regime `json:"regime"`
	Workload   string         `json:"workload"`
	Cores      []int          `json:"cores"`
	DurationS  int            `json:"duration_s"`
	Failing    []int          `json:"failing"`
	Parked     []int          `json:"parked"`
	ParkedSeq  int            `json:"parked_seq"`
	Candidates []int          `json:"candidates"`
	Starts     int            `json:"starts"`
	StartS     int            `json:"start_s"`
	Miss       float64        `json:"miss"`
	Rate       float64        `json:"rate"`
	Ranking    []int          `json:"ranking"`
	Reason     string         `json:"reason,omitempty"`
}

func (*HuntStart) Kind() Kind { return KindHuntStart }
func (p *HuntStart) Message() string {
	source := fmt.Sprintf("together %s trial %s", p.Regime, p.Trial)
	if p.Trial == "" {
		source = "idle"
	}
	parked := "parked at 0"
	if p.ParkedSeq != 0 {
		parked = fmt.Sprintf("parked offsets from lap end #%d", p.ParkedSeq)
	}
	msg := fmt.Sprintf("hunt %d: unattributed failure in %s; %s; candidates %s; groups of %d × %ds", p.Hunt, source, parked, coreList(p.Candidates), p.Starts, p.StartS)
	if p.Reason != "" {
		msg += "; " + p.Reason
	}
	return msg
}

type HuntGroup struct {
	Hunt        int                 `json:"hunt"`
	Group       int                 `json:"group"`
	Cores       []int               `json:"cores"`
	Profile     []int               `json:"profile"`
	Set         []int               `json:"set"`
	Granularity int                 `json:"granularity"`
	Stage       string              `json:"stage"`
	Index       int                 `json:"index"`
	DurationS   int                 `json:"duration_s"`
	Escalated   bool                `json:"escalated,omitempty"`
	FullChecked bool                `json:"full_checked,omitempty"`
	AnyFailed   bool                `json:"any_failed,omitempty"`
	Probe       *CombinationMember  `json:"probe,omitempty"`
	Held        []CombinationMember `json:"held,omitempty"`
	Inferred    string              `json:"inferred,omitempty"`
	Skipped     bool                `json:"skipped,omitempty"`
	Reason      string              `json:"reason"`
}

func (*HuntGroup) Kind() Kind { return KindHuntGroup }
func (p *HuntGroup) Message() string {
	prefix := fmt.Sprintf("hunt %d group %d: cores %s", p.Hunt, p.Group, coreList(p.Cores))
	running := "at failing offsets, the rest parked"
	if p.Probe != nil {
		prefix = fmt.Sprintf("hunt %d group %d: core %s at %d with %s", p.Hunt, p.Group, coreID(p.Probe.Core), p.Probe.Offset, memberList(p.Held))
		running = "the rest parked"
	}
	if p.Skipped {
		return fmt.Sprintf("%s skipped: %s", prefix, p.Reason)
	}
	if p.Inferred != "" {
		return fmt.Sprintf("%s %s inferred: %s", prefix, p.Inferred, p.Reason)
	}
	if p.Probe != nil {
		prefix += ","
	}
	message := fmt.Sprintf("%s %s; starts of %ds", prefix, running, p.DurationS)
	if p.Reason != "" {
		message += "; " + p.Reason
	}
	return message
}

type HuntEnd struct {
	Hunt    int                 `json:"hunt"`
	Result  string              `json:"result"`
	Cores   []int               `json:"cores,omitempty"`
	Members []CombinationMember `json:"members,omitempty"`
	Groups  int                 `json:"groups"`
	Reason  string              `json:"reason"`
}

func (*HuntEnd) Kind() Kind { return KindHuntEnd }
func (p *HuntEnd) Message() string {
	groups := fmt.Sprintf("%d group", p.Groups)
	if p.Groups != 1 {
		groups += "s"
	}
	switch {
	case p.Result == "direct" && len(p.Cores) == 1:
		return fmt.Sprintf("hunt %d ended after %s: core %s was attributed directly (%s)", p.Hunt, groups, coreID(p.Cores[0]), p.Reason)
	case len(p.Cores) == 1:
		if p.Reason != "" {
			return fmt.Sprintf("hunt %d found core %s after %s (%s)", p.Hunt, coreID(p.Cores[0]), groups, p.Reason)
		}
		return fmt.Sprintf("hunt %d found core %s after %s", p.Hunt, coreID(p.Cores[0]), groups)
	}
	return fmt.Sprintf("hunt %d %s: cores %s after %s (%s)", p.Hunt, p.Result, coreList(p.Cores), groups, p.Reason)
}

type HuntSkipped struct {
	Failure int    `json:"failure"`
	Reason  string `json:"reason"`
}

func (*HuntSkipped) Kind() Kind { return KindHuntSkipped }
func (p *HuntSkipped) Message() string {
	return fmt.Sprintf("failure #%d is not hunted: %s", p.Failure, p.Reason)
}

type CombinationMember struct {
	Core   int `json:"core"`
	Offset int `json:"offset"`
}
type Combination struct {
	Combination int                 `json:"combination"`
	Members     []CombinationMember `json:"members"`
	Fallback    bool                `json:"fallback,omitempty"`
	Hunt        int                 `json:"hunt"`
	Reason      string              `json:"reason"`
}

func (*Combination) Kind() Kind { return KindCombination }
func (p *Combination) Message() string {
	suffix := fmt.Sprintf("observed in hunt %d", p.Hunt)
	if p.Fallback {
		suffix = fmt.Sprintf("fallback over every candidate of hunt %d", p.Hunt)
	}
	return fmt.Sprintf("combination C%d: %s, %s", p.Combination, memberList(p.Members), suffix)
}

func memberList(members []CombinationMember) string {
	parts := make([]string, len(members))
	for i, m := range members {
		parts[i] = fmt.Sprintf("core %s %d", coreID(m.Core), m.Offset)
	}
	return strings.Join(parts, " + ")
}

type DeepeningRound struct {
	Round   int      `json:"round"`
	Event   LapEvent `json:"event"`
	Base    []int    `json:"base,omitempty"`
	BaseSeq int      `json:"base_seq,omitempty"`
	Target  []int    `json:"target,omitempty"`
	Profile []int    `json:"profile,omitempty"`
	Cores   []int    `json:"cores,omitempty"`
	Ranking []int    `json:"ranking,omitempty"`
	Starts  int      `json:"starts,omitempty"`
	StartS  int      `json:"start_s,omitempty"`
	Passed  bool     `json:"passed,omitempty"`
	Reason  string   `json:"reason,omitempty"`
}

func (*DeepeningRound) Kind() Kind { return KindDeepeningRound }
func (p *DeepeningRound) Message() string {
	if p.Event == LapStart {
		if len(p.Cores) == 1 {
			return fmt.Sprintf("deepening round %d start: core %s toward %v%s", p.Round, coreID(p.Cores[0]), p.Target, p.Reason)
		}
		return fmt.Sprintf("deepening round %d start: %d cores toward %v%s", p.Round, len(p.Cores), p.Target, p.Reason)
	}
	if p.Passed {
		return fmt.Sprintf("deepening round %d end: passed%s", p.Round, p.Reason)
	}
	return fmt.Sprintf("deepening round %d end: %s", p.Round, p.Reason)
}

type TunerWarning struct {
	Warning string `json:"warning"`
	Trial   string `json:"trial"`
	Passes  []int  `json:"passes"`
	Detail  string `json:"detail"`
}

func (*TunerWarning) Kind() Kind { return KindTunerWarning }
func (p *TunerWarning) Message() string {
	if p.Detail != "" {
		return fmt.Sprintf("%s: %s", p.Warning, p.Detail)
	}
	if p.Trial == "" {
		return fmt.Sprintf("%s: idle failure on a profile at least as shallow as %d passes in an all-core R6 class", p.Warning, len(p.Passes))
	}
	return fmt.Sprintf("%s: trial %s failed on a profile at least as shallow as %d passes in its class", p.Warning, p.Trial, len(p.Passes))
}

type BackendRetry struct {
	Backend string `json:"backend"`
	Attempt int    `json:"attempt"`
	WaitS   int    `json:"wait_s"`
	Reason  string `json:"reason"`
}

func (*BackendRetry) Kind() Kind { return KindBackendRetry }
func (p *BackendRetry) Message() string {
	return fmt.Sprintf("backend %s: retry %d of 3 after %ds: %s", p.Backend, p.Attempt, p.WaitS, p.Reason)
}
