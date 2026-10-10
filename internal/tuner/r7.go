package tuner

import (
	"cmp"
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
	req, sources, _ := s.r7RequestOrigins(workload, cores, profile, before)
	return req, sources
}

// r7RequestOrigins returns the loaded cores' requests, the measurements that supplied them, and the cores no
// measurement covers, whose requests their offsets stand in for.
func (s *State) r7RequestOrigins(workload string, cores []int, profile []int, before int) (map[int]float64, []int, []int) {
	exact := s.newestMeasurement(workload, coresKey(cores), before)
	full := map[int]*entry{}
	for _, ccd := range s.ccdIDs(cores) {
		if e := s.newestMeasurement(workload, s.ccdPartKey(ccd), before); e != nil {
			full[ccd] = e
		}
	}
	out := make(map[int]float64, len(cores))
	var sources, byOffset []int
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
		byOffset = append(byOffset, id)
	}
	slices.Sort(sources)
	return out, sources, byOffset
}

type measurementKey struct{ workload, cores string }

// newestMeasurement returns the newest measurement of workload on exactly the cores that key names, recorded at or
// before before when before is positive.
func (s *State) newestMeasurement(workload, key string, before int) *entry {
	positions := s.measurements[measurementKey{workload, key}]
	n := len(positions)
	if before > 0 {
		n, _ = slices.BinarySearchFunc(positions, before+1, func(i, seq int) int { return cmp.Compare(s.r7Measurements[i].seq, seq) })
	}
	for i := n - 1; i >= 0; i-- {
		if e := &s.r7Measurements[positions[i]]; len(e.requests) > 0 {
			return e
		}
	}
	return nil
}

// ccdPartKey keys the session's cores on ccd, in id order.
func (s *State) ccdPartKey(ccd int) string {
	if key, ok := s.ccdKeys[ccd]; ok {
		return key
	}
	var part []int
	for _, id := range s.ids() {
		if s.ccd[id] == ccd {
			part = append(part, id)
		}
	}
	if s.ccdKeys == nil {
		s.ccdKeys = map[int]string{}
	}
	s.ccdKeys[ccd] = coresKey(part)
	return s.ccdKeys[ccd]
}

