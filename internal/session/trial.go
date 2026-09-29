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
	w := machine.PickWorkload(t.Regime, index)
	if t.Workload != "" {
		i := slices.IndexFunc(machine.Workloads(t.Regime), func(w machine.Workload) bool { return w.ID == t.Workload })
		if i < 0 {
			return fmt.Errorf("trial workload %s: not a %s workload", t.Workload, t.Regime)
		}
		w = machine.Workloads(t.Regime)[i]
	}
	if !multi {
		cores, cpus = []int{t.Core}, info.CPUs[:min(w.Threads, len(info.CPUs))]
	}
	duration := t.DurationS
	tr := &trialRun{r: r, t: t, id: fmt.Sprintf("%04d", r.fold.trials+1)}
	profile := slices.Clone(r.applied)
	if t.Condition == machine.Isolated {
		profile = make([]int, len(r.cores))
		for i, c := range r.cores {
			if c.Core == t.Core {
				profile[i] = t.Offset
				break
			}
		}
	}
	p := &journal.TrialIntent{
		Trial: tr.id, Regime: t.Regime, Workload: w.ID, DurationS: duration,
		Condition: t.Condition, Phase: t.Phase, Retry: t.Retry, Rotation: t.Rotation,
		Profile: profile, Hunt: t.Hunt, Mask: t.Mask, Round: t.Round, Rerun: t.Rerun,
	}
	if multi {
		p.Cores = cores
	} else {
		p.Core, p.Offset = new(t.Core), new(t.Offset)
	}
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
	defer func() { r.cancelTrial = nil; cancel() }()
	running, err := r.in.Machine.Trials.Start(trialCtx, spec)
	if err != nil {
		return tr.failedToRun(trialCtx, since, 0, "setup failed", err)
	}
	s := running.Started()
	ts := &journal.TrialStart{Trial: tr.id, Scope: s.Scope, PID: s.PID, CPUs: s.CPUs, Argv: s.Argv, Files: s.Files}
	if len(s.Instances) > 1 {
		for _, in := range s.Instances {
			ts.Instances = append(ts.Instances, journal.TrialInstance{Core: in.Core, CPUs: in.CPUs, PID: in.PID, Scope: in.Scope})
		}
	}
	started, err := r.append(ts, intent.Seq)
	if err != nil {
		_, _ = running.Wait(trialCtx, &trialReport{tr: tr})
		return err
	}
	tr.start = started.Seq
	if s.Schedule != nil {
		if _, err := r.append(&journal.TrialSignal{Trial: tr.id, Schedule: s.Schedule.String(), Seed: s.Schedule.Seed}, started.Seq); err != nil {
			_, _ = running.Wait(trialCtx, &trialReport{tr: tr})
			return err
		}
	}
	report := &trialReport{tr: tr}
	res, err := running.Wait(trialCtx, report)
	if report.err != nil {
		return report.err
	}
	if err != nil {
		return tr.failedToRun(trialCtx, since, res.Ran, "trial runner failed", err)
	}
	if s.Schedule != nil {
		if _, err := r.append(&journal.TrialSignal{Trial: tr.id, Stops: res.Stops, Conts: res.Conts}, started.Seq); err != nil {
			return err
		}
	}
	mces, readErr, err := tr.teardown(since)
	if err != nil {
		return err
	}
	end := &journal.TrialEnd{Trial: tr.id, DurationS: int(res.Ran.Seconds()), TctlMaxC: res.TctlMaxC}
	switch {
	case len(res.Escaped) > 0:
		end.Outcome, end.Escaped, end.Reason = journal.OutcomeInconclusive, res.Escaped, "backend thread outside allowed cpus"
	case res.Signal != "":
		end.Outcome, end.Signal = journal.OutcomeFailure, res.Signal
		if t.Condition != machine.Isolated {
			end.Core = new(res.Core)
		}
	case len(mces) > 0:
		end.Outcome, end.Signal = journal.OutcomeFailure, mceSignal(mces)
	case readErr != nil:
		end.Outcome, end.Reason = journal.OutcomeInconclusive, "kernel log unreadable: "+readErr.Error()
	case res.Inconclusive != "":
		end.Outcome, end.Reason = journal.OutcomeInconclusive, res.Inconclusive
	default:
		end.Outcome = journal.OutcomePass
	}
	if _, err := r.append(end, append([]int{tr.start}, tr.mceSeqs(mces)...)...); err != nil {
		return err
	}
	if end.Outcome == journal.OutcomePass {
		if err := r.in.Machine.Trials.Passed(tr.id); err != nil {
			return fmt.Errorf("mark trial %s passed: %w", tr.id, err)
		}
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
	p.record(&journal.TrialProgress{Trial: p.tr.id, Signal: signal, Core: new(core), Detail: fmt.Sprintf("core %02d computation error: %s", core, detail)})
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

// writesTarget reports whether the trial sets its target before and resets it after: isolated trials at a nonzero
// offset. Resident trials run on the profile already applied.
func (tr *trialRun) writesTarget() bool {
	return tr.t.Condition == machine.Isolated
}

func (tr *trialRun) failedToRun(ctx context.Context, since time.Duration, ran time.Duration, what string, err error) error {
	r := tr.r
	if errors.Is(err, machine.ErrCrashed) {
		return err
	}
	interrupted := ctx.Err() != nil
	mces, _, terr := tr.teardown(since)
	if terr != nil {
		return terr
	}
	end := &journal.TrialEnd{Trial: tr.id, Outcome: journal.OutcomeInconclusive, DurationS: int(ran.Seconds()), Reason: fmt.Sprintf("%s: %v", what, err), BackendMissing: errors.Is(err, machine.ErrBackendMissing)}
	if interrupted {
		end.Interrupted, end.Reason = true, "stopped by signal"
	}
	if len(mces) > 0 {
		end.Outcome, end.Signal = journal.OutcomeFailure, mceSignal(mces)
	}
	_, err = r.append(end, append([]int{tr.start}, tr.mceSeqs(mces)...)...)
	return err
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

func (tr *trialRun) teardown(since time.Duration) (mces []recordedMCE, readErr, err error) {
	r := tr.r
	if tr.writesTarget() {
		if _, err := r.set(tr.t.Core, 0, tr.start); err != nil {
			return nil, nil, err
		}
	}
	found, readErr := r.in.Machine.Kernel.MCEs(r.in.Boot, since)
	if readErr != nil {
		if errors.Is(readErr, machine.ErrCrashed) {
			return nil, nil, readErr
		}
		return nil, readErr, nil
	}
	for _, m := range found {
		if r.fold.mceKeys[mceKey(r.in.Boot, m.Lines)] {
			continue
		}
		e, err := r.append(&journal.MCE{CPU: m.CPU, Core: m.Core, Bank: m.Bank, BankType: m.BankType, Corrected: m.Corrected, Lines: m.Lines}, tr.start)
		if err != nil {
			return nil, nil, err
		}
		mces = append(mces, recordedMCE{seq: e.Seq, corrected: m.Corrected})
	}
	return mces, nil, nil
}

func (tr *trialRun) mceSeqs(mces []recordedMCE) []int {
	seqs := make([]int, len(mces))
	for i, m := range mces {
		seqs[i] = m.seq
	}
	return seqs
}
