package session

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

func (r *runner) set(core, offset int, cause ...int) (int, error) {
	intent, err := r.append(&journal.SMUIntent{Op: journal.SMUSet, Core: new(core), Offset: offset}, cause...)
	if err != nil {
		return 0, err
	}
	if err := r.in.Machine.SMU.SetOffset(core, offset); err != nil {
		return 0, r.smuFailed(journal.SMUSet, new(core), offset, intent.Seq, err)
	}
	for i, c := range r.cores {
		if c.Core == core && r.applied != nil {
			r.applied[i] = offset
			break
		}
	}
	written, err := r.append(&journal.SMUWrite{Op: journal.SMUSet, Core: new(core), Offset: offset}, intent.Seq)
	if err != nil {
		return 0, err
	}
	reads, err := r.readback([]int{core}, offset, written.Seq)
	if err != nil {
		return 0, err
	}
	return reads[0], nil
}

func (r *runner) setAll(offset int, cause ...int) ([]int, error) {
	intent, err := r.append(&journal.SMUIntent{Op: journal.SMUSetAll, Offset: offset}, cause...)
	if err != nil {
		return nil, err
	}
	if err := r.in.Machine.SMU.SetAllOffsets(offset); err != nil {
		return nil, r.smuFailed(journal.SMUSetAll, nil, offset, intent.Seq, err)
	}
	written, err := r.append(&journal.SMUWrite{Op: journal.SMUSetAll, Offset: offset}, intent.Seq)
	if err != nil {
		return nil, err
	}
	cores := make([]int, len(r.cores))
	for i, c := range r.cores {
		cores[i] = c.Core
	}
	return r.readback(cores, offset, written.Seq)
}

func (r *runner) readback(cores []int, expected, cause int) ([]int, error) {
	var seqs []int
	for _, core := range cores {
		o, err := r.in.Machine.SMU.Offset(core)
		if err != nil {
			return nil, r.smuFailed(journal.SMURead, new(core), expected, cause, err)
		}
		e, err := r.append(&journal.SMUReadback{Core: core, Offset: o, Expected: new(expected)}, cause)
		if err != nil {
			return nil, err
		}
		if o != expected {
			return nil, errDeadEndEvidence
		}
		seqs = append(seqs, e.Seq)
	}
	return seqs, nil
}

func (r *runner) smuFailed(op journal.SMUOp, core *int, offset, cause int, err error) error {
	if errors.Is(err, machine.ErrCrashed) {
		return err
	}
	var causes []int
	if cause != 0 {
		causes = []int{cause}
	}
	if _, aerr := r.append(&journal.SMUError{Op: op, Core: core, Offset: offset, Error: err.Error()}, causes...); aerr != nil {
		return aerr
	}
	return errDeadEndEvidence
}

type trialRun struct {
	r      *runner
	t      tuner.Trial
	id     string
	intent journal.Event
	start  int
}

