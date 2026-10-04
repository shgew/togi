package tuner

import (
	"fmt"
	"math"
	"slices"
	"strconv"
	"strings"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/requests"
)

func (s *State) classCores(k trialClass) []int {
	if t, ok := s.classTargets[k.cores]; ok {
		return t.cores
	}
	var out []int
	for text := range strings.FieldsSeq(strings.Trim(k.cores, "[]")) {
		id, err := strconv.Atoi(text)
		if err == nil {
			out = append(out, id)
		}
	}
	return out
}
func (s *State) multiR7(k trialClass) bool { return k.regime == machine.R7 && len(s.classCores(k)) > 1 }

func (s *State) r7Requests(workload string, cores []int, profile []int) (map[int]float64, []int) {
	return s.r7RequestsBefore(workload, cores, profile, 0)
}

func (s *State) r7RequestsBefore(workload string, cores []int, profile []int, before int) (map[int]float64, []int) {
	var exact *entry
	full := map[int]*entry{}
	for i := range s.r7Measurements {
		e := &s.r7Measurements[i]
		if e.class.workload != workload || len(e.requests) == 0 || before > 0 && e.seq > before {
			continue
		}
		if slices.Equal(e.cores, cores) && (exact == nil || e.seq > exact.seq) {
			exact = e
		}
		for _, ccd := range s.ccdIDs(cores) {
			var part []int
			for _, id := range s.ids() {
				if s.ccd[id] == ccd {
					part = append(part, id)
				}
			}
			if slices.Equal(e.cores, part) && (full[ccd] == nil || e.seq > full[ccd].seq) {
				full[ccd] = e
			}
		}
	}
	out := make(map[int]float64, len(cores))
	var sources []int
	for _, id := range cores {
		source := exact
		if source == nil {
			source = full[s.ccd[id]]
		}
		if source != nil {
			if v, ok := source.requests[id]; ok {
				i := s.index(id)
				out[id] = v + requests.VoltsPerCount*float64(profile[i]-source.profile[i])
				if !slices.Contains(sources, source.seq) {
					sources = append(sources, source.seq)
				}
				continue
			}
		}
		out[id] = requests.VoltsPerCount * float64(profile[s.index(id)])
	}
	slices.Sort(sources)
	return out, sources
}
func (s *State) r7Top(workload string, cores []int, profile []int) []int {
	req, _ := s.r7Requests(workload, cores, profile)
	return s.r7TopRequests(cores, req)
}

func (s *State) r7TopRequests(cores []int, req map[int]float64) []int {
	var top []int
	for _, ccd := range s.ccdIDs(cores) {
		part := map[int]float64{}
		for _, id := range cores {
			if s.ccd[id] == ccd {
				part[id] = req[id]
			}
		}
		if groups := requests.Groups(part); len(groups) > 0 {
			top = append(top, groups[0]...)
		}
	}
	slices.Sort(top)
	return top
}
func (s *State) ccdIDs(cores []int) []int {
	var ids []int
	for _, id := range cores {
		if !slices.Contains(ids, s.ccd[id]) {
			ids = append(ids, s.ccd[id])
		}
	}
	slices.Sort(ids)
	return ids
}
func (s *State) entryTop(e entry) []int {
	top, _ := s.entryTopSources(e)
	return top
}

// entryTopSources returns a start's top groups and the measurements that
// ordered them, empty when its own top_requesters or offsets decided.
func (s *State) entryTopSources(e entry) ([]int, []int) {
	if len(e.top) > 0 {
		return e.top, nil
	}
	req, sources := s.startRequests(e)
	return s.r7TopRequests(e.cores, req), sources
}

