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
	"github.com/shgew/togi/internal/tuningboot"
)

// Build is the build that stamps each session start and resume.
func Build() journal.Build {
	return journal.Build{Version: togi.Version(), Rev: togi.Rev(), Ruleset: tuner.Ruleset, Schema: journal.Schema, Fixes: defect.Fixed(), EvidenceEpoch: tuner.EvidenceEpoch}
}

type Input struct {
	Config     config.Config
	ConfigPath string
	ConfigFile bool
	Boot       string
	Journal    Journal
	Machine    machine.Machine
	// Cycles is the number of clean cycles after search and deepening.
	Cycles int
	// Bootloader is set only in the tuning boot, where a dead end hands the next boot back to the normal system.
	Bootloader Bootloader
	// Prompt is nil when stdin or stderr is not a terminal.
	Prompt func(defect.Finding) (bool, error)
	// Defects overrides the binary's entries in tests; nil uses the shipped list.
	Defects []defect.Entry
	// Carry is what a transition carries into a new session; nil otherwise.
	Carry     *carry.Carry
	Stderr    io.Writer
	Close     func() error
	SessionID func(time.Time) (string, error)
}

type Bootloader interface {
	tuningboot.Environment
	ClearSavedEntry() (before, after string, err error)
}

type Journal interface {
	Events() []journal.Event
	BootReasonRecorded(id string) (bool, error)
	Append(p journal.Payload, cause ...int) (journal.Event, error)
	WriteState(s journal.State) error
	ReadState() (journal.State, error)
}

type StopReason string

const (
	StopSignal  StopReason = "signal"
	StopDeadEnd StopReason = "dead_end"
	StopCycles  StopReason = "cycles"
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

	// condition and applied describe this process's last writes or same-boot readbacks.
	condition            machine.Condition
	applied              []int
	fatal                error
	smuValidated         bool
	sameBootUnreconciled bool
	cancelTrial          context.CancelFunc
	kernelWaited         int
	running              machine.Running
	shutdownEvent        *journal.Shutdown
	containmentFailed    bool
	swept                bool
	bootReason           *tuningboot.Reason
	bootProgress         bool
}

func Run(ctx context.Context, in Input) (stop Stop, err error) {
	r := &runner{in: in, fold: newFold(), tuner: tuner.New()}
	defer func() {
		err = errors.Join(err, r.close(!errors.Is(err, machine.ErrCrashed), &stop))
	}()
	stop, err = r.run(ctx)
	if errors.Is(err, errDeadEndEvidence) {
		return Stop{}, errors.New("dead-end evidence recorded without a dead end")
	}
	return stop, err
}

func (r *runner) run(ctx context.Context) (Stop, error) {
	events := r.in.Journal.Events()
	if err := r.checkCompatibility(events); err != nil {
		return Stop{}, err
	}
	if r.in.Bootloader != nil {
		var err error
		r.bootReason, err = tuningboot.ReadReason(r.in.Bootloader)
		if err != nil {
			return Stop{}, fmt.Errorf("read previous tuning-boot reason: %w", err)
		}
	}
	journal.Replay(events, r.fold, &r.state, r.tuner)
	r.tuner.Project(&r.state)
	sameBoot := slices.Contains(r.fold.boots, r.in.Boot)
	r.sameBootUnreconciled = sameBoot && r.fold.baselineSeq != 0
	if len(events) > 0 {
		if err := r.checkState(); err != nil {
			return Stop{}, err
		}
	}
	pending, _ := pendingDeadEnd(events)
	if !sameBoot {
		if stop, err := r.resumeDeadEnd(events); stop != nil || err != nil {
			return deref(stop), err
		}
	}
	cores, err := r.in.Machine.Host.Topology()
	if err != nil {
		return Stop{}, fmt.Errorf("read topology: %w", err)
	}
	slices.SortFunc(cores, func(a, b machine.CoreInfo) int { return a.Core - b.Core })
	r.cores = cores
	if err := r.validateConfiguredCores(); err != nil {
		return Stop{}, err
	}

	recoveryCanceled := false
	if pending == nil {
		if err := r.startJournal(); err != nil {
			return Stop{}, err
		}
		boundary, err := r.startupBoundary()
		if err != nil {
			return Stop{}, err
		}
		if err := r.recordConfig(boundary); err != nil {
			return Stop{}, err
		}
		if err := r.recoverCrashes(ctx); err != nil {
			if !errors.Is(err, context.Canceled) {
				return r.afterEvidence(err)
			}
			if !r.sameBootUnreconciled {
				return r.shutdown(&journal.Shutdown{Reason: journal.ShutdownSignal}, StopSignal)
			}
			recoveryCanceled = true
		}
		if !sameBoot {
			if stop, err := r.checkDefects(); stop != nil || err != nil {
				return deref(stop), err
			}
			if stop, err := r.checkDeadEnd(); stop != nil || err != nil {
				return deref(stop), err
			}
		}
	}
	if stop, err := r.preflight(ctx); stop != nil || err != nil {
		return deref(stop), err
	}
	if stop, err := r.sweep(); stop != nil || err != nil {
		return deref(stop), err
	}
	if sameBoot {
		if stop, err := r.resumeSameBoot(events); stop != nil || err != nil {
			return deref(stop), err
		}
	}
	if recoveryCanceled {
		return r.shutdown(&journal.Shutdown{Reason: journal.ShutdownSignal}, StopSignal)
	}
	if err := r.startSession(); err != nil {
		return r.afterEvidence(err)
	}
	return r.loop(ctx)
}

