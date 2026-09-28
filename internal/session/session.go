// Package session is the run loop: session start, resume, crash attribution, trials and dead ends.
package session

import (
	"context"
	"errors"
	"fmt"
	"maps"
	"slices"
	"strings"
	"time"

	togi "github.com/shgew/togi"
	"github.com/shgew/togi/internal/carry"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

// Build is the build that stamps each session start and resume.
func Build() journal.Build {
	return journal.Build{Version: togi.Version(), Rev: togi.Rev(), Ruleset: tuner.Ruleset, Schema: journal.Schema, Fixes: defect.Fixed()}
}

type Input struct {
	Config     config.Config
	ConfigPath string
	ConfigFile bool
	Boot       string
	Journal    Journal
	Machine    machine.Machine
	// Rotations is the number of clean rotations of one profile after which the run stops; 0 runs guard endlessly.
	Rotations int
	// Bootloader is set only in the tuning boot, where a dead end hands the next boot back to the normal system.
	Bootloader Bootloader
	// Prompt is nil when stdin or stderr is not a terminal.
	Prompt func(defect.Finding) (bool, error)
	// Defects overrides the binary's entries in tests; nil uses the shipped list.
	Defects []defect.Entry
	// Carry is what a transition carries into a new session; nil otherwise.
	Carry *carry.Carry
}

type Bootloader interface {
	ClearSavedEntry() (before, after string, err error)
}

type Journal interface {
	Events() []journal.Event
	Append(p journal.Payload, cause ...int) (journal.Event, error)
	WriteState(s journal.State) error
	ReadState() (journal.State, error)
}

type StopReason string

const (
	StopSignal    StopReason = "signal"
	StopDeadEnd   StopReason = "dead_end"
	StopRotations StopReason = "rotations"
)

type Stop struct {
	Reason   StopReason
	DeadEnd  *journal.DeadEnd
	Evidence []journal.Event
	// Reboot is set when the dead end asks the caller to reboot into the normal system.
	Reboot bool
}

var ErrNoSuchCore = errors.New("no such core")

var errDeadEndEvidence = errors.New("dead-end evidence recorded")

type runner struct {
	in    Input
	cores []machine.CoreInfo
	fold  *fold
	state journal.State
	tuner *tuner.State

	// condition and applied describe what this process last wrote to every core; empty until then.
	condition machine.Condition
	applied   []int
}

func Run(ctx context.Context, in Input) (Stop, error) {
	r := &runner{in: in, fold: newFold(), tuner: tuner.New()}
	stop, err := r.run(ctx)
	if errors.Is(err, errDeadEndEvidence) {
		return Stop{}, errors.New("dead-end evidence recorded without a dead end")
	}
	return stop, err
}

func (r *runner) run(ctx context.Context) (Stop, error) {
	events := r.in.Journal.Events()
	if len(events) > 0 {
		if err := journal.Compatible(journal.BuildOf(events), Build()); err != nil {
			if r.in.Bootloader != nil {
				if _, _, clearErr := r.in.Bootloader.ClearSavedEntry(); clearErr != nil {
					return Stop{}, fmt.Errorf("clear GRUB saved entry after incompatible journal: %w: %w", err, clearErr)
				}
			}
			return Stop{}, err
		}
	}
	journal.Replay(events, r.fold, &r.state, r.tuner)
	r.tuner.Project(&r.state)
	if len(events) > 0 {
		if err := r.checkState(); err != nil {
			return Stop{}, err
		}
	}
	if stop, err := r.resumeDeadEnd(events); stop != nil || err != nil {
		return deref(stop), err
	}
	cores, err := r.in.Machine.Host.Topology()
	if err != nil {
		return Stop{}, fmt.Errorf("read topology: %w", err)
	}
	slices.SortFunc(cores, func(a, b machine.CoreInfo) int { return a.Core - b.Core })
	r.cores = cores
	for _, core := range slices.Sorted(maps.Keys(r.in.Config.StartOffsets)) {
		if r.coreInfo(core) == nil {
			return Stop{}, fmt.Errorf("start offset for core %d: %w", core, ErrNoSuchCore)
		}
	}
	for _, core := range slices.Sorted(maps.Keys(r.in.Config.CandidateEdges)) {
		if r.coreInfo(core) == nil {
			return Stop{}, fmt.Errorf("candidate edge for core %d: %w", core, ErrNoSuchCore)
		}
	}

	if !r.fold.started {
		if _, err := r.append(&journal.SessionStart{Build: Build(), Session: r.in.Machine.Clock.Now().UTC().Format("20060102T150405Z"), Cores: cores}); err != nil {
			return Stop{}, err
		}
	}
	if _, err := r.append(&journal.ConfigLoaded{Build: Build(), Path: r.in.ConfigPath, File: r.in.ConfigFile, Config: r.in.Config}); err != nil {
		return Stop{}, err
	}
	if err := r.recoverCrashes(); err != nil {
		return Stop{}, err
	}
	if stop, err := r.checkDefects(); stop != nil || err != nil {
		return deref(stop), err
	}
	if stop, err := r.checkDeadEnd(); stop != nil || err != nil {
		return deref(stop), err
	}
	if stop, err := r.preflight(); stop != nil || err != nil {
		return deref(stop), err
	}
	if err := r.startSession(); err != nil {
		return r.afterEvidence(err)
	}
	return r.loop(ctx)
}

func deref(s *Stop) Stop {
	if s == nil {
		return Stop{}
	}
	return *s
}

func (r *runner) coreInfo(core int) *machine.CoreInfo {
	for i := range r.cores {
		if r.cores[i].Core == core {
			return &r.cores[i]
		}
	}
	return nil
}

func (r *runner) append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := r.in.Journal.Append(p, cause...)
	if err != nil {
		return journal.Event{}, err
	}
	r.fold.Fold(e)
	r.state.Fold(e)
	r.tuner.Fold(e)
	r.tuner.Project(&r.state)
	if err := r.in.Journal.WriteState(r.state); err != nil {
		return journal.Event{}, err
	}
	return e, nil
}