func (r *runner) trial(ctx context.Context, a tuner.Action) error {
	t := a.Trial
	multi := len(t.Cores) > 0
	var cores, cpus []int
	index := r.fold.allIndex[t.Regime]
	info := r.coreInfo(t.Core)
	if multi {
		for _, id := range t.Cores {
			c := r.coreInfo(id)
			if c == nil {
				return fmt.Errorf("trial on core %d: %w", id, ErrNoSuchCore)
			}
			cores = append(cores, id)
			cpus = append(cpus, c.CPUs[0])
		}
	} else {
		if info == nil {
			return fmt.Errorf("trial on core %d: %w", t.Core, ErrNoSuchCore)
		}
		index = r.fold.index[t.Core][t.Regime]
	}
	profile := slices.Clone(r.applied)
	if t.Condition == machine.Alone {
		profile = make([]int, len(r.cores))
		for i, c := range r.cores {
			if c.Core == t.Core {
				profile[i] = t.Offset
				break
			}
		}
	}
	p, w, err := t.Complete(index, profile)
	if err != nil {
		return err
	}
	if multi {
		p.Cores = cores
	} else {
		cores, cpus = []int{t.Core}, info.CPUs[:min(w.Threads, len(info.CPUs))]
	}
	duration := t.DurationS
	tr := &trialRun{r: r, t: t, id: fmt.Sprintf("%04d", r.fold.trials+1)}
	p.Trial = tr.id
	boundary, err := r.kernelBoundary("", 0, false)
	if err != nil {
		return err
	}
	p.KernelBoundary = boundary
	intent, err := r.append(p, a.Cause...)
	if err != nil {
		return err
	}
	tr.intent, tr.start = intent, intent.Seq
	if tr.writesTarget() {
		if _, err := r.set(t.Core, t.Offset, intent.Seq); err != nil {
			return err
		}
	}

	since := r.in.Machine.Clock.Monotonic()
	spec := machine.TrialSpec{
		ID: tr.id, Regime: t.Regime, Workload: w, Condition: t.Condition,
		Cores: cores, CPUs: cpus, Duration: time.Duration(duration) * time.Second,
		Index: index, Seed: uint64(intent.Seq),
	}
	trialCtx, cancel := context.WithCancel(ctx)
	r.cancelTrial = cancel
	running, err := r.in.Machine.Trials.Start(trialCtx, spec)
	if err != nil {
		defer cancel()
		r.cancelTrial = nil
		return tr.finish(trialCtx, since, machine.Result{}, "setup failed", err)
	}
	r.running = running
	s := running.Started()
	ts := &journal.TrialStart{Trial: tr.id, WindowStartNS: new(since.Nanoseconds()), Scope: s.Scope, PID: s.PID, CPUs: s.CPUs, Argv: s.Argv, Files: s.Files}
	if len(s.Instances) > 1 {
		for _, in := range s.Instances {
			ts.Instances = append(ts.Instances, journal.TrialInstance{Core: in.Core, CPUs: in.CPUs, PID: in.PID, Scope: in.Scope})
		}
	}
	started, err := r.append(ts, intent.Seq)
	if err != nil {
		return err
	}
	tr.start = started.Seq
	if s.Schedule != nil {
		if _, err := r.append(&journal.TrialSignal{Trial: tr.id, Schedule: s.Schedule.String(), Seed: s.Schedule.Seed}, started.Seq); err != nil {
			return err
		}
	}
	report := &trialReport{tr: tr}
	res, err := running.Wait(trialCtx, report)
	r.running = nil
	r.cancelTrial = nil
	defer cancel()
	if errors.Is(err, machine.ErrContainment) {
		r.containmentFailed = true
	}
	if report.err != nil {
		return errors.Join(report.err, err)
	}
	if s.Schedule != nil && err == nil {
		if _, err := r.append(&journal.TrialSignal{Trial: tr.id, Stops: res.Stops, Conts: res.Conts}, started.Seq); err != nil {
			return err
		}
	}
	return tr.finish(trialCtx, since, res, "trial runner failed", err)
}

type trialEvidence struct {
	result      machine.Result
	mces        []recordedMCE
	reset       *journal.TrialEnd
	missing     string
	duration    time.Duration
	containment string
}

func adjudicateTrial(e trialEvidence) *journal.TrialEnd {
	end := &journal.TrialEnd{DurationS: int(e.result.Ran.Seconds()), TctlMaxC: e.result.TctlMaxC, Reason: e.missing, ContainmentError: e.containment}
	switch {
	case len(e.result.Escaped) > 0:
		end.Outcome, end.Escaped = journal.OutcomeInconclusive, e.result.Escaped
		end.Reason = joinDiagnostic("backend thread outside allowed cpus", e.missing)
	case e.containment != "":
		end.Outcome = journal.OutcomeInconclusive
	case e.result.Signal != "":
		end.Outcome, end.Signal, end.Core = journal.OutcomeFailure, e.result.Signal, new(e.result.Core)
	case len(e.mces) > 0:
		end.Outcome, end.Signal = journal.OutcomeFailure, mceSignal(e.mces)
	case e.reset != nil:
		end.Outcome, end.Signal = e.reset.Outcome, e.reset.Signal
		end.Reason = joinDiagnostic(e.reset.Reason, e.missing)
	case e.missing != "" || e.result.Inconclusive != "":
		end.Outcome = journal.OutcomeInconclusive
		end.Reason = joinDiagnostic(e.result.Inconclusive, e.missing)
	case e.result.Ran < e.duration:
		end.Outcome, end.Reason = journal.OutcomeInconclusive, "trial ended before its full duration"
	default:
		end.Outcome = journal.OutcomePass
	}
	return end
}

func joinDiagnostic(reason, diagnostic string) string {
	if reason == "" {
		return diagnostic
	}
	if diagnostic == "" {
		return reason
	}
	return reason + "; " + diagnostic
}