// startRequests returns a start's own requests, or those derived from
// measurements available as of that start, never later ones.
func (s *State) startRequests(e entry) (map[int]float64, []int) {
	if len(e.requests) > 0 {
		return e.requests, []int{e.seq}
	}
	return s.r7RequestsBefore(e.class.workload, e.cores, e.profile, e.seq)
}
func (s *State) failureTargets(e entry) []int {
	if e.named != nil {
		return []int{*e.named}
	}
	top := s.entryTop(e)
	if e.stalled != nil {
		top = slices.DeleteFunc(slices.Clone(top), func(id int) bool { return s.ccd[id] != s.ccd[*e.stalled] })
	}
	return top
}
func (s *State) consumeR7(ev journal.Event, id int) {
	for _, seq := range ev.Cause {
		f := s.failureBySeq(seq)
		if f == nil || !s.multiR7(f.class) {
			continue
		}
		if s.r7Handled == nil {
			s.r7Handled = map[int]map[int]bool{}
		}
		if s.r7Handled[f.seq] == nil {
			s.r7Handled[f.seq] = map[int]bool{}
		}
		s.r7Handled[f.seq][id] = true
		if failed := s.r7FailureEntry(*f); failed != nil {
			for _, other := range s.failureTargets(*failed) {
				if failed.named != nil || s.ccd[other] == s.ccd[id] {
					s.r7Handled[f.seq][other] = true
				}
			}
		}
		break
	}
}
func (s *State) r7Decision() (Action, bool) {
	a, ok := s.r7PendingDecision()
	a.Cause = uniqueSeqs(a.Cause)
	return a, ok
}
func (s *State) r7PendingDecision() (Action, bool) {
	for _, f := range s.pendingFailures {
		if !s.multiR7(f.class) {
			continue
		}
		failed := s.r7FailureEntry(f)
		if failed == nil {
			continue
		}
		targets := s.failureTargets(*failed)
		for _, id := range targets {
			if s.r7Handled[f.seq][id] {
				continue
			}
			if c := s.core(id); c != nil {
				return s.r7CoreDecision(f, *failed, c), true
			}
		}
	}
	return Action{}, false
}

func (s *State) r7FailureEntry(f pendingFailure) *entry {
	for i := range s.ledger[f.class] {
		e := &s.ledger[f.class][i]
		if s.sameR7Failure(e.seq, f.seq) {
			if e.named == nil {
				e.named = f.failure.Core
			}
			return e
		}
	}
	return nil
}

type r7Order struct {
	group   []int
	sources []int
	reason  string
	rail    float64
	stepped bool
	named   bool
}

func (s *State) r7TargetGroup(failed entry, id int) r7Order {
	if failed.named != nil {
		if failed.profile[s.index(id)] != 0 {
			return r7Order{group: []int{id}, named: true}
		}
		if top, sources := s.entryTopSources(failed); slices.Contains(top, id) {
			return r7Order{group: []int{id}, named: true, sources: sources}
		}
	}
	req, sources := s.startRequests(failed)
	part := map[int]float64{}
	for _, core := range failed.cores {
		if s.ccd[core] == s.ccd[id] {
			part[core] = req[core]
		}
	}
	rail, _ := requests.Top(part)
	groups := r7RequestGroups(part, failed.top)
	order := r7Order{sources: sources, rail: rail}
	if failed.named != nil {
		order.reason = fmt.Sprintf("named core %02d failed at CO 0 without being a top requester; back off CCD %d's top group instead", id, s.ccd[id])
	}
	for i, group := range groups {
		movable := slices.DeleteFunc(slices.Clone(group), func(core int) bool {
			return failed.profile[s.index(core)] == 0
		})
		if len(movable) == 0 {
			continue
		}
		order.group = movable
		if i > 0 {
			order.stepped = true
			if order.reason != "" {
				order.reason += "; "
			}
			order.reason += fmt.Sprintf("CCD %d groups %v are already at CO 0; step down request order to group %v", s.ccd[id], groups[:i], group)
		}
		return order
	}
	return order
}

func r7RequestGroups(part map[int]float64, top []int) [][]int {
	var first []int
	for _, id := range top {
		if _, ok := part[id]; ok {
			first = append(first, id)
			delete(part, id)
		}
	}
	groups := requests.Groups(part)
	if len(first) == 0 {
		return groups
	}
	slices.Sort(first)
	return append([][]int{first}, groups...)
}