func (r *runner) checkState() error {
	saved, err := r.in.Journal.ReadState()
	fields := journal.StateFields()
	if err == nil {
		fields = journal.DiffFields(saved, r.state)
	}
	if len(fields) == 0 {
		return nil
	}
	if err := r.in.Journal.WriteState(r.state); err != nil {
		return err
	}
	_, err = r.append(&journal.StateRebuilt{Fields: fields})
	return err
}

func (r *runner) recoverCrashes() error {
	boot := r.in.Boot
	for _, crashed := range r.fold.crashedBoots(boot) {
		next := r.fold.nextBoot(crashed, boot)
		own, err := r.in.Machine.Kernel.MCEs(crashed, time.Time{})
		if err != nil {
			return fmt.Errorf("read kernel log of boot %s: %w", crashed, err)
		}
		after, err := r.in.Machine.Kernel.MCEs(next, time.Time{})
		if err != nil {
			return fmt.Errorf("read kernel log of boot %s: %w", next, err)
		}
		for _, m := range own {
			if err := r.recordMCE(m, crashed); err != nil {
				return err
			}
		}
		for _, m := range after {
			if !m.Corrected {
				if err := r.recordMCE(m, next); err != nil {
					return err
				}
			}
		}
		inTrial := r.fold.open != nil && r.fold.open.boot == crashed
		kind := tuner.ClassifyCrash(inTrial, r.fold.applied[crashed] != 0)
		detected := &journal.CrashDetected{PreviousBoot: crashed, InFlight: r.fold.lastIntentIn(crashed), Stray: kind == tuner.CrashStray}
		if !detected.Stray {
			detected.Condition = r.fold.appliedCond[crashed]
		}
		if _, err := r.append(detected, r.fold.recordedFor(crashed, boot)...); err != nil {
			return err
		}
	}
	if err := r.closeOpenTrial(); err != nil {
		return err
	}
	for _, seq := range slices.Clone(r.fold.pendingIdle) {
		crash := r.eventAt(seq).Data.(*journal.CrashDetected)
		cause := append([]int{seq}, r.fold.recordedFor(crash.PreviousBoot, boot)...)
		failure := &journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed}
		if crash.Condition == machine.Resident {
			failure.Regime, failure.Condition = machine.R6, machine.Resident
		}
		if _, err := r.append(failure, cause...); err != nil {
			return err
		}
	}
	for {
		a, ok := r.tuner.Attribution()
		if !ok {
			return nil
		}
		if _, err := r.append(a.Payload, a.Cause...); err != nil {
			return err
		}
	}
}

