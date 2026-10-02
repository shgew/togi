package main

import (
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type trial struct {
	intent       *journal.TrialIntent
	seq, endSeq  int
	time         time.Time
	lastEvidence time.Time
	cause        []int
	crashed      bool
	boot         string
	started      bool
	end          *journal.TrialEnd
	key          string
}
type runInfo struct {
	start, end      time.Time
	builds          map[string]int
	trials, crashes int
	ending          string
	boot            string
}
type huntInfo struct {
	start      *journal.HuntStart
	time, end  time.Time
	seq        int
	result     *journal.HuntEnd
	commitment string
	masks      []*maskInfo
	trials     []*trial
}
type maskInfo struct {
	plan   *journal.HuntMask
	trials []*trial
}
type projection struct {
	trials   []*trial
	byID     map[string]*trial
	idle     []journal.Event
	runs     []*runInfo
	hunts    []*huntInfo
	huntByID map[int]*huntInfo
	boots    map[string]bootTime
	nextBoot map[string]string
	cores    []machine.CoreInfo
}

type bootTime struct {
	start      time.Time
	firstEvent time.Time
}

func project(session facts.Session) *projection {
	events := session.Events
	trials := make(map[int]*facts.Trial, len(session.Trials))
	for _, t := range session.Trials {
		trials[t.Seq] = t
	}
	p := &projection{byID: map[string]*trial{}, huntByID: map[int]*huntInfo{}, boots: map[string]bootTime{}, nextBoot: map[string]string{}}
	var active *runInfo
	commitments := map[int]*huntInfo{}
	var previousBoot string
	masks := map[[2]int]*maskInfo{}
	for _, e := range events {
		if _, ok := p.boots[e.Boot]; !ok {
			p.boots[e.Boot] = bootTime{start: e.Time.Add(-time.Duration(e.Mono) * time.Millisecond), firstEvent: e.Time}
			if previousBoot != "" {
				p.nextBoot[previousBoot] = e.Boot
			}
			previousBoot = e.Boot
		}
		if active != nil {
			active.end = e.Time
		}
		switch v := e.Data.(type) {
		case *journal.SessionStart:
			p.cores = v.Cores
		case *journal.ConfigLoaded:
			// A new boot without shutdown is the continuation of the interrupted run.
			if active == nil || active.boot == e.Boot {
				active = &runInfo{start: e.Time, end: e.Time, boot: e.Boot, builds: map[string]int{}, ending: "open"}
				p.runs = append(p.runs, active)
			}
			active.boot = e.Boot
			active.builds[buildName(v.Build)]++
		case *journal.Shutdown:
			if active != nil {
				active.ending = string(v.Reason)
				active.end = e.Time
				active = nil
			}
		case *journal.TrialIntent:
			p.addTrial(trials[e.Seq], active, masks)
		case *journal.CrashDetected:
			if active != nil {
				active.crashes++
			}
		case *journal.Failure:
			if v.KnownFailure == 0 && v.Trial == "" {
				p.idle = append(p.idle, e)
			}
		case *journal.HuntStart:
			h := &huntInfo{start: v, time: e.Time, seq: e.Seq, commitment: "none"}
			p.hunts = append(p.hunts, h)
			p.huntByID[v.Hunt] = h
		case *journal.HuntMask:
			m := &maskInfo{plan: v}
			masks[[2]int{v.Hunt, v.Mask}] = m
			if h := p.huntByID[v.Hunt]; h != nil {
				h.masks = append(h.masks, m)
			}
		case *journal.HuntEnd:
			if h := p.huntByID[v.Hunt]; h != nil {
				h.end = e.Time
				h.result = v
				commitments[e.Seq] = h
			}
		case *journal.MarkJoint:
			if h := p.huntByID[v.Hunt]; h != nil {
				commitments[e.Seq] = h
			}
		case *journal.TunerDecision:
			recordCommitment(e, v, commitments)
		}
	}
	if len(events) > 0 {
		p.finishHunts(events[len(events)-1].Time)
	}
	return p
}

func recordCommitment(e journal.Event, v *journal.TunerDecision, commitments map[int]*huntInfo) {
	if v.Decision != journal.Backoff {
		return
	}
	for _, cause := range e.Cause {
		if h := commitments[cause]; h != nil {
			h.commitment = fmt.Sprintf("%02d %d->%d", v.Core, v.FromOffset, v.ToOffset)
			return
		}
	}
}

func (p *projection) finishHunts(last time.Time) {
	for _, h := range p.hunts {
		if h.end.IsZero() {
			h.end = last
		}
	}
}

func (p *projection) addTrial(record *facts.Trial, active *runInfo, masks map[[2]int]*maskInfo) {
	v := record.Intent
	t := &trial{intent: v, key: class(v, p.cores), seq: record.Seq, endSeq: record.EndSeq, time: record.Time, lastEvidence: record.LastEvidence, cause: record.Cause, boot: record.Boot, started: record.Started, crashed: record.Crashed, end: record.End}
	p.trials = append(p.trials, t)
	p.byID[v.Trial] = t
	if active != nil {
		active.trials++
	}
	if h := p.huntByID[v.Hunt]; h != nil {
		h.trials = append(h.trials, t)
	}
	if m := masks[[2]int{v.Hunt, v.Mask}]; m != nil {
		m.trials = append(m.trials, t)
	}
}

func buildName(b journal.Build) string {
	if b.Version == "" {
		return "unstamped"
	}
	if b.Rev == "" {
		return b.Version
	}
	return b.Version + "+" + b.Rev
}
func loaded(t *journal.TrialIntent, cores []machine.CoreInfo) []int {
	c := slices.Clone(t.Cores)
	if len(c) == 0 && t.Core != nil {
		c = []int{*t.Core}
	}
	if len(c) == 0 && t.Regime.AllCores() {
		for _, core := range cores {
			c = append(c, core.Core)
		}
	}
	slices.Sort(c)
	return c
}
func coreList(cores []int) string {
	a := make([]string, len(cores))
	for i, c := range cores {
		a[i] = fmt.Sprintf("%02d", c)
	}
	return strings.Join(a, ",")
}
func class(t *journal.TrialIntent, cores []machine.CoreInfo) string {
	return fmt.Sprintf("%s/%s/%s/%ds", t.Regime, t.Workload, coreList(loaded(t, cores)), t.DurationS)
}

// compareProfile compares every profile element, including unloaded cores: idle
// offsets are part of the evidence, even though they are not part of its class.
func compareProfile(a, b []int, deeper bool) bool {
	if len(a) != len(b) {
		return false
	}
	for i, v := range a {
		if deeper && v > b[i] || !deeper && v < b[i] {
			return false
		}
	}
	return true
}
func priorPasses(history []*trial, target *journal.TrialIntent, before int, cores []machine.CoreInfo, idle []journal.Event) int {
	k := class(target, cores)
	cutoff := 0
	for _, t := range history {
		if t.end == nil || t.endSeq >= before || t.key != k {
			continue
		}
		if t.end.Outcome == journal.OutcomeFailure && compareProfile(t.intent.Profile, target.Profile, false) {
			cutoff = max(cutoff, t.endSeq)
		}
	}
	if target.Regime == machine.R6 {
		allCores := loaded(target, cores)
		all := len(cores) > 0 && len(allCores) == len(cores)
		for _, c := range cores {
			all = all && slices.Contains(allCores, c.Core)
		}
		if all {
			for _, e := range idle {
				f := e.Data.(*journal.Failure)
				if e.Seq < before && compareProfile(f.Profile, target.Profile, false) {
					cutoff = max(cutoff, e.Seq)
				}
			}
		}
	}
	n := 0
	for _, t := range history {
		if t.end != nil && t.endSeq > cutoff && t.endSeq < before && t.end.Outcome == journal.OutcomePass && t.key == k && compareProfile(t.intent.Profile, target.Profile, true) {
			n++
		}
	}
	return n
}
