// Package session is the run loop: session start, resume, crash attribution, trials and dead ends.
package session

import (
	"context"
	"errors"
	"fmt"
	"io"
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
	// Rotations is the number of clean qualifying rotations after search and refinement.
	Rotations int
	// Bootloader is set only in the tuning boot, where a dead end hands the next boot back to the normal system.
	Bootloader Bootloader
	// Prompt is nil when stdin or stderr is not a terminal.
	Prompt func(defect.Finding) (bool, error)
	// Defects overrides the binary's entries in tests; nil uses the shipped list.
	Defects []defect.Entry
	// Carry is what a transition carries into a new session; nil otherwise.
	Carry  *carry.Carry
	Stderr io.Writer
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
	condition    machine.Condition
	applied      []int
	fatal        error
	cancelTrial  context.CancelFunc
	kernelWaited int
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
	boundary, err := r.startupBoundary()
	if err != nil {
		return Stop{}, err
	}
	if _, err := r.append(&journal.ConfigLoaded{Build: Build(), KernelBoundary: boundary, Path: r.in.ConfigPath, File: r.in.ConfigFile, Config: r.in.Config}); err != nil {
		return Stop{}, err
	}
	if err := r.recoverCrashes(ctx); err != nil {
		if errors.Is(err, context.Canceled) {
			return r.shutdown(&journal.Shutdown{Reason: journal.ShutdownSignal}, StopSignal)
		}
		return r.afterEvidence(err)
	}
	if stop, err := r.checkDefects(); stop != nil || err != nil {
		return deref(stop), err
	}
	if stop, err := r.checkDeadEnd(); stop != nil || err != nil {
		return deref(stop), err
	}
	if stop, err := r.preflight(ctx); stop != nil || err != nil {
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
	if r.fatal != nil {
		return journal.Event{}, r.fatal
	}
	e, err := r.in.Journal.Append(p, cause...)
	if err != nil {
		return journal.Event{}, r.latch(err)
	}
	r.fold.Fold(e)
	r.state.Fold(e)
	r.tuner.Fold(e)
	r.tuner.Project(&r.state)
	if err := r.in.Journal.WriteState(r.state); err != nil {
		return journal.Event{}, r.latch(err)
	}
	return e, nil
}

func (r *runner) latch(err error) error {
	if errors.Is(err, machine.ErrCrashed) {
		return err
	}
	if r.fatal != nil {
		return r.fatal
	}
	r.fatal = fmt.Errorf("journal write: %w", err)
	if r.cancelTrial != nil {
		r.cancelTrial()
	}
	zeroErr := r.in.Machine.SMU.SetAllOffsets(0)
	status := "readback all 0"
	var problems []string
	ids := r.fold.ids
	if len(r.cores) > 0 {
		ids = make([]int, len(r.cores))
		for i, c := range r.cores {
			ids[i] = c.Core
		}
	}
	for _, core := range ids {
		o, readErr := r.in.Machine.SMU.Offset(core)
		if readErr != nil {
			problems = append(problems, fmt.Sprintf("core %02d unreadable: %v", core, readErr))
		} else if o != 0 {
			problems = append(problems, fmt.Sprintf("core %02d reads %d", core, o))
		}
	}
	if len(problems) > 0 {
		status = "readback: " + strings.Join(problems, ", ")
	}
	if r.in.Stderr != nil {
		if zeroErr != nil {
			fmt.Fprintf(r.in.Stderr, "togi: journal write failed: %v; setting every core to CO 0 without an intent failed: %v (%s)\n", err, zeroErr, status)
		} else {
			fmt.Fprintf(r.in.Stderr, "togi: journal write failed: %v; every core set to CO 0 without an intent (%s)\n", err, status)
		}
	}
	return r.fatal
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
		return r.latch(err)
	}
	_, err = r.append(&journal.StateRebuilt{Fields: fields})
	return err
}

