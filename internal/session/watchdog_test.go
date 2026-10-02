package session

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type watchdogClock struct {
	now    time.Duration
	cancel context.CancelFunc
}

func (c *watchdogClock) Now() time.Time           { return time.Unix(0, int64(c.now)) }
func (c *watchdogClock) Monotonic() time.Duration { return c.now }
func (c *watchdogClock) Sleep(ctx context.Context, d time.Duration) error {
	c.now += d
	if c.cancel != nil {
		c.cancel()
	}
	return ctx.Err()
}

type watchdogHost struct {
	machine.Host
	clock   *watchdogClock
	readyAt time.Duration
}

func (h watchdogHost) Watchdog() machine.Check {
	return machine.Check{Name: "watchdog", Detail: "test hardware watchdog", OK: h.clock.now >= h.readyAt}
}

type stopBeforeTrial struct{ Journal }

func (j stopBeforeTrial) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if err == nil && e.Kind == journal.KindTrialIntent {
		return e, errKilled
	}
	return e, err
}

func TestWatchdogPreflight(t *testing.T) {
	t.Parallel()
	for _, tt := range []struct {
		name               string
		readyAt            time.Duration
		tuning             bool
		cancel             bool
		canceledBeforePoll bool
		wait               time.Duration
		stop               StopReason
		trial              bool
	}{
		{name: "already armed", tuning: true, trial: true},
		{name: "late arm", readyAt: 3 * time.Second, tuning: true, wait: 3 * time.Second, trial: true},
		{name: "deadline arm", readyAt: 30 * time.Second, tuning: true, wait: 30 * time.Second, trial: true},
		{name: "too late", readyAt: 31 * time.Second, tuning: true, wait: 30 * time.Second, stop: StopDeadEnd},
		{name: "cancel waiting", readyAt: time.Hour, tuning: true, cancel: true, wait: time.Second, stop: StopSignal},
		{name: "cancel before polling", readyAt: time.Hour, tuning: true, cancel: true, canceledBeforePoll: true, stop: StopSignal},
		{name: "manual run", readyAt: time.Hour, trial: true},
	} {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			clock := &watchdogClock{}
			if tt.cancel {
				clock.cancel = cancel
			}
			m := newSim(t, small()).Seams()
			m.Clock = clock
			m.Host = watchdogHost{Host: m.Host, clock: clock, readyAt: tt.readyAt}
			boot, err := m.Host.BootID()
			if err != nil {
				t.Fatal(err)
			}
			dir := t.TempDir()
			j, err := journal.Open(dir, journal.Options{Boot: boot, Now: clock.Now, Monotonic: clock.Monotonic, Build: Build()})
			if err != nil {
				t.Fatal(err)
			}
			t.Cleanup(func() {
				if err := j.Close(); err != nil {
					t.Error(err)
				}
			})
			in := Input{Config: config.Default(), Boot: boot, Journal: stopBeforeTrial{j}, Machine: m}
			bl := &fakeBootloader{}
			if tt.tuning {
				in.Bootloader = bl
			}
			if tt.canceledBeforePoll {
				cancel()
			}
			stop, err := Run(ctx, in)
			if tt.trial {
				if !errors.Is(err, errKilled) {
					t.Fatalf("run: %v, want trial boundary", err)
				}
			} else if err != nil || stop.Reason != tt.stop {
				t.Fatalf("stop %+v, error %v, want %s", stop, err, tt.stop)
			}
			if diff := cmp.Diff(tt.wait, clock.now); diff != "" {
				t.Fatalf("wait (-want +got): %s", diff)
			}
			var checks []bool
			trial := false
			for _, e := range j.Events() {
				if p, ok := e.Data.(*journal.PreflightCheck); ok && p.Check == "watchdog" {
					checks = append(checks, p.OK)
				}
				if e.Kind == journal.KindSMUIntent || e.Kind == journal.KindTrialIntent {
					if !tt.trial || time.Duration(e.Mono)*time.Millisecond < tt.wait {
						t.Fatalf("unsafe action before watchdog: %+v", e)
					}
				}
				trial = trial || e.Kind == journal.KindTrialIntent
			}
			if trial != tt.trial {
				t.Fatalf("trial reached %v, want %v", trial, tt.trial)
			}
			var wantChecks []bool
			if tt.tuning && !tt.cancel {
				wantChecks = []bool{tt.trial}
			}
			if diff := cmp.Diff(wantChecks, checks); diff != "" {
				t.Fatalf("watchdog result (-want +got): %s", diff)
			}
			if tt.stop == StopDeadEnd && (stop.DeadEnd.Condition != journal.DeadEndPreflight || bl.calls != 1) {
				t.Fatalf("timeout did not return to normal boot: stop %+v, clears %d", stop, bl.calls)
			}
		})
	}
}