func (r *runner) startJournal() error {
	if r.fold.started {
		return nil
	}
	now := r.in.Machine.Clock.Now()
	id := now.UTC().Format("20060102T150405Z")
	if r.in.SessionID != nil {
		var err error
		id, err = r.in.SessionID(now)
		if err != nil {
			return err
		}
	}
	_, err := r.append(&journal.SessionStart{Build: Build(), Session: id, Cores: r.cores, Evidence: tuner.EvidenceEpoch})
	return err
}

func (r *runner) recordConfig(boundary journal.KernelBoundary) error {
	if _, err := r.append(&journal.ConfigLoaded{Build: Build(), KernelBoundary: boundary, Path: r.in.ConfigPath, File: r.in.ConfigFile, Config: configSnapshot(r.in.Config)}); err != nil {
		return err
	}
	return r.warnWatchdog()
}

func (r *runner) checkCompatibility(events []journal.Event) error {
	if len(events) == 0 {
		return nil
	}
	err := journal.Compatible(journal.BuildOf(events), Build())
	if err == nil {
		return nil
	}
	if r.in.Bootloader != nil {
		reasonErr := r.saveLeaveReason("journal incompatible")
		if _, _, clearErr := r.in.Bootloader.ClearSavedEntry(); clearErr != nil {
			return errors.Join(fmt.Errorf("clear GRUB saved entry after incompatible journal: %w: %w", err, clearErr), reasonErr)
		}
		return errors.Join(err, reasonErr)
	}
	return err
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

func (r *runner) validateConfiguredCores() error {
	for _, core := range slices.Sorted(maps.Keys(r.in.Config.StartOffsets)) {
		if r.coreInfo(core) == nil {
			return fmt.Errorf("start offset for core %d: %w", core, ErrNoSuchCore)
		}
	}
	for _, core := range slices.Sorted(maps.Keys(r.in.Config.CandidateSoloLimits)) {
		if r.coreInfo(core) == nil {
			return fmt.Errorf("candidate solo limit for core %d: %w", core, ErrNoSuchCore)
		}
	}
	return nil
}

func (r *runner) append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := r.appendJournal(p, cause...)
	if err != nil {
		return journal.Event{}, err
	}
	if err := r.in.Journal.WriteState(r.state); err != nil {
		if _, err := r.appendJournal(&journal.SessionWarning{Operation: "write state projection", Error: err.Error()}, e.Seq); err != nil {
			return journal.Event{}, err
		}
	}
	return e, nil
}

func (r *runner) appendJournal(p journal.Payload, cause ...int) (journal.Event, error) {
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
	if !r.bootProgress && r.in.Bootloader != nil {
		r.bootProgress = true
		if err := r.recordBootProgress(); err != nil {
			return journal.Event{}, err
		}
	}
	return e, nil
}

func (r *runner) recordBootProgress() error {
	if err := tuningboot.ResetCount(r.in.Bootloader); err != nil {
		return fmt.Errorf("reset tuning-boot restart count after durable journal append: %w", err)
	}
	return r.importBootReason()
}

