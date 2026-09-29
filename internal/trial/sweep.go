package trial

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/shgew/togi/internal/machine"
)

func trialScope(unit string) bool {
	id, ok := strings.CutPrefix(unit, "togi-trial-")
	if !ok {
		return false
	}
	id, ok = strings.CutSuffix(id, ".scope")
	if !ok || id == "" {
		return false
	}
	for _, c := range id {
		switch {
		case c == '-', c == '_', c == '.', c == ':',
			c >= '0' && c <= '9', c >= 'a' && c <= 'z', c >= 'A' && c <= 'Z':
			continue
		default:
			return false
		}
	}
	return true
}

func (r *Runner) Sweep(parent context.Context) (string, error) {
	ctx, cancel := context.WithTimeout(parent, teardownLimit)
	defer cancel()
	deadline, _ := ctx.Deadline()
	units, err := r.host.ListScopes(ctx)
	if err != nil {
		return "", errors.Join(machine.ErrContainment, fmt.Errorf("list leftover trial scopes: %w", err))
	}
	processes, err := r.host.ScopeProcesses(ctx)
	if err != nil {
		return "", errors.Join(machine.ErrContainment, fmt.Errorf("list leftover trial processes: %w", err))
	}
	var scopes []string
	for _, unit := range units {
		if trialScope(unit) {
			scopes = append(scopes, strings.TrimSuffix(unit, ".scope"))
		}
	}
	var instances []*instance
	for _, p := range processes {
		if !trialScope(p.Scope) {
			continue
		}
		scopes = append(scopes, strings.TrimSuffix(p.Scope, ".scope"))
		if !slices.ContainsFunc(instances, func(inst *instance) bool { return inst.PID == p.Group }) {
			instances = append(instances, &instance{PID: p.Group})
		}
	}
	slices.Sort(scopes)
	scopes = slices.Compact(scopes)
	var verificationErr error
	phase := 0
	collect := func(until time.Time) bool {
		phase++
		if phase == 2 {
			verificationErr = nil
		}
		checkCtx, checkCancel := context.WithDeadline(ctx, until)
		defer checkCancel()
		ticker := time.NewTicker(100 * time.Millisecond)
		defer ticker.Stop()
		for {
			clear := true
			for _, p := range processes {
				if !trialScope(p.Scope) {
					continue
				}
				alive, err := r.host.ProcessAlive(p)
				if err != nil {
					verificationErr = fmt.Errorf("verify process %d exited: %w", p.PID, err)
					return false
				}
				clear = clear && !alive
			}
			if phase == 2 {
				units, err := r.host.ListScopes(checkCtx)
				if err != nil {
					verificationErr = fmt.Errorf("verify leftover trial units removed: %w", err)
					return false
				}
				remaining, err := r.host.ScopeProcesses(checkCtx)
				if err != nil {
					verificationErr = fmt.Errorf("verify leftover trial processes removed: %w", err)
					return false
				}
				clear = clear && !slices.ContainsFunc(units, trialScope) && !slices.ContainsFunc(remaining, func(p scopeProcess) bool { return trialScope(p.Scope) })
			}
			if clear {
				return true
			}
			select {
			case <-checkCtx.Done():
				verificationErr = errors.New("leftover trial unit or process remains at sweep deadline")
				return false
			case <-ticker.C:
			}
		}
	}
	err = terminate(sweepHost{r.host}, instances, scopes, deadline, r.options.StopGrace, collect)
	if err != nil || verificationErr != nil || ctx.Err() != nil {
		return "", errors.Join(machine.ErrContainment, err, verificationErr, ctx.Err())
	}
	return fmt.Sprintf("removed %d leftover trial scopes; no trial unit or process remains", len(scopes)), nil
}

type sweepHost struct{ processHost }

func (h sweepHost) KillScope(ctx context.Context, scope string) ([]byte, error) {
	out, err := h.processHost.KillScope(ctx, scope)
	if err != nil && !scopeMissing(out, err) {
		return out, err
	}
	out, err = h.StopScope(ctx, scope)
	if scopeMissing(out, err) {
		return nil, nil
	}
	return out, err
}
