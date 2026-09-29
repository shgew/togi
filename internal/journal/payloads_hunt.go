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
	Anchor     []int          `json:"anchor"`
	AnchorSeq  int            `json:"anchor_seq"`
	Candidates []int          `json:"candidates"`
	Starts     int            `json:"starts"`
	StartS     int            `json:"start_s"`
	Miss       float64        `json:"miss"`
	Rate       float64        `json:"rate"`
	Ranking    []int          `json:"ranking"`
}

func (*HuntStart) Kind() Kind { return KindHuntStart }
func (p *HuntStart) Message() string {
	source := fmt.Sprintf("resident %s trial %s", p.Regime, p.Trial)
	if p.Trial == "" {
		source = "idle"
	}
	anchor := "all-zero"
	if p.AnchorSeq != 0 {
		anchor = fmt.Sprintf("rotation end #%d", p.AnchorSeq)
	}
	return fmt.Sprintf("hunt %d: unattributed crash in %s; anchor from %s; candidates %s; masks of %d × %ds", p.Hunt, source, anchor, coreList(p.Candidates), p.Starts, p.StartS)
}

type HuntMask struct {
	Hunt        int    `json:"hunt"`
	Mask        int    `json:"mask"`
	Cores       []int  `json:"cores"`
	Profile     []int  `json:"profile"`
	Set         []int  `json:"set"`
	Granularity int    `json:"granularity"`
	Stage       string `json:"stage"`
	Index       int    `json:"index"`
	DurationS   int    `json:"duration_s"`
	Escalated   bool   `json:"escalated,omitempty"`
	FullChecked bool   `json:"full_checked,omitempty"`
	AnyFailed   bool   `json:"any_failed,omitempty"`
	Inferred    string `json:"inferred,omitempty"`
	Skipped     bool   `json:"skipped,omitempty"`
	Reason      string `json:"reason"`
}

func (*HuntMask) Kind() Kind { return KindHuntMask }
func (p *HuntMask) Message() string {
	return fmt.Sprintf("hunt %d mask %d: cores %s at failing offsets, the rest at the anchor; %ds starts", p.Hunt, p.Mask, coreList(p.Cores), p.DurationS)
}

type HuntEnd struct {
	Hunt   int    `json:"hunt"`
	Result string `json:"result"`
	Cores  []int  `json:"cores,omitempty"`
	Masks  int    `json:"masks"`
	Reason string `json:"reason"`
}

func (*HuntEnd) Kind() Kind { return KindHuntEnd }
func (p *HuntEnd) Message() string {
	masks := fmt.Sprintf("%d mask", p.Masks)
	if p.Masks != 1 {
		masks += "s"
	}
	switch {
	case p.Result == "direct" && len(p.Cores) == 1:
		return fmt.Sprintf("hunt %d ended after %s: core %s was attributed directly (%s)", p.Hunt, masks, coreID(p.Cores[0]), p.Reason)
	case len(p.Cores) == 1:
		return fmt.Sprintf("hunt %d found core %s after %s", p.Hunt, coreID(p.Cores[0]), masks)
	}
	return fmt.Sprintf("hunt %d %s: cores %s after %s (%s)", p.Hunt, p.Result, coreList(p.Cores), masks, p.Reason)
}

type HuntSkipped struct {
	Failure int    `json:"failure"`
	Reason  string `json:"reason"`
}

func (*HuntSkipped) Kind() Kind { return KindHuntSkipped }
func (p *HuntSkipped) Message() string {
	return fmt.Sprintf("failure #%d is not hunted: %s", p.Failure, p.Reason)
}

type JointMember struct {
	Core   int `json:"core"`
	Offset int `json:"offset"`
}
type MarkJoint struct {
	Mark     int           `json:"mark"`
	Members  []JointMember `json:"members"`
	Fallback bool          `json:"fallback,omitempty"`
	Hunt     int           `json:"hunt"`
	Reason   string        `json:"reason"`
}

func (*MarkJoint) Kind() Kind { return KindMarkJoint }
func (p *MarkJoint) Message() string {
	members := make([]string, len(p.Members))
	for i, m := range p.Members {
		members[i] = fmt.Sprintf("core %s %d", coreID(m.Core), m.Offset)
	}
	suffix := fmt.Sprintf("observed in hunt %d", p.Hunt)
	if p.Fallback {
		suffix = fmt.Sprintf("fallback over every candidate of hunt %d", p.Hunt)
	}
	return fmt.Sprintf("joint mark J%d: %s, %s", p.Mark, strings.Join(members, " + "), suffix)
}

type RefineRound struct {
	Round     int           `json:"round"`
	Event     RotationEvent `json:"event"`
	Anchor    []int         `json:"anchor,omitempty"`
	AnchorSeq int           `json:"anchor_seq,omitempty"`
	Target    []int         `json:"target,omitempty"`
	Profile   []int         `json:"profile,omitempty"`
	Cores     []int         `json:"cores,omitempty"`
	Ranking   []int         `json:"ranking,omitempty"`
	Starts    int           `json:"starts,omitempty"`
	StartS    int           `json:"start_s,omitempty"`
	Passed    bool          `json:"passed,omitempty"`
	Reason    string        `json:"reason,omitempty"`
}

func (*RefineRound) Kind() Kind { return KindRefineRound }
func (p *RefineRound) Message() string {
	if p.Event == RotationStart {
		if len(p.Cores) == 1 {
			return fmt.Sprintf("refine round %d start: core %s toward %v", p.Round, coreID(p.Cores[0]), p.Target)
		}
		return fmt.Sprintf("refine round %d start: %d cores toward %v", p.Round, len(p.Cores), p.Target)
	}
	if p.Passed {
		return fmt.Sprintf("refine round %d end: passed", p.Round)
	}
	return fmt.Sprintf("refine round %d end: %s", p.Round, p.Reason)
}

type TunerWarning struct {
	Warning string `json:"warning"`
	Trial   string `json:"trial"`
	Passes  []int  `json:"passes"`
	Detail  string `json:"detail"`
}

func (*TunerWarning) Kind() Kind { return KindTunerWarning }
func (p *TunerWarning) Message() string {
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