func (s *State) r7CoreDecision(f pendingFailure, failed entry, c *core) Action {
	order := s.r7TargetGroup(failed, c.id)
	if len(order.group) == 0 || order.named && failed.profile[s.index(c.id)] == 0 {
		return Action{Kind: Decide, Payload: s.r7FailedAtZero(failed, c.id, order), Cause: append([]int{f.seq}, order.sources...)}
	}
	if len(order.group) > 1 && s.rankingSeq == 0 {
		return Action{Kind: ReadRanking}
	}
	chosen := order.group[0]
	for _, id := range order.group[1:] {
		if s.lowerPreferred(id, chosen) {
			chosen = id
		}
	}
	cause := append([]int{f.seq}, order.sources...)
	if !order.named && s.rankingSeq > 0 {
		cause = append(cause, s.rankingSeq)
	}
	if s.round != nil {
		return Action{Kind: Decide, Payload: &journal.DeepeningRound{Round: s.round.start.Round, Event: journal.LapEnd, Reason: fmt.Sprintf("R7 failure #%d requires backoff", f.seq)}, Cause: []int{f.seq}}
	}
	return s.r7Backoff(f, failed, s.core(chosen), cause, order)
}

// r7FailedAtZero explains which zero rule ended the session: a named core that
// was top in its start, or every loaded core of the affected CCD at CO 0.
func (s *State) r7FailedAtZero(failed entry, id int, order r7Order) *journal.DeadEnd {
	dead := failedAtZero(id)
	switch {
	case order.named:
		basis := "its start's recorded top requesters"
		if len(failed.top) == 0 {
			basis = fmt.Sprintf("request measurements %v", order.sources)
			if len(order.sources) == 0 {
				basis = "offset order (no request telemetry)"
			}
		}
		dead.Detail = fmt.Sprintf("core %02d failed at CO 0 as a top requester of CCD %d by %s; the instability is not caused by Curve Optimizer", id, s.ccd[id], basis)
	case failed.named == nil:
		var loaded []int
		for _, core := range failed.cores {
			if s.ccd[core] == s.ccd[id] {
				loaded = append(loaded, core)
			}
		}
		dead.Detail = fmt.Sprintf("unattributed R7 failure counts against CCD %d's top group, and every loaded core of that CCD %v is at CO 0; the instability is not caused by Curve Optimizer", s.ccd[id], loaded)
	}
	return dead
}

func (s *State) r7Backoff(f pendingFailure, failed entry, c *core, cause []int, order r7Order) Action {
	req, sources := s.startRequests(failed)
	if _, measured := req[c.id]; !measured {
		req, sources = s.r7RequestsBefore(f.class.workload, []int{c.id}, failed.profile, failed.seq)
	}
	targetEntry := failed
	if !order.named {
		targetEntry.named = nil
	}
	failingTop := req[c.id]
	if order.stepped {
		failingTop = order.rail
	}
	target, passSeqs := s.r7VoltageTarget(targetEntry, c.id, failingTop)
	counts := 1
	if len(passSeqs) > 0 {
		counts = requests.Counts(req[c.id], target)
	}
	if order.stepped && len(sources) > 0 {
		counts = max(counts, int(math.Floor((order.rail-req[c.id])/requests.VoltsPerCount+1e-9))+1)
	}
	reason := fmt.Sprintf("voltage-targeted R7 backoff after failure #%d: core %02d ", f.seq, c.id)
	switch {
	case len(sources) == 0:
		reason += fmt.Sprintf("order came from offsets at CO %d; no request telemetry, %s", failed.profile[s.index(c.id)], r7Count(counts, "count"))
	case len(passSeqs) == 0:
		reason += fmt.Sprintf("request %.3f V; no qualifying pass, %s", req[c.id], r7Count(counts, "count"))
	case req[c.id] >= target:
		reason += fmt.Sprintf("request %.3f V already met the passed target %.3f V; %s", req[c.id], target, r7Count(counts, "count"))
	default:
		reason += fmt.Sprintf("request %.3f V to %.3f V, %s", req[c.id], target, r7Count(counts, "count"))
	}
	if order.reason != "" {
		reason += "; " + order.reason
	}
	if order.stepped && len(sources) > 0 {
		reason += fmt.Sprintf("; %s to rise above %.3f V", r7Count(counts, "count"), order.rail)
	}
	cause = append(cause, sources...)
	cause = append(cause, passSeqs...)
	cause = uniqueSeqs(cause)
	fail := failed.profile[s.index(c.id)]
	if c.fail != nil {
		fail = max(fail, *c.fail)
	}
	pass, _ := keepPass(c.pass, fail)
	to := min(0, max(c.offset+1, failed.profile[s.index(c.id)]+counts, fail+1))
	return Action{Kind: Decide, Payload: &journal.TunerDecision{Core: c.id, Phase: journal.PhaseChecking, Decision: journal.Backoff, FromOffset: c.offset, ToOffset: to, Pass: pass, FailurePoint: new(fail), Reason: reason + s.carriedReason(cause)}, Cause: cause}
}