func (r *runner) recordMCE(m machine.MCE, fromBoot string) error {
	if r.fold.mceKeys[mceKey(fromBoot, m.Lines)] {
		return nil
	}
	_, err := r.append(&journal.MCE{CPU: m.CPU, Core: m.Core, Bank: m.Bank, BankType: m.BankType, Corrected: m.Corrected, FromBoot: fromBoot, Lines: m.Lines})
	return err
}

func (r *runner) closeOpenTrial() error {
	open := r.fold.open
	if open == nil {
		return nil
	}
	end := &journal.TrialEnd{Trial: open.intent.Trial, Outcome: journal.OutcomeInconclusive, Interrupted: true, Reason: journal.TrialReasonStoppedDuringTrial}
	cause := []int{open.seq}
	if seq, crashed := r.fold.crashSeq[open.boot]; crashed {
		end = &journal.TrialEnd{Trial: open.intent.Trial, Outcome: journal.OutcomeFailure, Signal: machine.Crash, Reason: "machine crashed during the trial"}
		cause = append([]int{seq}, r.fold.recordedFor(open.boot, r.in.Boot)...)
	} else if len(open.mces) > 0 {
		signal := machine.UncorrectedMCE
		if open.corrected {
			signal = machine.CorrectedMCE
		}
		end = &journal.TrialEnd{Trial: open.intent.Trial, Outcome: journal.OutcomeFailure, Signal: signal, Interrupted: true, Reason: journal.TrialReasonStoppedAfterMachineCheck}
		cause = append(cause, open.mces...)
	}
	end.DurationS = int(open.ran().Seconds())
	_, err := r.append(end, cause...)
	return err
}

func (r *runner) eventAt(seq int) journal.Event {
	return r.in.Journal.Events()[seq-1]
}

func (r *runner) checkDeadEnd() (*Stop, error) {
	f := r.fold
	if f.smuSeq != 0 {
		return r.deadEnd(&journal.DeadEnd{Condition: journal.DeadEndSMU, Detail: f.smuDetail}, f.smuSeq)
	}
	if f.escapeSeq != 0 {
		return r.deadEnd(&journal.DeadEnd{Condition: journal.DeadEndContainment, Detail: f.escapeDetail}, f.escapeSeq)
	}
	for _, b := range []machine.Backend{machine.Mprime, machine.Ycruncher} {
		if streak := f.streaks[b]; len(streak) >= r.in.Config.DeadEnds.InconclusiveInARow {
			return r.deadEnd(&journal.DeadEnd{Condition: journal.DeadEndNoEvidence, Detail: fmt.Sprintf("%s inconclusive %d times in a row", b, len(streak))}, streak...)
		}
	}
	if len(f.stray) >= r.in.Config.DeadEnds.StrayCrashesInARow {
		return r.deadEnd(&journal.DeadEnd{Condition: journal.DeadEndBootLoop, Detail: fmt.Sprintf("%d crashes in a row before the profile was applied", len(f.stray))}, f.stray...)
	}
	return nil, nil
}

func (r *runner) afterEvidence(err error) (Stop, error) {
	if !errors.Is(err, errDeadEndEvidence) {
		return Stop{}, err
	}
	stop, err := r.checkDeadEnd()
	if err != nil {
		return Stop{}, err
	}
	if stop == nil {
		return Stop{}, errDeadEndEvidence
	}
	return *stop, nil
}

func (r *runner) resumeDeadEnd(events []journal.Event) (*Stop, error) {
	for i, e := range slices.Backward(events) {
		if _, ok := e.Data.(*journal.DeadEnd); !ok {
			continue
		}
		entry, shutdown := false, false
		for _, next := range events[i+1:] {
			if next.Kind == journal.KindBootSavedEntry && slices.Contains(next.Cause, e.Seq) {
				entry = true
			}
			if next.Kind == journal.KindShutdown && next.Data.(*journal.Shutdown).Reason == journal.ShutdownDeadEnd {
				shutdown = true
			}
		}
		if shutdown {
			return nil, nil
		}
		return r.finishDeadEnd(e, !entry)
	}
	return nil, nil
}

