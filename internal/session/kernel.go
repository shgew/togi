package session

import (
	"errors"
	"fmt"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func (r *runner) kernelBoundary(trial string, since time.Duration, recovering bool, cause ...int) (journal.KernelBoundary, error) {
	cursor := r.fold.kernelCursors[r.in.Boot]
	read, readErr := r.in.Machine.Kernel.ReadMCEs(r.in.Boot, cursor)
	if errors.Is(readErr, machine.ErrCrashed) {
		return journal.KernelBoundary{}, readErr
	}
	if errors.Is(readErr, machine.ErrCursorMissing) && cursor != "" {
		fresh, err := r.in.Machine.Kernel.ReadMCEs(r.in.Boot, "")
		read.MCEs = append(read.MCEs, fresh.MCEs...)
		if errors.Is(err, machine.ErrCrashed) {
			return journal.KernelBoundary{}, err
		}
		readErr = errors.Join(readErr, err)
		if err == nil {
			cursor = fresh.Cursor
		}
	} else if readErr == nil {
		cursor = read.Cursor
	}
	boundary := journal.KernelBoundary{KernelCursor: cursor}
	if readErr != nil {
		boundary.KernelError = "kernel log unreadable: " + readErr.Error()
	}
	for _, m := range read.MCEs {
		if r.fold.mceKeys[mceKey(r.in.Boot, m.Lines)] {
			continue
		}
		p := &journal.MCE{CPU: m.CPU, Core: m.Core, Bank: m.Bank, BankType: m.BankType, Corrected: m.Corrected, Lines: m.Lines}
		if trial != "" && m.Monotonic >= since {
			p.Trial = trial
		} else {
			if recovering && !m.Corrected && len(r.fold.crashedBoots(r.in.Boot)) > 0 {
				p.FromBoot = r.in.Boot
			} else {
				p.BetweenTrials = true
			}
		}
		if _, err := r.append(p, cause...); err != nil {
			return journal.KernelBoundary{}, err
		}
	}
	return boundary, nil
}

func (r *runner) startupBoundary() (journal.KernelBoundary, error) {
	trial := ""
	var since time.Duration
	if open := r.fold.open; open != nil && open.boot == r.in.Boot {
		trial = open.intent.Trial
		if open.windowStartNS != nil {
			since = time.Duration(*open.windowStartNS)
		} else {
			since = time.Duration(open.startMono) * time.Millisecond
		}
	}
	boundary, err := r.kernelBoundary(trial, since, true)
	if err != nil {
		return journal.KernelBoundary{}, fmt.Errorf("read startup kernel boundary: %w", err)
	}
	return boundary, nil
}

func (r *runner) recoveryBootMCEs(boot string, mces []machine.MCE) (bool, error) {
	open := r.fold.open
	inTrial := open != nil && open.boot == boot
	var since time.Duration
	if inTrial && open.windowStartNS != nil {
		since = time.Duration(*open.windowStartNS)
	}
	evidence := false
	for _, m := range mces {
		between := !inTrial || m.Monotonic < since
		evidence = evidence || !between
		if err := r.recordMCE(m, boot, between); err != nil {
			return false, err
		}
	}
	return evidence, nil
}
