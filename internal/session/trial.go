package session

import (
	"context"
	"errors"
	"fmt"
	"time"

	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
	"code.marleb.org/shgew/shycler/internal/tuner"
)

func (r *runner) set(core, offset int, cause ...int) error {
	intent, err := r.append(&journal.SMUIntent{Op: journal.SMUSet, Core: new(core), Offset: offset}, cause...)
	if err != nil {
		return err
	}
	if err := r.in.Machine.SMU.SetOffset(core, offset); err != nil {
		return r.smuFailed(journal.SMUSet, new(core), offset, intent.Seq, err)
	}
	written, err := r.append(&journal.SMUWrite{Op: journal.SMUSet, Core: new(core), Offset: offset}, intent.Seq)
	if err != nil {
		return err
	}
	_, err = r.readback([]int{core}, offset, written.Seq)
	return err
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

func (r *runner) trial(ctx context.Context, a tuner.Action) (*Stop, error) {
	t := a.Trial
	info := r.coreInfo(t.Core)
	if info == nil {
		return nil, fmt.Errorf("trial on core %d: %w", t.Core, ErrNoSuchCore)
	}
	index := r.fold.index[t.Core][t.Regime]
	w := machine.PickWorkload(t.Regime, index)
	duration := r.in.Config.Durations.SearchTrialS
	if t.Phase == journal.PhaseConfirmation {
		duration = r.in.Config.Durations.ConfirmationTrialS
	}
	cpus := info.CPUs[:min(w.Threads, len(info.CPUs))]
	tr := &trialRun{r: r, t: t, id: fmt.Sprintf("%04d", r.fold.trials+1)}
	intent, err := r.append(&journal.TrialIntent{
		Trial: tr.id, Core: new(t.Core), Offset: new(t.Offset), Regime: t.Regime, Workload: w.ID,
		DurationS: duration, Condition: machine.Isolated, Phase: t.Phase, Retry: t.Retry,
	}, a.Cause...)
	if err != nil {
		return nil, err
	}
	tr.intent, tr.start = intent, intent.Seq
	if t.Offset != 0 {
		if err := r.set(t.Core, t.Offset, intent.Seq); err != nil {
			return nil, err
		}
	}

	since := r.in.Machine.Clock.Now()
	spec := machine.TrialSpec{
		ID: tr.id, Regime: t.Regime, Workload: w, Condition: machine.Isolated,
		Cores: []int{t.Core}, CPUs: cpus, Duration: time.Duration(duration) * time.Second,
		Index: index, Seed: uint64(intent.Seq),
	}
	running, err := r.in.Machine.Trials.Start(ctx, spec)
	if err != nil {
		return tr.failedToRun(ctx, since, "setup failed", err)
	}
	s := running.Started()
	started, err := r.append(&journal.TrialStart{Trial: tr.id, Scope: s.Scope, PID: s.PID, CPUs: s.CPUs, Argv: s.Argv}, intent.Seq)
	if err != nil {
		return nil, err
	}
	tr.start = started.Seq
	if s.Schedule != nil {
		if _, err := r.append(&journal.TrialSignal{Trial: tr.id, Schedule: s.Schedule.String(), Seed: s.Schedule.Seed}, started.Seq); err != nil {
			return nil, err
		}
	}
	res, err := running.Wait(ctx)
	if err != nil {
		return tr.failedToRun(ctx, since, "trial runner failed", err)
	}
	if res.Stops+res.Conts > 0 {
		if _, err := r.append(&journal.TrialSignal{Trial: tr.id, Stops: res.Stops, Conts: res.Conts}, started.Seq); err != nil {
			return nil, err
		}
	}
	mces, readErr, err := tr.teardown(since)
	if err != nil {
		return nil, err
	}
	end := &journal.TrialEnd{Trial: tr.id, DurationS: int(res.Ran.Seconds()), TctlMaxC: new(res.TctlMaxC)}
	switch {
	case len(res.Escaped) > 0:
		end.Outcome, end.Escaped, end.Reason = journal.OutcomeInconclusive, res.Escaped, "backend thread outside allowed cpus"
	case res.Signal != "":
		end.Outcome, end.Signal = journal.OutcomeFailure, res.Signal
	case len(mces) > 0:
		end.Outcome, end.Signal = journal.OutcomeFailure, machine.UncorrectedMCE
		for _, m := range mces {
			if m.corrected {
				end.Signal = machine.CorrectedMCE
			}
		}
	case readErr != nil:
		end.Outcome, end.Reason = journal.OutcomeInconclusive, "kernel log unreadable: "+readErr.Error()
	case res.Inconclusive != "":
		end.Outcome, end.Reason = journal.OutcomeInconclusive, res.Inconclusive
	default:
		end.Outcome = journal.OutcomePass
	}
	_, err = r.append(end, append([]int{tr.start}, tr.mceSeqs(mces)...)...)
	return nil, err
}

func (tr *trialRun) failedToRun(ctx context.Context, since time.Time, what string, err error) (*Stop, error) {
	r := tr.r
	if errors.Is(err, machine.ErrCrashed) {
		return nil, err
	}
	interrupted := ctx.Err() != nil
	if _, _, terr := tr.teardown(since); terr != nil {
		return nil, terr
	}
	end := &journal.TrialEnd{Trial: tr.id, Outcome: journal.OutcomeInconclusive, Reason: fmt.Sprintf("%s: %v", what, err)}
	if interrupted {
		end.Interrupted, end.Reason = true, "stopped by signal"
	}
	if _, err := r.append(end, tr.start); err != nil {
		return nil, err
	}
	if !interrupted {
		return nil, nil
	}
	stop, err := r.shutdown(journal.ShutdownSignal, StopSignal)
	return &stop, err
}

type recordedMCE struct {
	seq       int
	corrected bool
}

func (tr *trialRun) teardown(since time.Time) (mces []recordedMCE, readErr, err error) {
	r := tr.r
	if tr.t.Offset != 0 {
		if err := r.set(tr.t.Core, 0, tr.start); err != nil {
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