func (r *runner) deadEnd(d *journal.DeadEnd, cause ...int) (*Stop, error) {
	switch {
	case r.in.Bootloader == nil:
		d.Action = journal.ActionExit
	case d.Condition == journal.DeadEndBootLoop:
		d.Action = journal.ActionClearSavedEntryAndReboot
	default:
		d.Action = journal.ActionClearSavedEntry
	}
	cause = slices.Clone(cause)
	e, err := r.append(d, cause...)
	if err != nil {
		return nil, err
	}
	return r.finishDeadEnd(e, true)
}

func (r *runner) finishDeadEnd(e journal.Event, clear bool) (*Stop, error) {
	d := e.Data.(*journal.DeadEnd)
	cleared := d.Action == journal.ActionExit
	switch {
	case d.Action == journal.ActionExit || (clear && r.in.Bootloader == nil):
	case clear:
		before, after, cerr := r.in.Bootloader.ClearSavedEntry()
		entry := &journal.BootSavedEntry{Before: before, After: after}
		if cerr != nil {
			entry.Error = cerr.Error()
		}
		if _, err := r.append(entry, e.Seq); err != nil {
			return nil, err
		}
		cleared = cerr == nil
	default:
		for _, event := range r.in.Journal.Events() {
			if event.Kind == journal.KindBootSavedEntry && slices.Contains(event.Cause, e.Seq) {
				cleared = event.Data.(*journal.BootSavedEntry).Error == ""
			}
		}
	}
	if d.Condition != journal.DeadEndSMU {
		if err := r.restore(); err != nil && !errors.Is(err, errDeadEndEvidence) {
			return nil, err
		}
	}
	if _, err := r.append(&journal.Shutdown{Reason: journal.ShutdownDeadEnd}); err != nil {
		return nil, err
	}
	stop := &Stop{Reason: StopDeadEnd, DeadEnd: d, Reboot: d.Action == journal.ActionClearSavedEntryAndReboot && cleared}
	for _, seq := range e.Cause {
		stop.Evidence = append(stop.Evidence, r.eventAt(seq))
	}
	return stop, nil
}

func (r *runner) preflight() (*Stop, error) {
	var (
		failed []int
		names  []string
	)
	for _, c := range r.in.Machine.Host.Preflight() {
		e, err := r.append(&journal.PreflightCheck{Check: c.Name, Detail: c.Detail, OK: c.OK})
		if err != nil {
			return nil, err
		}
		if !c.OK {
			failed = append(failed, e.Seq)
			names = append(names, fmt.Sprintf("%s (%s)", c.Name, c.Detail))
		}
	}
	// Check 8 reads the BIOS context through the SMU, which a failed check may make unreachable.
	if recorded := r.fold.context; recorded != nil && len(failed) == 0 {
		current, err := r.in.Machine.Host.BIOSContext()
		if err != nil {
			return nil, fmt.Errorf("read BIOS context: %w", err)
		}
		detail, ok := machine.CompareContext(*recorded, current)
		e, err := r.append(&journal.PreflightCheck{Check: "bios_context", Detail: detail, OK: ok})
		if err != nil {
			return nil, err
		}
		if !ok {
			failed = append(failed, e.Seq)
			names = append(names, fmt.Sprintf("bios_context (%s)", detail))
		}
	}
	if len(failed) == 0 {
		return nil, nil
	}
	return r.deadEnd(&journal.DeadEnd{Condition: journal.DeadEndPreflight, Detail: "failed checks: " + strings.Join(names, ", ")}, failed...)
}