func (r *runner) importBootReason() error {
	if r.bootReason == nil {
		return nil
	}
	recorded, err := r.in.Journal.BootReasonRecorded(r.bootReason.ID)
	if err != nil {
		return fmt.Errorf("find journaled tuning-boot reason: %w", err)
	}
	if !recorded {
		if _, err := r.appendJournal(&journal.BootLeaveReason{ReasonID: r.bootReason.ID, RestartLimitCount: r.bootReason.Count, Reason: r.bootReason.Reason}); err != nil {
			return err
		}
		if r.bootReason == nil {
			return nil
		}
	}
	if err := tuningboot.ClearReason(r.in.Bootloader); err != nil {
		return fmt.Errorf("clear journaled tuning-boot reason: %w", err)
	}
	r.bootReason = nil
	return nil
}

func (r *runner) saveLeaveReason(reason string) error {
	record, err := tuningboot.NewReason(reason, 0)
	if err == nil {
		err = tuningboot.WriteReason(r.in.Bootloader, record)
	}
	if err != nil {
		return fmt.Errorf("save tuning-boot leave reason: %w", err)
	}
	return nil
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
	return r.fatal
}

func (r *runner) emergencyRestore(err error) error {
	if !r.swept {
		if r.in.Stderr != nil {
			fmt.Fprintf(r.in.Stderr, "togi: journal write failed: %v; offsets unchanged before successful trial-scope sweep\n", err)
		}
		return nil
	}
	if !r.smuValidated {
		return nil
	}
	zeroErr := r.in.Machine.SMU.SetAllOffsets(0)
	status := "readback all 0"
	var problems []string
	var cleanupErrors []error
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
			cleanupErrors = append(cleanupErrors, fmt.Errorf("emergency read core %02d: %w", core, readErr))
		} else if o != 0 {
			problems = append(problems, fmt.Sprintf("core %02d reads %d", core, o))
			cleanupErrors = append(cleanupErrors, fmt.Errorf("emergency read core %02d: got %d, want 0", core, o))
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
	return errors.Join(append(cleanupErrors, zeroErr)...)
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
		_, warningErr := r.appendJournal(&journal.SessionWarning{Operation: "write state projection", Error: err.Error()})
		return warningErr
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
		reason, err := r.readResetReason(ctx, crashed, true)
		if err != nil {
			return err
		}
		confirmed = confirmed || reason.Kind != ""
		if !confirmed {
			for _, b := range r.fold.boots {
				other, err := r.readResetReason(ctx, b, false)
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
		record, readErr := r.in.Machine.Kernel.SavedPstore(crashed)
		if readErr != nil {
			if _, err := r.append(&journal.SessionWarning{Operation: "read saved pstore", Error: readErr.Error()}); err != nil {
				return err
			}
		} else {
			detected.Pstore = record
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

func (r *runner) readResetReason(ctx context.Context, boot string, after bool) (machine.ResetReason, error) {
	if r.fold.kernelRetries > 0 && r.kernelWaited == 0 {
		if err := r.waitKernelRetry(ctx); err != nil {
			return machine.ResetReason{}, err
		}
	}
	for {
		var reason machine.ResetReason
		var err error
		if after {
			reason, err = r.in.Machine.Kernel.ResetReasonAfter(boot)
		} else {
			reason, err = r.in.Machine.Kernel.ResetReason(boot)
		}
		if errors.Is(err, machine.ErrBootMissing) {
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
		missing: "togi stopped during the trial",
	}
	cause := []int{open.seq}
	if open.core != nil {
		evidence.result.Core = *open.core
	}
	if len(open.mces) > 0 {
		evidence.mces = []recordedMCE{{corrected: open.corrected}}
		cause = append(cause, open.mces...)
		if open.signal == "" {
			evidence.missing = "togi stopped during the trial after a machine check"
		}
	}
	interrupted, stopped := true, true
	if seq, crashed := r.fold.crashSeq[open.boot]; crashed {
		if open.signal == machine.CorrectedMCE || open.signal == machine.UncorrectedMCE {
			cause = append(cause, r.fold.recordedFor(open.boot, r.in.Boot)...)
		}
		if open.signal != "" {
			evidence.missing, stopped = "backend reported a failure before the reset", false
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
			evidence.missing, stopped = "", false
			interrupted = reset.Outcome != journal.OutcomeFailure
		}
	}
	evidence.missing = joinDiagnostic(evidence.missing, open.kernelError)
	end := adjudicateTrial(evidence)
	end.Trial, end.Interrupted = open.intent.Trial, interrupted
	// A kernel diagnostic joined into the reason has always kept the measured wording.
	end.LastEvidence = stopped && open.kernelError == ""
	end.KernelBoundary = journal.KernelBoundary{KernelCursor: r.fold.kernelCursors[r.in.Boot], KernelError: open.kernelError}
	if open.core == nil {
		end.Core = nil
	}
	cores := open.intent.Cores
	if len(cores) == 0 && open.intent.Core != nil {
		cores = []int{*open.intent.Core}
	}
	summary := sampleEvidence(r.in.Machine.Trials.Samples(open.intent.Trial), cores, open.intent.Regime, r.fold.ccds)
	end.VoltageRequestMedianV, end.VoltageRequestMinV = summary.voltageMedianV, summary.voltageMinV
	end.VoltageRequestsV, end.TopRequesters, end.CCDMHz = summary.requests.Requests, summary.requests.TopRequesters, summary.requests.CCDMHz
	if _, crashed := r.fold.crashSeq[open.boot]; crashed || end.Outcome == journal.OutcomeFailure {
		if end.Outcome == journal.OutcomeFailure {
			end.StalledCore, end.WorkerStalledMS = summary.stalledCore, summary.workerStalledMS
		}
		if sample := summary.last; sample != nil && crashed {
			end.LastSampleS = new(int(sample.ElapsedMS / 1000))
			end.LastSampleTctlC = sample.TctlC
			for _, mhz := range sample.CoreMHz {
				if end.LastSampleMinMHz == nil || mhz < *end.LastSampleMinMHz {
					end.LastSampleMinMHz = new(mhz)
				}
				if end.LastSampleMaxMHz == nil || mhz > *end.LastSampleMaxMHz {
					end.LastSampleMaxMHz = new(mhz)
				}
			}
		}
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

func pendingDeadEnd(events []journal.Event) (*journal.Event, bool) {
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
			return nil, false
		}
		return &events[i], !entry
	}
	return nil, false
}

func (r *runner) resumeDeadEnd(events []journal.Event) (*Stop, error) {
	e, clear := pendingDeadEnd(events)
	if e == nil {
		return nil, nil
	}
	return r.finishDeadEnd(*e, clear)
}

func (r *runner) resumeSameBoot(events []journal.Event) (*Stop, error) {
	a, pending, err := r.drainDecisions()
	if err != nil {
		return nil, err
	}
	if err := r.reconcileSameBoot(); err != nil {
		result, err := r.afterEvidence(err)
		return &result, err
	}
	if pending {
		return r.deadEnd(a.Payload.(*journal.DeadEnd), a.Cause...)
	}
	if stop, err := r.resumeDeadEnd(events); stop != nil || err != nil {
		return stop, err
	}
	if stop, err := r.checkDefects(); stop != nil || err != nil {
		return stop, err
	}
	return r.checkDeadEnd()
}

func (r *runner) reconcileSameBoot() error {
	if r.fold.baselineSeq == 0 {
		return nil
	}
	offsets := make([]int, len(r.cores))
	for i, c := range r.cores {
		o, err := r.in.Machine.SMU.Offset(c.Core)
		if err != nil {
			return r.smuFailed(journal.SMURead, new(c.Core), 0, 0, err)
		}
		if _, err := r.append(&journal.SMUReadback{Core: c.Core, Offset: o}); err != nil {
			return err
		}
		offsets[i] = o
	}
	r.applied = offsets
	r.condition = ""
	if err := r.restore(); err != nil {
		return err
	}
	r.sameBootUnreconciled = false
	return nil
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
	if r.sameBootUnreconciled {
		if pending, _ := pendingDeadEnd(r.in.Journal.Events()); pending != nil {
			return r.deadEndStop(d, cause, false), nil
		}
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
	if r.sameBootUnreconciled {
		return r.deadEndStop(d, e.Cause, false), nil
	}
	cleared := d.Action == journal.ActionExit
	switch {
	case d.Action == journal.ActionExit || (clear && r.in.Bootloader == nil):
	case clear:
		if err := r.importBootReason(); err != nil {
			return nil, err
		}
		reasonErr := r.saveLeaveReason(fmt.Sprintf("dead end %s: %s", d.Condition, d.Detail))
		before, after, cerr := r.in.Bootloader.ClearSavedEntry()
		entry := &journal.BootSavedEntry{Before: before, After: after}
		if reasonErr != nil {
			entry.ReasonError = reasonErr.Error()
		}
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
	r.shutdownEvent = &journal.Shutdown{Reason: journal.ShutdownDeadEnd}
	return r.deadEndStop(d, e.Cause, d.Action == journal.ActionClearSavedEntryAndReboot && cleared), nil
}

func (r *runner) deadEndStop(d *journal.DeadEnd, cause []int, reboot bool) *Stop {
	stop := &Stop{Reason: StopDeadEnd, DeadEnd: d, Reboot: reboot}
	for _, seq := range cause {
		stop.Evidence = append(stop.Evidence, r.eventAt(seq))
	}
	return stop
}

func (r *runner) preflight(ctx context.Context) (*Stop, error) {
	if r.sameBootUnreconciled {
		ctx = context.WithoutCancel(ctx)
	}
	if r.in.Bootloader != nil {
		if stop, err := r.waitWatchdog(ctx); stop != nil || err != nil {
			return stop, err
		}
	}
	var (
		failed []int
		names  []string
	)
	r.smuValidated = r.in.Machine.Host.ValidateSMU() == nil
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

func (r *runner) sweep() (*Stop, error) {
	detail, sweepErr := r.in.Machine.Trials.Sweep(context.Background())
	r.swept = sweepErr == nil
	if sweepErr != nil {
		detail = sweepErr.Error()
	}
	e, err := r.append(&journal.PreflightCheck{Check: "trial_scopes", Detail: detail, OK: sweepErr == nil})
	if err != nil {
		return nil, err
	}
	if sweepErr != nil {
		return r.deadEnd(&journal.DeadEnd{Condition: journal.DeadEndContainment, Detail: detail}, e.Seq)
	}
	return nil, nil
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
		if o, ok := r.in.Config.CandidateSoloLimits[c.Core]; ok {
			start, reason, check = o, "configured candidate solo limit", true
		} else if o, ok := r.in.Config.StartOffsets[c.Core]; ok {
			start, reason = o, "configured start offset"
		} else if has && cc.CandidateSoloLimit != nil {
			start, reason, check = *cc.CandidateSoloLimit, fmt.Sprintf("candidate solo limit %d carried from session %s", *cc.CandidateSoloLimit, cc.CandidateSoloLimitSession), true
		} else if start != b {
			reason = fmt.Sprintf("baseline %d clamped to %d", b, start)
		}
		p := &journal.CorePhase{Core: c.Core, To: phase, CheckSoloLimit: check}
		if check {
			p.Workloads = []string{machine.Workloads(machine.R1)[0].ID, machine.Workloads(machine.R2)[0].ID}
		}
		cause := []int{r.fold.baselineSeq}
		if has {
			cause = append(cause, r.fold.carriedSeq)
		}
		if has && cc.FailurePoint != nil {
			m := *cc.FailurePoint
			p.FailurePoint = new(m)
			if m < 0 && start <= m {
				start = m + 1
				reason += fmt.Sprintf("; clamped to %d, one count shallower than the failure point %d carried from session %s", start, m, cc.FailurePointSession)
			}
		}
		p.Offset, p.Reason = start, reason
		if _, err := r.append(p, cause...); err != nil {
			return err
		}
	}
	return nil
}

// recordCarry commits the transition after its same-BIOS facts have been recorded.
func (r *runner) recordCarry() error {
	c := r.in.Carry
	if err := c.ResolveFacts(r.fold.context); err != nil {
		return err
	}
	p := &journal.SessionCarried{Sources: c.Sources, FailurePoints: true}
	if c.Context == nil {
		p.FailurePoints, p.Detail = false, "the archived session recorded no BIOS context"
	} else if detail, ok := machine.CompareContext(*c.Context, *r.fold.context); !ok {
		p.FailurePoints, p.Detail = false, detail
	}
	if p.FailurePoints && len(c.Facts) > 0 {
		type identity struct {
			session string
			seq     int
		}
		recorded := make(map[identity]struct{})
		for _, e := range r.in.Journal.Events() {
			var source journal.FactSource
			switch v := e.Data.(type) {
			case *journal.TrialCarried:
				source = v.Source
			case *journal.FailureCarried:
				source = v.Source
			default:
				continue
			}
			recorded[identity{source.Session, source.Seq}] = struct{}{}
		}
		for _, f := range c.Facts {
			key := identity{f.Session, f.Seq}
			if _, ok := recorded[key]; ok {
				continue
			}
			if _, err := r.append(f.Payload()); err != nil {
				return err
			}
			recorded[key] = struct{}{}
		}
	}
	for _, cc := range c.Cores {
		if r.coreInfo(cc.Core) == nil {
			continue
		}
		if !p.FailurePoints {
			cc.FailurePoint, cc.FailurePointSession, cc.FailurePointSeq, cc.FailurePointSignal = nil, "", 0, ""
			if cc.CandidateSoloLimit == nil {
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
	case machine.Alone:
	case machine.Together:
		target = r.tuner.Profile()
	case machine.Parked:
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
				if constraint, reaches := r.tuner.Reaches(next); reaches {
					return fmt.Errorf("refusing to write core %02d to %d: the profile would reach %s", c.Core, target[i], constraint)
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
			if g, ok := a.Payload.(*journal.CheckingCycle); ok && g.Event == journal.CycleStart && r.reachedCycles() {
				return r.shutdown(&journal.Shutdown{Reason: journal.ShutdownCycles, Cycles: r.in.Cycles}, StopCycles)
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

func (r *runner) reachedCycles() bool {
	return r.in.Cycles > 0 && r.tuner.CleanCycles() >= r.in.Cycles
}

func (r *runner) shutdown(p *journal.Shutdown, stop StopReason) (Stop, error) {
	r.shutdownEvent = p
	return Stop{Reason: stop}, nil
}

func (r *runner) close(restore bool, stop *Stop) (err error) {
	if r.in.Close != nil {
		defer func() { err = errors.Join(err, r.in.Close()) }()
	}
	if r.cancelTrial != nil {
		r.cancelTrial()
	}
	if r.running != nil {
		err = errors.Join(err, r.running.Stop())
		r.running = nil
	}
	if !restore || r.containmentFailed || errors.Is(err, machine.ErrContainment) {
		return err
	}
	defer func() {
		if r.fatal != nil {
			err = errors.Join(err, r.emergencyRestore(errors.Unwrap(r.fatal)))
		}
	}()
	if err != nil {
		return err
	}
	if r.fatal != nil {
		return err
	}
	if stop.Reason == StopSignal {
		if drainErr := r.drain(stop); drainErr != nil {
			return errors.Join(err, drainErr)
		}
	}
	var restoreErr error
	if stop.DeadEnd == nil || stop.DeadEnd.Condition != journal.DeadEndSMU {
		restoreErr = r.restore()
	}
	if errors.Is(restoreErr, errDeadEndEvidence) {
		cleanupStop, cleanupErr := r.afterEvidence(restoreErr)
		if stop.Reason != StopDeadEnd {
			*stop = cleanupStop
		}
		restoreErr = cleanupErr
	}
	err = errors.Join(err, restoreErr)
	if restoreErr == nil && !r.sameBootUnreconciled && r.shutdownEvent != nil {
		boundary, boundaryErr := r.kernelBoundary("", 0, false)
		if boundaryErr != nil {
			return errors.Join(err, boundaryErr)
		}
		r.shutdownEvent.KernelBoundary = boundary
		_, appendErr := r.append(r.shutdownEvent)
		err = errors.Join(err, appendErr)
	}
	return err
}

func (r *runner) drain(stop *Stop) error {
	a, pending, err := r.drainDecisions()
	if err != nil || !pending {
		return err
	}
	result, err := r.deadEnd(a.Payload.(*journal.DeadEnd), a.Cause...)
	if result != nil {
		*stop = *result
	}
	return err
}

func (r *runner) drainDecisions() (tuner.Action, bool, error) {
	for {
		a, ok := r.tuner.Drain()
		if !ok {
			return tuner.Action{}, false, nil
		}
		if _, ok := a.Payload.(*journal.DeadEnd); ok {
			return a, true, nil
		}
		if _, err := r.append(a.Payload, a.Cause...); err != nil {
			return tuner.Action{}, false, err
		}
	}
}

// restore keeps baseline offsets only where they are no deeper than the safe current profile.
func (r *runner) restore() error {
	if r.applied == nil {
		return nil
	}
	targets := make([]int, len(r.cores))
	for i, c := range r.cores {
		o := r.fold.baseline[i]
		if s := slices.IndexFunc(r.state.Cores, func(s journal.CoreState) bool { return s.Core == c.Core }); s >= 0 {
			o = max(o, r.state.Cores[s].Offset)
			if failed := r.state.Cores[s].FailurePoint; failed != nil {
				o = max(o, *failed+1)
			}
		}
		targets[i] = machine.ClampOffset(o)
	}
	if slices.Equal(r.applied, targets) {
		return nil
	}
	return r.apply(targets, &journal.ProfileRestored{Offsets: targets}, r.fold.baselineSeq)
}
