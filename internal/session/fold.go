package session

import (
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

type openIntent struct {
	seq   int
	kind  journal.Kind
	boot  string
	trial string
}

type openTrial struct {
	seq           int
	intent        *journal.TrialIntent
	boot          string
	startSeq      int
	startCPUs     []int
	started       time.Time
	startMono     int64
	lastMono      int64
	signal        machine.Signal
	core          *int
	last          time.Time
	mces          []int
	corrected     bool
	windowStartNS *int64
	targetIntent  int
	targetWrite   int
	kernelError   string
}

// ran is how long the trial is known to have run: from its start to its last recorded event. Nothing is recorded
// between events, so after a crash this is a lower bound.
func (o *openTrial) ran() time.Duration {
	if o.startSeq == 0 {
		return 0
	}
	if o.lastMono != 0 && o.lastMono >= o.startMono {
		return time.Duration(o.lastMono-o.startMono) * time.Millisecond
	}
	return o.last.Sub(o.started)
}

func (o *openTrial) includesMCE(mono *int64) bool {
	if mono == nil {
		return true
	}
	since := o.startMono * int64(time.Millisecond)
	if o.windowStartNS != nil {
		since = *o.windowStartNS
	}
	return *mono >= since
}

type recoveredMCE struct {
	seq         int
	fromBoot    string
	corrected   bool
	monotonicNS *int64
	// claimed is the boot whose crash.detected first cited this MCE; it is no evidence of any other crash.
	claimed string
}

type fold struct {
	started     bool
	context     *machine.BIOSContext
	backends    journal.ConfigBackends
	baselineSeq int
	baseline    []int
	ids         []int
	// ccds maps each core in the session's recorded topology to its CCD.
	ccds       map[int]int
	noticed    bool
	phase      map[int]journal.Phase
	carriedSeq int
	carried    map[int]journal.CarriedCore

	boots            []string
	lastKind         map[string]journal.Kind
	applied          map[string]int
	appliedCond      map[string]machine.Condition
	crashSeq         map[string]int
	appliedMono      map[string]int64
	applying         map[string]bool
	registers        map[string][]int
	uncertain        map[string][]bool
	retries          map[machine.Backend]*journal.BackendRetry
	retryFollowed    map[machine.Backend]bool
	lastReason       map[machine.Backend]string
	missingSeq       int
	missingDetail    string
	thermalSeq       int
	thermalDetail    string
	kernelRetries    int
	kernelRetrySeqs  []int
	kernelDeadDetail string
	kernelCursors    map[string]string
	// huntGroup is set by a hunt.group and stays set through the group's parked profile.applied, parked trial.intent,
	// a restoration and any interruption, because a resumed session may run the same recorded group again without a new
	// hunt.group. A together profile.applied or trial.intent, a profile change or the hunt's end clears it. While it is
	// set, a nonzero write that is not a restoration is the group's parked application and counts as parked from its
	// first write, before profile.applied.
	huntGroup bool

	unmatched   []openIntent
	open        *openTrial
	mceKeys     map[string]bool
	recovered   []recoveredMCE
	pendingIdle []int

	smuSeq       int
	smuDetail    string
	escapeSeq    int
	escapeDetail string
	streaks      map[machine.Backend][]int
	stray        []int

	trials   int
	index    map[int]map[machine.Regime]int
	allIndex map[machine.Regime]int

	// build is journal.BuildOf of the events folded so far, kept current as they arrive; nil until the session start.
	build *journal.Build
	// deadEnd is the latest dead end; deadEndEntry and deadEndShutdown say whether its saved-entry clearing and its
	// shutdown have followed it.
	deadEnd         *journal.Event
	deadEndEntry    bool
	deadEndShutdown bool
}

func newFold() *fold {
	return &fold{
		phase:         map[int]journal.Phase{},
		carried:       map[int]journal.CarriedCore{},
		lastKind:      map[string]journal.Kind{},
		applied:       map[string]int{},
		appliedCond:   map[string]machine.Condition{},
		crashSeq:      map[string]int{},
		appliedMono:   map[string]int64{},
		applying:      map[string]bool{},
		registers:     map[string][]int{},
		uncertain:     map[string][]bool{},
		retries:       map[machine.Backend]*journal.BackendRetry{},
		retryFollowed: map[machine.Backend]bool{},
		lastReason:    map[machine.Backend]string{},
		kernelCursors: map[string]string{},
		mceKeys:       map[string]bool{},
		streaks:       map[machine.Backend][]int{},
		index:         map[int]map[machine.Regime]int{},
		allIndex:      map[machine.Regime]int{},
	}
}

func mceKey(boot string, lines []string) string {
	return boot + "\n" + strings.Join(lines, "\n")
}

func (f *fold) Fold(e journal.Event) {
	if f.deadEnd != nil {
		f.followDeadEnd(e)
	}
	if _, seen := f.lastKind[e.Boot]; !seen {
		f.boots = append(f.boots, e.Boot)
		f.lastKind[e.Boot] = e.Kind
	} else if e.Kind != journal.KindSessionWarning && e.Kind != journal.KindBootLeaveReason {
		f.lastKind[e.Boot] = e.Kind
	}
	armed := f.huntGroup
	switch p := e.Data.(type) {
	case *journal.HuntGroup:
		f.huntGroup = true
	case *journal.ProfileApplied:
		f.huntGroup = f.huntGroup && p.Condition == machine.Parked
	case *journal.TrialIntent:
		f.huntGroup = f.huntGroup && p.Condition == machine.Parked
	case *journal.ProfileChange, *journal.HuntEnd:
		f.huntGroup = false
	}
	switch p := e.Data.(type) {
	case *journal.SessionStart:
		if f.build == nil {
			f.build = new(journal.BuildOf([]journal.Event{e}))
		}
		f.started = true
		f.ids = make([]int, len(p.Cores))
		f.ccds = make(map[int]int, len(p.Cores))
		for i, c := range p.Cores {
			f.ids[i] = c.Core
			f.ccds[c.Core] = c.CCD
		}
		slices.Sort(f.ids)
	case *journal.ConfigLoaded:
		if f.build != nil && p.Version != "" {
			f.build.Version, f.build.Rev, f.build.Fixes = p.Version, p.Rev, p.Fixes
		}
		f.backends = p.Config.Backends
		f.kernelBoundary(e, p.KernelBoundary)
	case *journal.Shutdown:
		f.kernelBoundary(e, p.KernelBoundary)
	case *journal.SessionContext:
		ctx := p.BIOSContext
		f.context = &ctx
	case *journal.SessionBaseline:
		if len(f.registers[e.Boot]) == 0 {
			f.registers[e.Boot] = slices.Clone(p.Offsets)
			f.uncertain[e.Boot] = make([]bool, len(p.Offsets))
		}
		f.baselineSeq, f.baseline = e.Seq, p.Offsets
	case *journal.SessionNotice:
		f.noticed = true
	case *journal.SessionCarried:
		f.carriedSeq = e.Seq
		for _, c := range p.Carried {
			f.carried[c.Core] = c
		}
	case *journal.CorePhase:
		f.phase[p.Core] = p.To
	case *journal.ProfileApplied:
		f.applied[e.Boot] = e.Seq
		f.appliedCond[e.Boot] = p.Condition
		if !f.applying[e.Boot] {
			f.appliedMono[e.Boot] = e.Mono
		}
		delete(f.applying, e.Boot)
		f.stray = nil
	case *journal.ProfileRestored:
		delete(f.applied, e.Boot)
		delete(f.appliedCond, e.Boot)
		delete(f.applying, e.Boot)
	case *journal.SMUIntent:
		f.dropSMUIntent()
		if f.open != nil && f.open.boot == e.Boot && f.open.startSeq == 0 && slices.Contains(e.Cause, f.open.seq) && p.Core != nil && f.open.intent.Core != nil && *p.Core == *f.open.intent.Core && f.open.intent.Offset != nil && p.Offset == *f.open.intent.Offset {
			f.open.targetIntent = e.Seq
		}
		f.unmatched = append(f.unmatched, openIntent{seq: e.Seq, kind: e.Kind, boot: e.Boot})
		if len(f.registers[e.Boot]) == 0 {
			f.registers[e.Boot] = slices.Clone(f.baseline)
			f.uncertain[e.Boot] = make([]bool, len(f.baseline))
		}
		for i := range f.registers[e.Boot] {
			if p.Core != nil && (i >= len(f.ids) || *p.Core != f.ids[i]) {
				continue
			}
			f.registers[e.Boot][i] = min(f.registers[e.Boot][i], p.Offset)
			f.uncertain[e.Boot][i] = true
		}
		if p.Offset != 0 && f.open == nil && !slices.Contains(e.Cause, f.baselineSeq) {
			f.applied[e.Boot] = e.Seq
			f.appliedCond[e.Boot] = machine.Together
			if armed {
				f.appliedCond[e.Boot] = machine.Parked
			}
			if !f.applying[e.Boot] {
				f.appliedMono[e.Boot] = e.Mono
				f.applying[e.Boot] = true
			}
			f.stray = nil
		}
	case *journal.SMUWrite:
		f.dropSMUIntent()
		if f.open != nil && f.open.boot == e.Boot && f.open.targetIntent != 0 && slices.Contains(e.Cause, f.open.targetIntent) {
			f.open.targetWrite = e.Seq
		}
	case *journal.SMUError:
		f.dropSMUIntent()
		f.smuSeq, f.smuDetail = e.Seq, "SMU command failed: "+p.Error
	case *journal.SMUReadback:
		if p.Expected != nil && *p.Expected != p.Offset {
			f.smuSeq, f.smuDetail = e.Seq, fmt.Sprintf("core %02d reads CO %d after writing %d", p.Core, p.Offset, *p.Expected)
		}
		if i := slices.Index(f.ids, p.Core); i >= 0 && i < len(f.registers[e.Boot]) {
			f.registers[e.Boot][i] = p.Offset
			f.uncertain[e.Boot][i] = false
		}
		if f.open != nil && f.open.boot == e.Boot && f.open.startSeq == 0 && f.open.targetWrite != 0 && slices.Contains(e.Cause, f.open.targetWrite) && f.open.intent.Core != nil && p.Core == *f.open.intent.Core && p.Expected != nil && f.open.intent.Offset != nil && *p.Expected == *f.open.intent.Offset && p.Offset == *p.Expected {
			f.open.windowStartNS = new(e.Mono * int64(time.Millisecond))
		}
	case *journal.TrialIntent:
		f.unmatched = append(f.unmatched, openIntent{seq: e.Seq, kind: e.Kind, boot: e.Boot, trial: p.Trial})
		f.trials++
		f.open = &openTrial{seq: e.Seq, intent: p, boot: e.Boot, windowStartNS: new(e.Mono * int64(time.Millisecond))}
		f.kernelBoundary(e, p.KernelBoundary)
		w, _ := machine.WorkloadByID(p.Workload)
		f.retryFollowed[w.Backend] = true
	case *journal.TrialStart:
		if f.open != nil && f.open.intent.Trial == p.Trial {
			f.open.startSeq, f.open.startCPUs = e.Seq, p.CPUs
			f.open.started, f.open.last = e.Time, e.Time
			f.open.startMono, f.open.lastMono = e.Mono, e.Mono
			if p.WindowStartNS != nil {
				f.open.windowStartNS = p.WindowStartNS
			}
		}
	case *journal.TrialProgress:
		f.trialActivity(p.Trial, e.Time, e.Mono)
		if f.open != nil && p.Signal != "" {
			f.open.signal, f.open.core = p.Signal, p.Core
		}
	case *journal.TrialSignal:
		f.trialActivity(p.Trial, e.Time, e.Mono)
	case *journal.TrialSample:
		f.trialActivity(p.Trial, e.Time, e.Mono)
	case *journal.MCE:
		f.recordMCE(e, p)
	case *journal.CrashDetected:
		f.kernelRetries = 0
		f.kernelRetrySeqs = nil
		f.crashSeq[p.PreviousBoot] = e.Seq
		inTrial := f.open != nil && f.open.boot == p.PreviousBoot
		kind := tuner.RecordedCrashKind(p, inTrial)
		if kind == tuner.CrashThermal {
			f.thermalSeq = e.Seq
			f.thermalDetail = fmt.Sprintf("the machine reset on a thermal trip (%s); check cooling before tuning again", p.ResetReasonRaw)
		}
		for i := range f.recovered {
			if m := &f.recovered[i]; m.claimed == "" && slices.Contains(e.Cause, m.seq) {
				m.claimed = p.PreviousBoot
			}
		}
		f.unmatched = slices.DeleteFunc(f.unmatched, func(o openIntent) bool { return o.boot == p.PreviousBoot })
		switch kind {
		case tuner.CrashStray:
			f.stray = append(f.stray, e.Seq)
		case tuner.CrashIdle:
			f.pendingIdle = append(f.pendingIdle, e.Seq)
		case tuner.CrashInTrial, tuner.CrashInconclusive, tuner.CrashThermal:
		}
	case *journal.TrialEnd:
		f.kernelBoundary(e, p.KernelBoundary)
		f.unmatched = slices.DeleteFunc(f.unmatched, func(o openIntent) bool { return o.kind == journal.KindTrialIntent && o.trial == p.Trial })
		if f.open == nil || f.open.intent.Trial != p.Trial {
			return
		}
		f.trialEnded(e, p)
		if p.BackendMissing {
			w, _ := machine.WorkloadByID(f.open.intent.Workload)
			f.missingSeq, f.missingDetail = e.Seq, fmt.Sprintf("backend %s is missing: %s", w.Backend, p.Reason)
		}
		f.open = nil
	case *journal.Failure:
		f.pendingIdle = slices.DeleteFunc(f.pendingIdle, func(seq int) bool { return slices.Contains(e.Cause, seq) })
	case *journal.BackendRetry:
		if p.Backend == "kernel_log" {
			f.kernelRetries++
			f.kernelRetrySeqs = append(f.kernelRetrySeqs, e.Seq)
		} else {
			b := machine.Backend(p.Backend)
			f.retries[b] = p
			f.retryFollowed[b] = false
		}
	case *journal.DeadEnd:
		f.deadEnd, f.deadEndEntry, f.deadEndShutdown = &e, false, false
		switch p.Condition {
		case journal.DeadEndSMU:
			f.smuSeq = 0
		case journal.DeadEndContainment:
			f.escapeSeq = 0
		case journal.DeadEndNoEvidence:
			f.streaks = map[machine.Backend][]int{}
			f.missingSeq = 0
			f.kernelDeadDetail = ""
		case journal.DeadEndThermalTrip:
			f.thermalSeq = 0
		case journal.DeadEndBootLoop:
			f.stray = nil
		case journal.DeadEndFailureAtZero, journal.DeadEndPreflight, journal.DeadEndDefect:
		}
	}
}

func (f *fold) recordMCE(e journal.Event, p *journal.MCE) {
	boot := p.FromBoot
	if boot == "" {
		boot = e.Boot
	}
	f.mceKeys[mceKey(boot, p.Lines)] = true
	if p.BetweenTrials {
		return
	}
	if p.FromBoot != "" {
		f.recovered = append(f.recovered, recoveredMCE{seq: e.Seq, fromBoot: p.FromBoot, corrected: p.Corrected, monotonicNS: p.MonotonicNS})
		return
	}
	if f.open == nil || p.Trial != "" && p.Trial != f.open.intent.Trial {
		return
	}
	if p.MonotonicNS != nil && (boot != f.open.boot || !f.open.includesMCE(p.MonotonicNS)) {
		return
	}
	f.open.mces = append(f.open.mces, e.Seq)
	f.open.corrected = f.open.corrected || p.Corrected
}

func (f *fold) kernelBoundary(e journal.Event, b journal.KernelBoundary) {
	if b.KernelCursor != "" {
		f.kernelCursors[e.Boot] = b.KernelCursor
	}
	if f.open != nil && f.open.boot == e.Boot && b.KernelError != "" {
		f.open.kernelError = joinDiagnostic(f.open.kernelError, b.KernelError)
	}
}

func (f *fold) trialActivity(trial string, at time.Time, mono int64) {
	if f.open != nil && f.open.startSeq != 0 && f.open.intent.Trial == trial {
		f.open.last, f.open.lastMono = at, mono
	}
}

func (f *fold) dropSMUIntent() {
	f.unmatched = slices.DeleteFunc(f.unmatched, func(o openIntent) bool { return o.kind == journal.KindSMUIntent })
}

func (f *fold) trialEnded(e journal.Event, p *journal.TrialEnd) {
	intent := f.open.intent
	w, _ := machine.WorkloadByID(intent.Workload)
	switch p.Outcome {
	case journal.OutcomeInconclusive:
		if !p.Interrupted {
			f.lastReason[w.Backend] = p.Reason
			f.streaks[w.Backend] = append(f.streaks[w.Backend], e.Seq)
		}
	case journal.OutcomePass, journal.OutcomeFailure:
		f.streaks[w.Backend] = nil
		if intent.Core == nil {
			f.allIndex[intent.Regime]++
			break
		}
		if f.index[*intent.Core] == nil {
			f.index[*intent.Core] = map[machine.Regime]int{}
		}
		f.index[*intent.Core][intent.Regime]++
	}
	if len(p.Escaped) > 0 {
		f.escapeSeq = e.Seq
		f.escapeDetail = fmt.Sprintf("backend thread on cpu %s outside allowed cpus %s", cpus(p.Escaped), cpus(f.open.startCPUs))
	}
	if p.ContainmentError != "" {
		f.escapeSeq, f.escapeDetail = e.Seq, p.ContainmentError
	}
}

func cpus(list []int) string {
	s := make([]string, len(list))
	for i, c := range list {
		s[i] = fmt.Sprint(c)
	}
	return strings.Join(s, ",")
}

func (f *fold) crashedBoots(current string) []string {
	var out []string
	for _, b := range f.boots {
		if b == current || f.lastKind[b] == journal.KindShutdown {
			continue
		}
		if _, named := f.crashSeq[b]; named {
			continue
		}
		out = append(out, b)
	}
	return out
}

func (f *fold) nextBoot(boot, current string) string {
	i := slices.Index(f.boots, boot)
	if i >= 0 && i+1 < len(f.boots) {
		return f.boots[i+1]
	}
	return current
}

func (f *fold) recordedFor(boot, current string) []int {
	next := f.nextBoot(boot, current)
	var seqs []int
	for _, m := range f.recovered {
		if m.claimed != "" && m.claimed != boot {
			continue
		}
		if m.fromBoot == boot && f.open != nil && f.open.boot == boot && !f.open.includesMCE(m.monotonicNS) {
			continue
		}
		if m.fromBoot == boot || (m.fromBoot == next && !m.corrected) {
			seqs = append(seqs, m.seq)
		}
	}
	return seqs
}

func (f *fold) lastIntentIn(boot string) *int {
	for _, m := range slices.Backward(f.unmatched) {
		if m.boot == boot {
			return new(m.seq)
		}
	}
	return nil
}

// pendingDeadEnd returns the latest dead end unless a dead-end shutdown followed it, and whether its saved boot entry
// still has to be cleared.
func (f *fold) pendingDeadEnd() (*journal.Event, bool) {
	if f.deadEnd == nil || f.deadEndShutdown {
		return nil, false
	}
	return f.deadEnd, !f.deadEndEntry
}

func (f *fold) followDeadEnd(e journal.Event) {
	if e.Kind == journal.KindBootSavedEntry {
		f.deadEndEntry = f.deadEndEntry || slices.Contains(e.Cause, f.deadEnd.Seq)
	}
	if shutdown, ok := e.Data.(*journal.Shutdown); ok {
		f.deadEndShutdown = f.deadEndShutdown || shutdown.Reason == journal.ShutdownDeadEnd
	}
}