func (s *State) indexMeasurements() {
	s.measurements = map[measurementKey][]int{}
	for i, e := range s.r7Measurements {
		k := measurementKey{e.class.workload, coresKey(e.cores)}
		s.measurements[k] = append(s.measurements[k], i)
	}
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

// entryTopSources returns a trial's top groups and the measurements that
// ordered them, empty when its own top_requesters or offsets decided.
func (s *State) entryTopSources(e entry) ([]int, []int) {
	if len(e.top) > 0 {
		return e.top, nil
	}
	d := s.derivedOf(e)
	if !d.hasTop {
		d.top, d.hasTop = s.r7TopRequests(e.cores, d.req), true
	}
	return d.top, d.sources
}

// trialRequests returns a trial's own requests, or those derived from
// measurements available as of that trial, never later ones.
func (s *State) trialRequests(e entry) (map[int]float64, []int) {
	d := s.derivedOf(e)
	return d.req, d.sources
}

// derived is what a trial's own requests, or the measurements before it, imply. Later measurements never change it,
// so only a deleted measurement or a new session invalidates it. Callers must not modify its slices or maps.
type derived struct {
	workload       string
	cores, profile []int
	req            map[int]float64
	sources        []int
	top            []int
	hasTop         bool
	targets        []int
	hasTargets     bool
	named, stalled int
}

func (s *State) derivedOf(e entry) *derived {
	if d := s.derivedBySeq[e.seq]; d != nil && d.workload == e.class.workload && slices.Equal(d.cores, e.cores) && slices.Equal(d.profile, e.profile) {
		return d
	}
	d := &derived{workload: e.class.workload, cores: slices.Clone(e.cores), profile: slices.Clone(e.profile)}
	if len(e.requests) > 0 {
		d.req, d.sources = e.requests, []int{e.seq}
	} else {
		d.req, d.sources = s.r7RequestsBefore(e.class.workload, e.cores, e.profile, e.seq)
	}
	if s.derivedBySeq == nil {
		s.derivedBySeq = map[int]*derived{}
	}
	s.derivedBySeq[e.seq] = d
	return d
}

func optionalCore(p *int) int {
	if p == nil {
		return -1
	}
	return *p
}

// failureTargets returns the cores a failure counts against: its named core, or the top groups of the
// affected CCDs. When another affected CCD still has a movable loaded core, a CCD whose loaded cores
// were all at CO 0 in that trial is not affected.
func (s *State) failureTargets(e entry) []int {
	if s.r7NamedCulprit(e) {
		return []int{*e.named}
	}
	d := s.derivedOf(e)
	named, stalled := optionalCore(e.named), optionalCore(e.stalled)
	if d.hasTargets && d.named == named && d.stalled == stalled {
		return d.targets
	}
	top := s.entryTop(e)
	if e.stalled != nil {
		top = slices.DeleteFunc(slices.Clone(top), func(id int) bool { return s.ccd[id] != s.ccd[*e.stalled] })
	}
	if movable := slices.DeleteFunc(slices.Clone(top), func(id int) bool { return !s.r7LoadedMovable(e, s.ccd[id]) }); len(movable) > 0 {
		top = movable
	}
	d.targets, d.hasTargets, d.named, d.stalled = top, true, named, stalled
	return top
}

// r7NamedCulprit reports whether a named failure counts against its named core. A core named at CO 0 on a
// CCD with no loaded core has no top group of its own, so its failure counts as unattributed.
func (s *State) r7NamedCulprit(e entry) bool {
	if e.named == nil {
		return false
	}
	return e.profile[s.index(*e.named)] != 0 || slices.ContainsFunc(e.cores, func(id int) bool { return s.ccd[id] == s.ccd[*e.named] })
}

func (s *State) r7LoadedMovable(e entry, ccd int) bool {
	return slices.ContainsFunc(e.cores, func(id int) bool { return s.ccd[id] == ccd && e.profile[s.index(id)] != 0 })
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
		if loc, ok := s.located[f.seq]; ok && loc.named != nil {
			s.r7Handled[f.seq][*loc.named] = true
		}
		if failed := s.r7FailureEntry(*f); failed != nil {
			for _, other := range s.failureTargets(*failed) {
				if s.r7NamedCulprit(*failed) || s.ccd[other] == s.ccd[id] {
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
	open := s.r7OpenFailures()
	kept := 0
	for j, i := range open {
		a, ok, settled := s.r7FailureDecision(s.pendingFailures[i])
		if !settled {
			open[kept] = i
			kept++
		}
		if ok {
			kept += copy(open[kept:], open[j+1:])
			s.r7Open = open[:kept]
			return a, true
		}
	}
	s.r7Open = open[:kept]
	return Action{}, false
}

// r7FailureDecision returns the decision failure f requires now, if any, and whether f can decide nothing more until
// r7Epoch changes: it is not a live multi-core R7 failure, its located hunt settled it, or every core it counts
// against was handled. Handled cores only accumulate; whatever changes a failure's entry, targets or located hunt
// bumps r7Epoch.
func (s *State) r7FailureDecision(f pendingFailure) (Action, bool, bool) {
	if !s.multiR7(f.class) || !f.carried && f.failure.Condition == machine.Parked {
		return Action{}, false, true
	}
	failed := s.r7FailureEntry(f)
	if failed == nil {
		return Action{}, false, false
	}
	var located *locatedHunt
	if loc, ok := s.located[f.seq]; ok {
		if loc.result != "loaded" {
			return Action{}, false, true
		}
		if loc.named != nil {
			if s.r7Handled[f.seq][*loc.named] {
				return Action{}, false, true
			}
			a, ok := s.r7LocatedNamedDecision(f, loc)
			return a, ok, false
		}
		located = &loc
	}
	targets := s.failureTargets(*failed)
	if !slices.ContainsFunc(targets, func(id int) bool { return !s.r7Handled[f.seq][id] }) {
		return Action{}, false, true
	}
	if located == nil && s.locatable(f) != nil {
		return Action{}, false, false
	}
	for _, id := range targets {
		if s.r7Handled[f.seq][id] {
			continue
		}
		if c := s.core(id); c != nil {
			if a, ok := s.r7CoreDecision(f, *failed, c, located); ok {
				return a, true, false
			}
		}
	}
	return Action{}, false, false
}

// r7OpenFailures lists, in order, the positions in pendingFailures that r7FailureDecision has not found settled since
// r7Epoch last changed.
func (s *State) r7OpenFailures() []int {
	if !s.r7OpenBuilt || s.r7OpenEpoch != s.r7Epoch {
		s.r7Open = s.r7Open[:0]
		for i := range s.pendingFailures {
			s.r7Open = append(s.r7Open, i)
		}
		s.r7OpenEpoch, s.r7OpenBuilt = s.r7Epoch, true
	}
	return s.r7Open
}

// r7LocatedNamedDecision charges the loaded core that a located hunt's group failure named: the hunt ended
// loaded, and that failure decides the move under the named-core rules with its own failed trial, as it would
// have outside the hunt. The decision cites the hunted failure first, so the same move consumes it.
func (s *State) r7LocatedNamedDecision(source pendingFailure, loc locatedHunt) (Action, bool) {
	g := s.failureBySeq(loc.failure)
	c := s.core(*loc.named)
	if g == nil || c == nil {
		return Action{}, false
	}
	failed := s.r7FailureEntry(*g)
	if failed == nil {
		return Action{}, false
	}
	a, ok := s.r7CoreDecision(*g, *failed, c, &loc)
	if ok && a.Kind == Decide {
		a.Cause = append([]int{source.seq}, a.Cause...)
	}
	return a, ok
}

// r7FailureEntry returns a multi-core R7 failure's failed trial. Once its all-zero rerun passed, neither its named
// core nor its stalled core confines it any more: it counts as unattributed against the cores still off CO 0.
func (s *State) r7FailureEntry(f pendingFailure) *entry {
	i := s.failureEntryPos(f.class, f.seq)
	if i < 0 {
		return nil
	}
	e := &s.ledger[f.class][i]
	if r := s.zeroReruns[f.seq]; r != nil && r.passed {
		unconfined := *e
		unconfined.named, unconfined.stalled = nil, nil
		return &unconfined
	}
	return e
}

func (s *State) setFailureIndex(seq int) {
	i := len(s.pendingFailures)
	s.failureIndex[seq] = i
	if s.failureGroups == nil {
		s.failureGroups = map[int][]int{}
	}
	s.failureGroups[i] = append(s.failureGroups[i], seq)
	s.failurePos = nil
	s.r7Epoch++
}

// eachFailureEntry visits the position in class's ledger of every entry that sameR7Failure matches to seq. The ledger is
// ordered by seq.
func (s *State) eachFailureEntry(class trialClass, seq int, visit func(i int)) {
	entries := s.ledger[class]
	find := func(x int) {
		if i, ok := slices.BinarySearchFunc(entries, x, func(e entry, x int) int { return cmp.Compare(e.seq, x) }); ok {
			visit(i)
		}
	}
	find(seq)
	if idx, ok := s.failureIndex[seq]; ok {
		for _, x := range s.failureGroups[idx] {
			if x != seq && s.failureIndex[x] == idx {
				find(x)
			}
		}
	}
}

// failureEntryPos returns the position of the first such entry, or -1. Found positions are memoized until a failure
// regroups seqs or evidence is deleted; ledger appends never move the first entry.
func (s *State) failureEntryPos(class trialClass, seq int) int {
	if p, ok := s.failurePos[seq]; ok && p.class == class {
		return p.pos
	}
	best := -1
	s.eachFailureEntry(class, seq, func(i int) {
		if best < 0 || i < best {
			best = i
		}
	})
	if best >= 0 {
		if s.failurePos == nil {
			s.failurePos = map[int]failurePos{}
		}
		s.failurePos[seq] = failurePos{class, best}
	}
	return best
}

type failurePos struct {
	class trialClass
	pos   int
}

type r7Order struct {
	group   []int
	sources []int
	reason  string
	located string
	rail    float64
	stepped bool
	named   bool
}

func (s *State) r7TargetGroup(failed entry, id int) r7Order {
	named := s.r7NamedCulprit(failed)
	if named {
		if failed.profile[s.index(id)] != 0 {
			return r7Order{group: []int{id}, named: true}
		}
		if top, sources := s.entryTopSources(failed); slices.Contains(top, id) {
			return r7Order{group: []int{id}, named: true, sources: sources}
		}
	}
	req, sources := s.trialRequests(failed)
	part := map[int]float64{}
	for _, core := range failed.cores {
		if s.ccd[core] == s.ccd[id] {
			part[core] = req[core]
		}
	}
	rail, _ := requests.Top(part)
	groups := r7RequestGroups(part, failed.top)
	order := r7Order{sources: sources, rail: rail}
	switch {
	case named:
		order.reason = fmt.Sprintf("named core %02d failed at CO 0 without being a top requester; back off CCD %d's top group instead", id, s.ccd[id])
	case failed.named != nil:
		order.reason = fmt.Sprintf("named core %02d failed at CO 0 on CCD %d, which had no loaded core; count the failure against loaded CCD %d's top group", *failed.named, s.ccd[*failed.named], s.ccd[id])
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

// r7CoreDecision returns the decision a failure requires against target c, citing the located hunt that kept
// it on the loaded cores, if any. It reports none when the core the decision would move already sits shallower
// than in the failed trial: the failure needs no move while that holds, and stays pending in case that core
// returns to its failing offset.
func (s *State) r7CoreDecision(f pendingFailure, failed entry, c *core, located *locatedHunt) (Action, bool) {
	if s.r7Answered(failed, c.id) {
		return Action{}, false
	}
	order := s.r7TargetGroup(failed, c.id)
	var locatedSeqs []int
	if located != nil {
		locatedSeqs = []int{located.end}
		clause := fmt.Sprintf("hunt %d kept the failure on the loaded cores", located.hunt)
		switch {
		case located.named != nil:
			clause = fmt.Sprintf("hunt %d ended loaded after failure #%d named loaded core %02d, which also answers failure #%d", located.hunt, located.failure, *located.named, located.source)
			locatedSeqs = append(locatedSeqs, located.failure)
		case located.failure != 0:
			clause += fmt.Sprintf(" after failure #%d with every unloaded core at CO 0", located.failure)
			locatedSeqs = append(locatedSeqs, located.failure)
		}
		order.located = clause
		if order.reason != "" {
			order.reason += "; "
		}
		order.reason += clause
	}
	if r := s.zeroReruns[f.seq]; r != nil && r.passed {
		locatedSeqs = append(locatedSeqs, r.end)
		if order.reason != "" {
			order.reason += "; "
		}
		order.reason += fmt.Sprintf("the rerun of failure #%d with every core at CO 0 passed (#%d), so it counts as unattributed against the cores off CO 0", f.seq, r.end)
	}
	if len(order.group) == 0 || order.named && failed.profile[s.index(c.id)] == 0 {
		cause := append([]int{f.seq}, locatedSeqs...)
		cause = append(cause, order.sources...)
		dead := s.r7FailedAtZero(failed, c.id, order)
		if located != nil && located.allZero && located.failure != 0 {
			// Every loaded core was at 0, so the failed locate was the all-zero rerun.
			return Action{Kind: Decide, Payload: dead, Cause: cause}, true
		}
		return s.atZero(f, dead, cause)
	}
	if len(order.group) > 1 && s.rankingSeq == 0 {
		return Action{Kind: ReadRanking}, true
	}
	chosen := order.group[0]
	for _, id := range order.group[1:] {
		if s.lowerPreferred(id, chosen) {
			chosen = id
		}
	}
	if s.r7ShallowerThanFailed(failed, chosen) {
		return Action{}, false
	}
	cause := append([]int{f.seq}, locatedSeqs...)
	cause = append(cause, order.sources...)
	if !order.named && s.rankingSeq > 0 {
		cause = append(cause, s.rankingSeq)
	}
	if s.round != nil {
		return Action{Kind: Decide, Payload: &journal.DeepeningRound{Round: s.round.start.Round, Event: journal.CycleEnd, Reason: fmt.Sprintf("R7 failure #%d requires backoff", f.seq)}, Cause: []int{f.seq}}, true
	}
	return s.r7Backoff(f, failed, s.core(chosen), cause, order), true
}

// r7Answered reports, without computing request order, that every core a decision against target id could
// move already sits shallower than in the failed trial. A named core at CO 0 is never answered here: whether
// it dead-ends depends on its trial's request order.
func (s *State) r7Answered(failed entry, id int) bool {
	if s.r7NamedCulprit(failed) {
		return s.r7ShallowerThanFailed(failed, id)
	}
	movable := false
	for _, core := range failed.cores {
		if s.ccd[core] != s.ccd[id] || failed.profile[s.index(core)] == 0 {
			continue
		}
		if !s.r7ShallowerThanFailed(failed, core) {
			return false
		}
		movable = true
	}
	return movable
}

func (s *State) r7ShallowerThanFailed(failed entry, id int) bool {
	c := s.core(id)
	return c != nil && c.offset > failed.profile[s.index(id)]
}

// r7FailedAtZero explains which zero rule ended the session: a named core that
// was top in its trial, or every loaded core of the affected CCD at CO 0.
func (s *State) r7FailedAtZero(failed entry, id int, order r7Order) *journal.DeadEnd {
	dead := failedAtZero(id)
	switch {
	case order.named:
		basis := "its trial's recorded top requesters"
		if len(failed.top) == 0 {
			basis = fmt.Sprintf("request measurements %v", order.sources)
			if len(order.sources) == 0 {
				basis = "offset order (no request telemetry)"
			}
		}
		dead.Detail = fmt.Sprintf("core %02d failed at CO 0 as a top requester of CCD %d by %s; the instability is not caused by Curve Optimizer", id, s.ccd[id], basis)
	case !s.r7NamedCulprit(failed):
		subject := "unattributed R7 failure"
		if failed.named != nil {
			subject = fmt.Sprintf("R7 failure naming core %02d at CO 0 on CCD %d, which had no loaded core,", *failed.named, s.ccd[*failed.named])
		}
		var loaded []int
		for _, core := range failed.cores {
			if s.ccd[core] == s.ccd[id] {
				loaded = append(loaded, core)
			}
		}
		located := ""
		if order.located != "" {
			located = ", and " + order.located
		}
		dead.Detail = fmt.Sprintf("%s counts against CCD %d's top group, and every loaded core of that CCD %v is at CO 0%s; the instability is not caused by Curve Optimizer", subject, s.ccd[id], loaded, located)
	}
	return dead
}

func (s *State) r7Backoff(f pendingFailure, failed entry, c *core, cause []int, order r7Order) Action {
	req, sources, measured := s.r7FailingRequest(failed, c.id)
	counts := 1
	var target float64
	var passSeqs []int
	if measured {
		targetEntry := failed
		if !order.named {
			targetEntry.named = nil
		}
		failingTop := req[c.id]
		if order.stepped {
			failingTop = order.rail
		}
		target, passSeqs = s.r7VoltageTarget(targetEntry, c.id, failingTop)
		if len(passSeqs) > 0 {
			counts = requests.Counts(req[c.id], target)
		}
		if order.stepped {
			counts = max(counts, int(math.Floor((order.rail-req[c.id])/requests.VoltsPerCount+1e-9))+1)
		}
	}
	reason := fmt.Sprintf("voltage-targeted R7 backoff after failure #%d: core %02d ", f.seq, c.id)
	switch {
	case len(sources) == 0:
		reason += fmt.Sprintf("order came from offsets at CO %d; no request telemetry, %s", failed.profile[s.index(c.id)], r7Count(counts, "count"))
	case !measured:
		reason += fmt.Sprintf("order came from offsets at CO %d; request measurements %v do not cover it, %s", failed.profile[s.index(c.id)], sources, r7Count(counts, "count"))
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
	if order.stepped && measured {
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
	to := min(0, max(failed.profile[s.index(c.id)]+counts, fail+1))
	return Action{Kind: Decide, Payload: &journal.TunerDecision{Core: c.id, Phase: journal.PhaseChecking, Decision: journal.Backoff, FromOffset: c.offset, ToOffset: to, Pass: pass, FailurePoint: new(fail), Reason: reason + s.carriedReason(cause)}, Cause: cause}
}

// r7FailingRequest returns the requests that supplied core id's failing request, their measurement sources,
// and whether a measurement covered id rather than its offset standing in.
func (s *State) r7FailingRequest(failed entry, id int) (map[int]float64, []int, bool) {
	if _, ok := failed.requests[id]; ok {
		return failed.requests, []int{failed.seq}, true
	}
	cores := failed.cores
	if len(failed.requests) > 0 || !slices.Contains(cores, id) {
		cores = []int{id}
	}
	req, sources, byOffset := s.r7RequestOrigins(failed.class.workload, cores, failed.profile, failed.seq)
	return req, sources, !slices.Contains(byOffset, id)
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

// targetCandidate collects the qualifying passes at one profile and the lowest
// top request among them.
type targetCandidate struct {
	voltage float64
	minSeq  int
	seqs    []int
}

func (g *targetCandidate) add(voltage float64, seq int) {
	if voltage < g.voltage || voltage == g.voltage && seq < g.minSeq {
		g.voltage, g.minSeq = voltage, seq
	}
	g.seqs = append(g.seqs, seq)
}

// cited returns the earliest n passes, always including the one that set the target.
func (g *targetCandidate) cited(n int) []int {
	seqs := slices.Clone(g.seqs[:n])
	if !slices.Contains(seqs, g.minSeq) {
		seqs[n-1] = g.minSeq
		slices.Sort(seqs)
	}
	return seqs
}

func (s *State) r7VoltageTarget(f entry, id int, request float64) (float64, []int) {
	groups := map[string]*targetCandidate{}
	for class, entries := range s.ledger {
		if class.regime != machine.R7 || class.workload != f.class.workload {
			continue
		}
		for i := range entries {
			e := &entries[i]
			if !e.pass || len(e.requests) == 0 || !slices.Contains(e.cores, id) || !s.current(e) {
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
			if groups[key] == nil {
				groups[key] = &targetCandidate{voltage: voltage, minSeq: e.seq}
			}
			groups[key].add(voltage, e.seq)
		}
	}
	best := math.Inf(1)
	var seqs []int
	first := 0
	for _, g := range groups {
		slices.Sort(g.seqs)
		if len(g.seqs) >= s.n && (g.voltage < best || g.voltage == best && (len(seqs) == 0 || g.seqs[0] < first)) {
			best, first, seqs = g.voltage, g.seqs[0], g.cited(s.n)
		}
	}
	return best, seqs
}

func (s *State) recordR7Measurement(seq int, p *journal.TrialIntent, end *journal.TrialEnd) {
	if p.Regime != machine.R7 || len(end.VoltageRequestsV) == 0 {
		return
	}
	s.r7Measurements = append(s.r7Measurements, entry{seq: seq, class: classOf(p), cores: slices.Clone(p.Cores), profile: slices.Clone(p.Profile), requests: end.VoltageRequestsV, top: end.TopRequesters, clocks: end.CCDMHz})
	if s.measurements == nil {
		s.measurements = map[measurementKey][]int{}
	}
	k := measurementKey{p.Workload, coresKey(p.Cores)}
	s.measurements[k] = append(s.measurements[k], len(s.r7Measurements)-1)
}
