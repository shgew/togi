package session

import (
	"fmt"
	"slices"
	"strings"
	"time"

	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
)

type openIntent struct {
	seq   int
	kind  journal.Kind
	boot  string
	trial string
}

type openTrial struct {
	seq       int
	intent    *journal.TrialIntent
	boot      string
	startSeq  int
	startCPUs []int
	started   time.Time
	last      time.Time
	mces      []int
	corrected bool
}

// ran is how long the trial is known to have run: from its start to its last recorded event. Only intents are
// fsynced, so after a crash this is a lower bound.
func (o *openTrial) ran() time.Duration {
	if o.startSeq == 0 {
		return 0
	}
	return o.last.Sub(o.started)
}

type recoveredMCE struct {
	seq       int
	fromBoot  string
	corrected bool
	// claimed is the boot whose crash.detected first cited this MCE; it is no evidence of any other crash.
	claimed string
}

type fold struct {
	started     bool
	context     *machine.BIOSContext
	baselineSeq int
	baseline    []int
	noticed     bool
	phase       map[int]journal.Phase

	boots       []string
	lastKind    map[string]journal.Kind
	applied     map[string]int
	appliedCond map[string]machine.Condition
	crashSeq    map[string]int

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
}

func newFold() *fold {
	return &fold{
		phase:       map[int]journal.Phase{},
		lastKind:    map[string]journal.Kind{},
		applied:     map[string]int{},
		appliedCond: map[string]machine.Condition{},
		crashSeq:    map[string]int{},
		mceKeys:     map[string]bool{},
		streaks:     map[machine.Backend][]int{},
		index:       map[int]map[machine.Regime]int{},
		allIndex:    map[machine.Regime]int{},
	}
}

func mceKey(boot string, lines []string) string {
	return boot + "\n" + strings.Join(lines, "\n")
}

func (f *fold) Fold(e journal.Event) {
	if _, seen := f.lastKind[e.Boot]; !seen {
		f.boots = append(f.boots, e.Boot)
	}
	f.lastKind[e.Boot] = e.Kind
	switch p := e.Data.(type) {
	case *journal.SessionStart:
		f.started = true
	case *journal.SessionContext:
		ctx := p.BIOSContext
		f.context = &ctx
	case *journal.SessionBaseline:
		f.baselineSeq, f.baseline = e.Seq, p.Offsets
	case *journal.SessionNotice:
		f.noticed = true
	case *journal.CorePhase:
		f.phase[p.Core] = p.To
	case *journal.ProfileApplied:
		f.applied[e.Boot] = e.Seq
		f.appliedCond[e.Boot] = p.Condition
		f.stray = nil
	case *journal.ProfileRestored:
		delete(f.applied, e.Boot)
		delete(f.appliedCond, e.Boot)
	case *journal.SMUIntent:
		f.dropSMUIntent()
		f.unmatched = append(f.unmatched, openIntent{seq: e.Seq, kind: e.Kind, boot: e.Boot})
		if p.Offset != 0 && f.open == nil && !slices.Contains(e.Cause, f.baselineSeq) {
			f.applied[e.Boot] = e.Seq
			f.appliedCond[e.Boot] = machine.Resident
			f.stray = nil
		}
	case *journal.SMUWrite:
		f.dropSMUIntent()
	case *journal.SMUError:
		f.dropSMUIntent()
		f.smuSeq, f.smuDetail = e.Seq, "SMU command failed: "+p.Error
	case *journal.SMUReadback:
		if p.Expected != nil && *p.Expected != p.Offset {
			f.smuSeq, f.smuDetail = e.Seq, fmt.Sprintf("core %02d reads CO %d after writing %d", p.Core, p.Offset, *p.Expected)
		}
	case *journal.TrialIntent:
		f.unmatched = append(f.unmatched, openIntent{seq: e.Seq, kind: e.Kind, boot: e.Boot, trial: p.Trial})
		f.trials++
		f.open = &openTrial{seq: e.Seq, intent: p, boot: e.Boot}
	case *journal.TrialStart:
		if f.open != nil && f.open.intent.Trial == p.Trial {
			f.open.startSeq, f.open.startCPUs = e.Seq, p.CPUs
			f.open.started, f.open.last = e.Time, e.Time
		}
	case *journal.TrialProgress:
		f.trialActivity(p.Trial, e.Time)
	case *journal.TrialSignal:
		f.trialActivity(p.Trial, e.Time)
	case *journal.TrialSample:
		f.trialActivity(p.Trial, e.Time)
	case *journal.MCE:
		boot := p.FromBoot
		if boot == "" {
			boot = e.Boot
		}
		f.mceKeys[mceKey(boot, p.Lines)] = true
		if p.FromBoot != "" {
			f.recovered = append(f.recovered, recoveredMCE{seq: e.Seq, fromBoot: p.FromBoot, corrected: p.Corrected})
		} else if f.open != nil {
			f.open.mces = append(f.open.mces, e.Seq)
			f.open.corrected = f.open.corrected || p.Corrected
		}
	case *journal.CrashDetected:
		f.crashSeq[p.PreviousBoot] = e.Seq
		for i := range f.recovered {
			if m := &f.recovered[i]; m.claimed == "" && slices.Contains(e.Cause, m.seq) {
				m.claimed = p.PreviousBoot
			}
		}
		f.unmatched = slices.DeleteFunc(f.unmatched, func(o openIntent) bool { return o.boot == p.PreviousBoot })
		inTrial := f.open != nil && f.open.boot == p.PreviousBoot
		if p.Stray {
			f.stray = append(f.stray, e.Seq)
		} else if !inTrial {
			f.pendingIdle = append(f.pendingIdle, e.Seq)
		}
	case *journal.TrialEnd:
		f.unmatched = slices.DeleteFunc(f.unmatched, func(o openIntent) bool { return o.kind == journal.KindTrialIntent && o.trial == p.Trial })
		if f.open == nil || f.open.intent.Trial != p.Trial {
			return
		}
		f.trialEnded(e, p)
		f.open = nil
	case *journal.Failure:
		if p.Attribution == journal.Unattributed {
			f.pendingIdle = slices.DeleteFunc(f.pendingIdle, func(seq int) bool { return slices.Contains(e.Cause, seq) })
		}
	case *journal.DeadEnd:
		switch p.Condition {
		case journal.DeadEndSMU:
			f.smuSeq = 0
		case journal.DeadEndContainment:
			f.escapeSeq = 0
		case journal.DeadEndNoEvidence:
			f.streaks = map[machine.Backend][]int{}
		case journal.DeadEndBootLoop:
			f.stray = nil
		case journal.DeadEndFailureAtZero, journal.DeadEndPreflight:
		}
	}
}

func (f *fold) trialActivity(trial string, at time.Time) {
	if f.open != nil && f.open.startSeq != 0 && f.open.intent.Trial == trial {
		f.open.last = at
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
		if m.fromBoot == boot || (m.fromBoot == next && !m.corrected) {
			seqs = append(seqs, m.seq)
		}
	}
	return seqs
}

func (f *fold) lastIntentIn(boot string) *int {
	for i := len(f.unmatched) - 1; i >= 0; i-- {
		if f.unmatched[i].boot == boot {
			return new(f.unmatched[i].seq)
		}
	}
	return nil
}