func r7Count(n int, noun string) string {
	if n != 1 {
		noun += "s"
	}
	return fmt.Sprintf("%d %s", n, noun)
}

func uniqueSeqs(seqs []int) []int {
	var unique []int
	for _, seq := range seqs {
		if !slices.Contains(unique, seq) {
			unique = append(unique, seq)
		}
	}
	return unique
}

func (s *State) lowerPreferred(a, b int) bool {
	ra, rb := slices.Index(s.ranking, a), slices.Index(s.ranking, b)
	if ra != rb {
		return ra > rb
	}
	return a > b
}
func (s *State) r7VoltageTarget(f entry, id int, request float64) (float64, []int) {
	type candidate struct {
		voltage float64
		seqs    []int
	}
	groups := map[string]*candidate{}
	for class, entries := range s.ledger {
		if class.regime != machine.R7 || class.workload != f.class.workload {
			continue
		}
		for _, e := range entries {
			if !e.pass || len(e.requests) == 0 || !slices.Contains(e.cores, id) {
				continue
			}
			if f.named == nil && !slices.Equal(e.cores, f.cores) {
				continue
			}
			if f.named != nil {
				if clock, ok := f.clocks[s.ccd[id]]; ok && e.clocks[s.ccd[id]] < clock {
					continue
				}
			}
			req := map[int]float64{}
			for _, core := range e.cores {
				if f.named != nil || s.ccd[core] == s.ccd[id] {
					req[core] = e.requests[core]
				}
			}
			voltage, ok := requests.Top(req)
			if !ok || f.named == nil && voltage <= request {
				continue
			}
			key := fmt.Sprint(e.profile)
			group := groups[key]
			if group == nil {
				group = &candidate{voltage: voltage}
				groups[key] = group
			}
			group.voltage = min(group.voltage, voltage)
			group.seqs = append(group.seqs, e.seq)
		}
	}
	best := math.Inf(1)
	var seqs []int
	for _, g := range groups {
		slices.Sort(g.seqs)
		if len(g.seqs) >= s.n && (g.voltage < best || g.voltage == best && (len(seqs) == 0 || g.seqs[0] < seqs[0])) {
			best = g.voltage
			seqs = slices.Clone(g.seqs[:s.n])
		}
	}
	return best, seqs
}

func (s *State) sameR7Failure(a, b int) bool {
	if a == b {
		return true
	}
	ai, aok := s.failureIndex[a]
	bi, bok := s.failureIndex[b]
	return aok && bok && ai == bi
}
func (s *State) recordR7Measurement(seq int, p *journal.TrialIntent, end *journal.TrialEnd) {
	if p.Regime != machine.R7 || len(end.VoltageRequestsV) == 0 {
		return
	}
	s.r7Measurements = append(s.r7Measurements, entry{seq: seq, class: classOf(p), cores: slices.Clone(p.Cores), profile: slices.Clone(p.Profile), requests: end.VoltageRequestsV, top: end.TopRequesters, clocks: end.CCDMHz})
}