func (r *runner) recoverCrashes(ctx context.Context) error {
	boot := r.in.Boot
	confirmed := false
	for _, crashed := range r.fold.crashedBoots(boot) {
		next := r.fold.nextBoot(crashed, boot)
		own, err := r.readMCEs(ctx, crashed)
		if err != nil {
			return err
		}
		after, err := r.readMCEs(ctx, next)
		if err != nil {
			return err
		}
		reason, err := r.readResetReason(ctx, next, false)
		if err != nil {
			return err
		}
		if !confirmed {
			for _, b := range r.fold.boots {
				if b == next {
					if reason.Kind != "" {
						confirmed = true
					}
					continue
				}
				other, err := r.readResetReason(ctx, b, true)
				if err != nil {
					return err
				}
				if other.Kind != "" {
					confirmed = true
					break
				}
			}
		}
		inTrial := r.fold.open != nil && r.fold.open.boot == crashed
		evidence, err := r.recoveryBootMCEs(crashed, own)
		if err != nil {
			return err
		}
		evidence = evidence || inTrial && r.fold.open.signal != ""
		for _, m := range after {
			if !m.Corrected {
				evidence = true
				if err := r.recordMCE(m, next, false); err != nil {
					return err
				}
			}
		}
		kind := tuner.ClassifyCrash(tuner.CrashFacts{InTrial: inTrial, Applied: r.fold.applied[crashed] != 0, Evidence: evidence, Reason: reason, Confirmed: confirmed})
		detected := &journal.CrashDetected{PreviousBoot: crashed, InFlight: r.fold.lastIntentIn(crashed), Stray: kind == tuner.CrashStray, ResetReason: reason.Kind, ResetReasonRaw: reason.Raw, Inconclusive: kind == tuner.CrashInconclusive || kind == tuner.CrashThermal}
		if !detected.Stray {
			detected.Condition = r.fold.appliedCond[crashed]
		}
		if _, err := r.append(detected, r.fold.recordedFor(crashed, boot)...); err != nil {
			return err
		}
		r.kernelWaited = 0
	}
	if err := r.closeOpenTrial(); err != nil {
		return err
	}
	for _, seq := range slices.Clone(r.fold.pendingIdle) {
		crash := r.eventAt(seq).Data.(*journal.CrashDetected)
		cause := append([]int{seq}, r.fold.recordedFor(crash.PreviousBoot, boot)...)
		profile := slices.Clone(r.fold.registers[crash.PreviousBoot])
		failure := &journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Regime: machine.R6, Condition: crash.Condition, Profile: profile}
		if !slices.Contains(r.fold.uncertain[crash.PreviousBoot], true) {
			if i, ok := tuner.SoleNonzero(profile); ok && i < len(r.cores) {
				failure.Attribution, failure.Core, failure.Offset = journal.Attributed, new(r.cores[i].Core), new(profile[i])
			}
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

func (r *runner) readMCEs(ctx context.Context, boot string) ([]machine.MCE, error) {
	if r.fold.kernelRetries > 0 && r.kernelWaited == 0 {
		if err := r.waitKernelRetry(ctx); err != nil {
			return nil, err
		}
	}
	for {
		mces, err := r.in.Machine.Kernel.MCEs(boot, 0)
		if boot != r.in.Boot && errors.Is(err, machine.ErrBootMissing) {
			return mces, nil
		}
		if err == nil || errors.Is(err, machine.ErrCrashed) {
			return mces, err
		}
		if retryErr := r.retryKernel(ctx, boot, err); retryErr != nil {
			return nil, retryErr
		}
	}
}

func (r *runner) readResetReason(ctx context.Context, boot string, mayBeVacuumed bool) (machine.ResetReason, error) {
	if r.fold.kernelRetries > 0 && r.kernelWaited == 0 {
		if err := r.waitKernelRetry(ctx); err != nil {
			return machine.ResetReason{}, err
		}
	}
	for {
		reason, err := r.in.Machine.Kernel.ResetReason(boot)
		if mayBeVacuumed && errors.Is(err, machine.ErrBootMissing) {
			return machine.ResetReason{}, nil
		}
		if err == nil || errors.Is(err, machine.ErrCrashed) {
			return reason, err
		}
		if retryErr := r.retryKernel(ctx, boot, err); retryErr != nil {
			return machine.ResetReason{}, retryErr
		}
	}
}

func (r *runner) retryKernel(ctx context.Context, boot string, err error) error {
	if r.fold.kernelRetries >= 3 {
		r.fold.kernelDeadDetail = fmt.Sprintf("kernel log of boot %s unreadable after retries at 1, 5 and 30 min: %v", boot, err)
		return errDeadEndEvidence
	}
	wait := []int{60, 300, 1800}[r.fold.kernelRetries]
	if _, appendErr := r.append(&journal.BackendRetry{Backend: "kernel_log", Attempt: r.fold.kernelRetries + 1, WaitS: wait, Reason: err.Error()}); appendErr != nil {
		return appendErr
	}
	return r.waitKernelRetry(ctx)
}

func (r *runner) waitKernelRetry(ctx context.Context) error {
	wait := []int{60, 300, 1800}[r.fold.kernelRetries-1]
	if sleepErr := r.in.Machine.Clock.Sleep(ctx, time.Duration(wait)*time.Second); sleepErr != nil {
		return fmt.Errorf("wait for kernel log retry: %w", sleepErr)
	}
	r.kernelWaited = r.fold.kernelRetries
	return nil
}

func (r *runner) recordMCE(m machine.MCE, fromBoot string, between bool) error {
	if r.fold.mceKeys[mceKey(fromBoot, m.Lines)] {
		return nil
	}
	_, err := r.append(&journal.MCE{CPU: m.CPU, Core: m.Core, Bank: m.Bank, BankType: m.BankType, Corrected: m.Corrected, MonotonicNS: new(int64(m.Monotonic)), FromBoot: fromBoot, BetweenTrials: between, Lines: m.Lines})
	return err
}

func (r *runner) closeOpenTrial() error {
	open := r.fold.open
	if open == nil {
		return nil
	}
	evidence := trialEvidence{
		result:  machine.Result{Ran: open.ran(), Signal: open.signal},
		missing: journal.TrialReasonStoppedDuringTrial,
	}
	cause := []int{open.seq}
	if open.core != nil {
		evidence.result.Core = *open.core
	}
	if len(open.mces) > 0 {
		evidence.mces = []recordedMCE{{corrected: open.corrected}}
		cause = append(cause, open.mces...)
		if open.signal == "" {
			evidence.missing = journal.TrialReasonStoppedAfterMachineCheck
		}
	}
	interrupted := true
	if seq, crashed := r.fold.crashSeq[open.boot]; crashed {
		if open.signal != "" {
			evidence.missing = "backend reported a failure before the reset"
		}
		crash := r.eventAt(seq).Data.(*journal.CrashDetected)
		reset := &journal.TrialEnd{Outcome: journal.OutcomeInconclusive}
		switch {
		case crash.ResetReason == machine.ResetThermalTrip && crash.Inconclusive:
			reset.Reason = "thermal trip during the trial"
		case crash.Inconclusive:
			reset.Reason = "the machine lost power during the trial"
		default:
			reset.Outcome, reset.Signal, reset.Reason = journal.OutcomeFailure, machine.Crash, "machine crashed during the trial"
		}
		evidence.reset = reset
		if open.signal == "" && len(open.mces) == 0 {
			cause = append([]int{seq}, r.fold.recordedFor(open.boot, r.in.Boot)...)
			evidence.missing = ""
			interrupted = reset.Outcome != journal.OutcomeFailure
		}
	}
	evidence.missing = joinDiagnostic(evidence.missing, open.kernelError)
	end := adjudicateTrial(evidence)
	end.Trial, end.Interrupted = open.intent.Trial, interrupted
	end.KernelBoundary = journal.KernelBoundary{KernelCursor: r.fold.kernelCursors[r.in.Boot], KernelError: open.kernelError}
	if open.core == nil {
		end.Core = nil
	}
	_, err := r.append(end, cause...)
	return err
}

func (r *runner) eventAt(seq int) journal.Event {
	return r.in.Journal.Events()[seq-1]
}

func (r *runner) checkDeadEnd() (*Stop, error) {
	f := r.fold
	if f.thermalSeq != 0 {
		return r.deadEnd(&journal.DeadEnd{Condition: journal.DeadEndThermalTrip, Detail: f.thermalDetail}, f.thermalSeq)
	}
	if f.kernelDeadDetail != "" {
		return r.deadEnd(&journal.DeadEnd{Condition: journal.DeadEndNoEvidence, Detail: f.kernelDeadDetail}, f.kernelRetrySeqs...)
	}
	if f.missingSeq != 0 {
		return r.deadEnd(&journal.DeadEnd{Condition: journal.DeadEndNoEvidence, Detail: f.missingDetail}, f.missingSeq)
	}
	if f.smuSeq != 0 {
		return r.deadEnd(&journal.DeadEnd{Condition: journal.DeadEndSMU, Detail: f.smuDetail}, f.smuSeq)
	}
	if f.escapeSeq != 0 {
		return r.deadEnd(&journal.DeadEnd{Condition: journal.DeadEndContainment, Detail: f.escapeDetail}, f.escapeSeq)
	}
	for _, b := range []machine.Backend{machine.Mprime, machine.Ycruncher} {
		if streak := f.streaks[b]; len(streak) >= r.in.Config.DeadEnds.InconclusiveInARow+3 {
			return r.deadEnd(&journal.DeadEnd{Condition: journal.DeadEndNoEvidence, Detail: fmt.Sprintf("backend %s inconclusive %d times in a row after retries at 1, 5 and 30 min", b, len(streak))}, streak...)
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
	boundary, err := r.kernelBoundary("", 0, false)
	if err != nil {
		return nil, err
	}
	if _, err := r.append(&journal.Shutdown{Reason: journal.ShutdownDeadEnd, KernelBoundary: boundary}); err != nil {
		return nil, err
	}
	stop := &Stop{Reason: StopDeadEnd, DeadEnd: d, Reboot: d.Action == journal.ActionClearSavedEntryAndReboot && cleared}
	for _, seq := range e.Cause {
		stop.Evidence = append(stop.Evidence, r.eventAt(seq))
	}
	return stop, nil
}

func (r *runner) preflight(ctx context.Context) (*Stop, error) {
	if r.in.Bootloader != nil {
		if stop, err := r.waitWatchdog(ctx); stop != nil || err != nil {
			return stop, err
		}
	}
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
	// The BIOS context reads through the SMU, which a failed check may make unreachable.
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
		check := false
		if o, ok := r.in.Config.CandidateEdges[c.Core]; ok {
			start, reason, check = o, "configured candidate edge", true
		} else if o, ok := r.in.Config.StartOffsets[c.Core]; ok {
			start, reason = o, "configured start offset"
		} else if has && cc.Edge != nil {
			start, reason, check = *cc.Edge, fmt.Sprintf("candidate edge %d carried from session %s", *cc.Edge, cc.EdgeSession), true
		} else if start != b {
			reason = fmt.Sprintf("baseline %d clamped to %d", b, start)
		}
		p := &journal.CorePhase{Core: c.Core, To: phase, CheckEdge: check}
		if check {
			p.Workloads = []string{machine.Workloads(machine.R1)[0].ID, machine.Workloads(machine.R2)[0].ID}
		}
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

func (r *runner) ensureCondition(t tuner.Trial) error {
	target := make([]int, len(r.cores))
	switch t.Condition {
	case machine.Isolated:
	case machine.Resident:
		target = r.tuner.Profile()
	case machine.Masked:
		target = t.Profile
	default:
		return fmt.Errorf("apply trial condition %s: unknown condition", t.Condition)
	}
	if len(target) != len(r.cores) {
		return fmt.Errorf("trial with a profile of %d offsets for %d cores", len(target), len(r.cores))
	}
	if r.condition == t.Condition && slices.Equal(r.applied, target) {
		return nil
	}
	return r.apply(target, &journal.ProfileApplied{Offsets: target, Condition: t.Condition}, r.tuner.ProfileSeq())
}

func (r *runner) apply(target []int, record journal.Payload, cause int) error {
	var reads []int
	var causes []int
	if cause != 0 {
		causes = []int{cause}
	}
	if r.applied == nil {
		seqs, err := r.setAll(0, causes...)
		if err != nil {
			return err
		}
		reads = append(reads, seqs...)
		r.applied = make([]int, len(r.cores))
	}
	if len(target) != len(r.applied) {
		return fmt.Errorf("apply profile of %d offsets for %d cores", len(target), len(r.applied))
	}
	for _, deeper := range []bool{false, true} {
		for i, c := range r.cores {
			if (target[i] < r.applied[i]) != deeper || target[i] == r.applied[i] {
				continue
			}
			if deeper {
				next := slices.Clone(r.applied)
				next[i] = target[i]
				if mark, reaches := r.tuner.Reaches(next); reaches {
					return fmt.Errorf("refusing to write core %02d to %d: the profile would reach %s", c.Core, target[i], mark)
				}
			}
			seq, err := r.set(c.Core, target[i], causes...)
			if err != nil {
				r.condition = ""
				return err
			}
			r.applied[i] = target[i]
			reads = append(reads, seq)
		}
	}
	_, err := r.append(record, reads...)
	if err == nil {
		if p, ok := record.(*journal.ProfileApplied); ok {
			r.condition = p.Condition
		} else {
			r.condition = ""
		}
	}
	return err
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
			if ctx.Err() != nil {
				return r.shutdown(&journal.Shutdown{Reason: journal.ShutdownSignal}, StopSignal)
			}
			if _, err := r.append(a.Payload, a.Cause...); err != nil {
				return Stop{}, err
			}
		case tuner.ReadRanking:
			if ctx.Err() != nil {
				return r.shutdown(&journal.Shutdown{Reason: journal.ShutdownSignal}, StopSignal)
			}
			values, err := r.in.Machine.Host.Ranking()
			ranking := make([]int, len(r.cores))
			for i, c := range r.cores {
				ranking[i] = c.Core
			}
			detail := ""
			switch {
			case err != nil:
				detail = err.Error()
			case len(values) != len(r.cores):
				detail = fmt.Sprintf("ranking has %d values for %d cores", len(values), len(r.cores))
			case len(values) > 0 && slices.Min(values) == slices.Max(values):
				detail = fmt.Sprintf("every core ranks %d", values[0])
			default:
				value := make(map[int]int, len(r.cores))
				for i, c := range r.cores {
					value[c.Core] = values[i]
				}
				slices.SortFunc(ranking, func(a, b int) int {
					if value[a] != value[b] {
						return value[b] - value[a]
					}
					return a - b
				})
			}
			if _, err := r.append(&journal.HostRanking{Ranking: ranking, Values: values, Detail: detail}); err != nil {
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
			if err := r.retryBackend(ctx, a.Trial); err != nil {
				if errors.Is(err, context.Canceled) {
					return r.shutdown(&journal.Shutdown{Reason: journal.ShutdownSignal}, StopSignal)
				}
				return Stop{}, err
			}
			if err := r.trial(ctx, a); err != nil && !errors.Is(err, errDeadEndEvidence) {
				return Stop{}, err
			}
		}
	}
}

func (r *runner) retryBackend(ctx context.Context, t tuner.Trial) error {
	id := t.Workload
	if id == "" {
		id = machine.PickWorkload(t.Regime, r.fold.index[t.Core][t.Regime]).ID
	}
	w, ok := machine.WorkloadByID(id)
	if !ok {
		return fmt.Errorf("retry backend of unknown workload %s", id)
	}
	n := len(r.fold.streaks[w.Backend])
	k := r.in.Config.DeadEnds.InconclusiveInARow
	if n < k {
		return nil
	}
	attempt := n - k + 1
	if attempt > 3 {
		return nil
	}
	wait := []int{60, 300, 1800}[attempt-1]
	if previous := r.fold.retries[w.Backend]; previous == nil || previous.Attempt != attempt || r.fold.retryFollowed[w.Backend] {
		if _, err := r.append(&journal.BackendRetry{Backend: string(w.Backend), Attempt: attempt, WaitS: wait, Reason: r.fold.lastReason[w.Backend]}); err != nil {
			return err
		}
	}
	if err := r.in.Machine.Clock.Sleep(ctx, time.Duration(wait)*time.Second); err != nil {
		return fmt.Errorf("wait for backend %s retry: %w", w.Backend, err)
	}
	return nil
}

func (r *runner) reachedRotations() bool {
	return r.in.Rotations > 0 && r.tuner.QualifiedRotations() >= r.in.Rotations
}

func (r *runner) shutdown(p *journal.Shutdown, stop StopReason) (Stop, error) {
	if err := r.restore(); err != nil {
		return r.afterEvidence(err)
	}
	boundary, err := r.kernelBoundary("", 0, false)
	if err != nil {
		return Stop{}, err
	}
	p.KernelBoundary = boundary
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
	return r.apply(targets, &journal.ProfileRestored{Offsets: targets}, r.fold.baselineSeq)
}