func (r *runner) startSession() error {
	if r.fold.context == nil {
		ctx, err := r.in.Machine.Host.BIOSContext()
		if err != nil {
			return fmt.Errorf("read BIOS context: %w", err)
		}
		if _, err := r.append(&journal.SessionContext{BIOSContext: ctx}); err != nil {
			return err
		}
	}
	if r.fold.baselineSeq == 0 {
		offsets := make([]int, len(r.cores))
		var reads []int
		for i, c := range r.cores {
			o, err := r.in.Machine.SMU.Offset(c.Core)
			if err != nil {
				return r.smuFailed(journal.SMURead, new(c.Core), 0, 0, err)
			}
			e, err := r.append(&journal.SMUReadback{Core: c.Core, Offset: o})
			if err != nil {
				return err
			}
			offsets[i] = o
			reads = append(reads, e.Seq)
		}
		if _, err := r.append(&journal.SessionBaseline{Offsets: offsets}, reads...); err != nil {
			return err
		}
	}
	if !r.fold.noticed {
		var nonzero []int
		for i, o := range r.fold.baseline {
			if o != 0 {
				nonzero = append(nonzero, r.cores[i].Core)
			}
		}
		if len(nonzero) > 0 {
			if _, err := r.append(&journal.SessionNotice{Notice: journal.NoticeNonzeroBaseline, Cores: nonzero}, r.fold.baselineSeq); err != nil {
				return err
			}
		}
	}
	if r.in.Carry != nil && r.fold.carriedSeq == 0 && len(r.fold.phase) == 0 {
		if err := r.recordCarry(); err != nil {
			return err
		}
	}
	for i, c := range r.cores {
		if _, ok := r.fold.phase[c.Core]; ok {
			continue
		}
		b := r.fold.baseline[i]
		cc, has := r.fold.carried[c.Core]
		phase, start, reason := journal.PhaseSearch, machine.ClampOffset(b), "baseline"
		if o, ok := r.in.Config.CandidateEdges[c.Core]; ok {
			phase, start, reason = journal.PhaseConfirmation, o, "configured candidate edge"
		} else if o, ok := r.in.Config.StartOffsets[c.Core]; ok {
			start, reason = o, "configured start offset"
		} else if has && cc.Edge != nil {
			phase, start, reason = journal.PhaseConfirmation, *cc.Edge, fmt.Sprintf("candidate edge %d carried from session %s", *cc.Edge, cc.EdgeSession)
		} else if start != b {
			reason = fmt.Sprintf("baseline %d clamped to %d", b, start)
		}
		p := &journal.CorePhase{Core: c.Core, To: phase}
		cause := []int{r.fold.baselineSeq}
		if has {
			cause = append(cause, r.fold.carriedSeq)
		}
		if has && cc.FailedMark != nil {
			m := *cc.FailedMark
			p.FailedMark = new(m)
			if m < 0 && start <= m {
				start = m + 1
				reason += fmt.Sprintf("; clamped to %d, one count shallower than the failed mark %d carried from session %s", start, m, cc.MarkSession)
			}
		}
		p.Offset, p.Reason = start, reason
		if _, err := r.append(p, cause...); err != nil {
			return err
		}
	}
	return nil
}

// recordCarry records what the transition carries into this session: the edges always, the failed marks only when the
// BIOS context matches the archived session's.
func (r *runner) recordCarry() error {
	c := r.in.Carry
	p := &journal.SessionCarried{Sources: c.Sources, Marks: true}
	if c.Context == nil {
		p.Marks, p.Detail = false, "the archived session recorded no BIOS context"
	} else if detail, ok := machine.CompareContext(*c.Context, *r.fold.context); !ok {
		p.Marks, p.Detail = false, detail
	}
	for _, cc := range c.Cores {
		if r.coreInfo(cc.Core) == nil {
			continue
		}
		if !p.Marks {
			cc.FailedMark, cc.MarkSession, cc.MarkSeq, cc.MarkSignal = nil, "", 0, ""
			if cc.Edge == nil {
				continue
			}
		}
		p.Carried = append(p.Carried, cc)
	}
	_, err := r.append(p)
	return err
}