func (tr *trialRun) finish(ctx context.Context, since time.Duration, res machine.Result, what string, runnerErr error) error {
	r := tr.r
	crashed := errors.Is(runnerErr, machine.ErrCrashed)
	containment := ""
	if errors.Is(runnerErr, machine.ErrContainment) {
		containment = runnerErr.Error()
		r.containmentFailed = true
	}
	if crashed && containment == "" && len(res.Escaped) == 0 && res.Signal == "" {
		return runnerErr
	}
	var boundary journal.KernelBoundary
	if !crashed && containment == "" {
		var err error
		boundary, err = tr.teardown(since)
		if err != nil {
			if !errors.Is(err, machine.ErrCrashed) || len(res.Escaped) == 0 && res.Signal == "" {
				return err
			}
			runnerErr, crashed = errors.Join(runnerErr, err), true
		}
	}
	mces := make([]recordedMCE, 0, len(r.fold.open.mces))
	for _, seq := range r.fold.open.mces {
		p := r.eventAt(seq).Data.(*journal.MCE)
		mces = append(mces, recordedMCE{seq: seq, corrected: p.Corrected})
	}
	diagnostic := joinDiagnostic(r.fold.open.kernelError, boundary.KernelError)
	if runnerErr != nil {
		diagnostic = joinDiagnostic(diagnostic, fmt.Sprintf("%s: %v", what, runnerErr))
	}
	end := adjudicateTrial(trialEvidence{result: res, mces: mces, missing: diagnostic, duration: time.Duration(tr.t.DurationS) * time.Second, containment: containment})
	end.Trial = tr.id
	end.KernelBoundary = boundary
	end.KernelError = joinDiagnostic(r.fold.open.kernelError, boundary.KernelError)
	end.BackendMissing = errors.Is(runnerErr, machine.ErrBackendMissing)
	end.Interrupted = ctx.Err() != nil || crashed
	if ctx.Err() != nil {
		end.Reason = joinDiagnostic(end.Reason, "stopped by signal")
	}
	if tr.t.Condition == machine.Alone {
		end.Core = nil
	}
	cores := tr.t.Cores
	if len(cores) == 0 {
		cores = []int{tr.t.Core}
	}
	summary := sampleEvidence(r.in.Machine.Trials.Samples(tr.id), cores, tr.t.Regime, r.ccdOf)
	end.VoltageRequestMedianV, end.VoltageRequestMinV = summary.voltageMedianV, summary.voltageMinV
	end.VoltageRequestsV, end.TopRequesters, end.CCDMHz = summary.requests.Requests, summary.requests.TopRequesters, summary.requests.CCDMHz
	if end.Outcome == journal.OutcomeFailure {
		end.StalledCore, end.WorkerStalledMS = summary.stalledCore, summary.workerStalledMS
	}
	ended, err := r.append(end, append([]int{tr.start}, tr.mceSeqs(mces)...)...)
	if err != nil {
		if containment != "" {
			return errors.Join(err, runnerErr)
		}
		return err
	}
	if end.Outcome == journal.OutcomePass {
		if err := r.in.Machine.Trials.Passed(tr.id); err != nil {
			_, warningErr := r.append(&journal.SessionWarning{Operation: "retain passed trial", Trial: tr.id, Error: err.Error()}, ended.Seq)
			return warningErr
		}
	}
	return runnerErrIfCrashed(runnerErr)
}

func runnerErrIfCrashed(err error) error {
	if errors.Is(err, machine.ErrCrashed) {
		return err
	}
	return nil
}

type trialReport struct {
	tr  *trialRun
	err error
}

func (p *trialReport) Progress(detail string) {
	p.record(&journal.TrialProgress{Trial: p.tr.id, Detail: detail})
}

func (p *trialReport) Signal(core int, signal machine.Signal, detail string) {
	label := string(signal)
	switch signal {
	case machine.ComputationError:
		label = "computation error"
	case machine.UnexpectedExit:
		label = "backend exited early"
	case machine.Stall:
		label = "backend stalled"
	case machine.CorrectedMCE, machine.UncorrectedMCE, machine.Crash:
	}
	progress := &journal.TrialProgress{Trial: p.tr.id, Signal: signal, Detail: fmt.Sprintf("core %02d %s: %s", core, label, detail)}
	if signal != machine.CorrectedMCE && signal != machine.UncorrectedMCE {
		progress.Core = new(core)
	}
	p.record(progress)
}

func (p *trialReport) Sample(s machine.Sample) {
	p.record(&journal.TrialSample{Trial: p.tr.id, Warning: s.Warning, PID: s.PID, TID: s.TID, CPU: s.CPU})
}

func (p *trialReport) record(payload journal.Payload) {
	if p.err != nil {
		return
	}
	_, p.err = p.tr.r.append(payload, p.tr.start)
}

// writesTarget reports whether the trial sets and reads back its target before running and resets it afterward.
// Trials alone do so at every offset, including 0; trials together run on the profile already applied.
func (tr *trialRun) writesTarget() bool {
	return tr.t.Condition == machine.Alone
}

type recordedMCE struct {
	seq       int
	corrected bool
}

func mceSignal(mces []recordedMCE) machine.Signal {
	for _, m := range mces {
		if m.corrected {
			return machine.CorrectedMCE
		}
	}
	return machine.UncorrectedMCE
}

func (tr *trialRun) teardown(since time.Duration) (journal.KernelBoundary, error) {
	r := tr.r
	if tr.writesTarget() {
		if _, err := r.set(tr.t.Core, 0, tr.start); err != nil {
			return journal.KernelBoundary{}, err
		}
	}
	return r.kernelBoundary(tr.id, since, false, tr.start)
}

func (tr *trialRun) mceSeqs(mces []recordedMCE) []int {
	seqs := make([]int, len(mces))
	for i, m := range mces {
		seqs[i] = m.seq
	}
	return seqs
}