// ensureCondition writes every core for the trial's condition unless this process already did: all at 0 for isolated
// trials, the profile for resident ones. condition is cleared first, so a failed or interrupted write is redone.
func (r *runner) ensureCondition(t tuner.Trial) error {
	switch t.Condition {
	case machine.Isolated:
		if r.condition == machine.Isolated {
			return nil
		}
		r.condition = ""
		reads, err := r.setAll(0)
		if err != nil {
			return err
		}
		zeros := make([]int, len(r.cores))
		if _, err := r.append(&journal.ProfileApplied{Offsets: zeros, Condition: machine.Isolated}, reads...); err != nil {
			return err
		}
		r.condition, r.applied = machine.Isolated, zeros
	case machine.Resident:
		profile := r.tuner.Profile()
		if r.condition == machine.Resident && slices.Equal(r.applied, profile) {
			return nil
		}
		if len(profile) != len(r.cores) {
			return fmt.Errorf("resident trial with a profile of %d offsets for %d cores", len(profile), len(r.cores))
		}
		r.condition = ""
		reads := make([]int, len(r.cores))
		for i, c := range r.cores {
			seq, err := r.set(c.Core, profile[i], r.tuner.ProfileSeq())
			if err != nil {
				return err
			}
			reads[i] = seq
		}
		if _, err := r.append(&journal.ProfileApplied{Offsets: profile, Condition: machine.Resident}, reads...); err != nil {
			return err
		}
		r.condition, r.applied = machine.Resident, profile
	}
	return nil
}

func (r *runner) loop(ctx context.Context) (Stop, error) {
	for {
		if stop, err := r.checkDeadEnd(); stop != nil || err != nil {
			return deref(stop), err
		}
		a := r.tuner.Next()
		switch a.Kind {
		case tuner.Decide:
			if d, ok := a.Payload.(*journal.DeadEnd); ok {
				stop, err := r.deadEnd(d, a.Cause...)
				return deref(stop), err
			}
			if g, ok := a.Payload.(*journal.GuardRotation); ok && g.Event == journal.RotationStart && r.reachedRotations() {
				return r.shutdown(&journal.Shutdown{Reason: journal.ShutdownRotations, Rotations: r.in.Rotations}, StopRotations)
			}
			if d, ok := a.Payload.(*journal.TunerDecision); ok && d.Decision == journal.Regain {
				if ctx.Err() != nil {
					return r.shutdown(&journal.Shutdown{Reason: journal.ShutdownSignal}, StopSignal)
				}
				if r.reachedRotations() {
					return r.shutdown(&journal.Shutdown{Reason: journal.ShutdownRotations, Rotations: r.in.Rotations}, StopRotations)
				}
			}
			if _, err := r.append(a.Payload, a.Cause...); err != nil {
				return Stop{}, err
			}
		case tuner.RunTrial:
			if ctx.Err() != nil {
				return r.shutdown(&journal.Shutdown{Reason: journal.ShutdownSignal}, StopSignal)
			}
			err := r.ensureCondition(a.Trial)
			if errors.Is(err, errDeadEndEvidence) {
				continue
			}
			if err != nil {
				return Stop{}, err
			}
			if err := r.trial(ctx, a); err != nil && !errors.Is(err, errDeadEndEvidence) {
				return Stop{}, err
			}
		}
	}
}

func (r *runner) reachedRotations() bool {
	return r.in.Rotations > 0 && r.tuner.CleanRotations() >= r.in.Rotations
}

func (r *runner) shutdown(p *journal.Shutdown, stop StopReason) (Stop, error) {
	if err := r.restore(); err != nil {
		return r.afterEvidence(err)
	}
	if _, err := r.append(p); err != nil {
		return Stop{}, err
	}
	return Stop{Reason: stop}, nil
}

// restore writes every core back to its baseline once this process has written offsets, so the machine keeps running
// on the values it had before togi started, except that a core never goes deeper than its current offset.
func (r *runner) restore() error {
	if r.applied == nil {
		return nil
	}
	targets := make([]int, len(r.cores))
	for i, c := range r.cores {
		o := r.fold.baseline[i]
		if s := slices.IndexFunc(r.state.Cores, func(s journal.CoreState) bool { return s.Core == c.Core }); s >= 0 {
			o = max(o, r.state.Cores[s].Offset)
		}
		targets[i] = machine.ClampOffset(o)
	}
	if slices.Equal(r.applied, targets) {
		return nil
	}
	var reads []int
	if slices.Min(targets) == slices.Max(targets) {
		seqs, err := r.setAll(targets[0], r.fold.baselineSeq)
		if err != nil {
			return err
		}
		reads = seqs
	} else {
		for i, c := range r.cores {
			seq, err := r.set(c.Core, targets[i], r.fold.baselineSeq)
			if err != nil {
				return err
			}
			reads = append(reads, seq)
		}
	}
	_, err := r.append(&journal.ProfileRestored{Offsets: targets}, reads...)
	return err
}
