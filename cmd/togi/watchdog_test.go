package main

import (
	"bytes"
	"context"
	"runtime"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
)

type countedWatchdogHost struct {
	machine.Host
	polls *int
}

func (h countedWatchdogHost) Watchdog() machine.Check {
	*h.polls++
	return h.Host.Watchdog()
}

type warningObservedSMU struct {
	machine.SMU
	t      *testing.T
	stderr *bytes.Buffer
	warn   bool
	writes *int
	cancel context.CancelFunc
}

func (s warningObservedSMU) SetOffset(core, offset int) error {
	*s.writes++
	if got := strings.Contains(s.stderr.String(), "togi run: warning: no active hardware watchdog"); got != s.warn {
		s.t.Fatalf("warning before offset write = %t, want %t; stderr: %s", got, s.warn, s.stderr)
	}
	s.cancel()
	return s.SMU.SetOffset(core, offset)
}

func TestRunWatchdogWarning(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("hardware runs need Linux")
	}
	for _, tt := range []struct {
		name   string
		active bool
		tuning bool
		warn   bool
		polls  int
		code   int
	}{
		{name: "manual without watchdog", warn: true, polls: 1},
		{name: "manual active watchdog", active: true, polls: 1},
		{name: "tuning active watchdog", active: true, tuning: true, polls: 1},
		{name: "tuning without watchdog", tuning: true, polls: 31, code: exitPreflight},
	} {
		t.Run(tt.name, func(t *testing.T) {
			g := testGlobals(t)
			m, err := sim.New(sim.Config{Seed: 82, Cores: 2})
			if err != nil {
				t.Fatal(err)
			}
			if !tt.active {
				m.FailCheck("watchdog", "no hardware watchdog state available")
			}
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			var stderr bytes.Buffer
			polls, writes := 0, 0
			seams := m.Seams()
			seams.Host = countedWatchdogHost{Host: seams.Host, polls: &polls}
			seams.SMU = warningObservedSMU{SMU: seams.SMU, t: t, stderr: &stderr, warn: tt.warn, writes: &writes, cancel: cancel}
			newMachine := func(config.Config, string) (machine.Machine, error) { return seams, nil }
			var bootloader session.Bootloader
			if tt.tuning {
				bootloader = &clearingBootloader{}
			}
			code := runHardware(ctx, &g, config.Default(), false, bootloader, 0, &stderr, journal.Renderer{}, nil, newMachine)
			if diff := cmp.Diff(tt.code, code); diff != "" {
				t.Fatalf("exit (-want +got): %s; stderr: %s", diff, &stderr)
			}
			if diff := cmp.Diff(tt.polls, polls); diff != "" {
				t.Fatalf("watchdog polls (-want +got): %s", diff)
			}
			if diff := cmp.Diff(tt.code != exitPreflight, writes > 0); diff != "" {
				t.Fatalf("reached offset write (-want +got): %s", diff)
			}
			events, _, err := journal.Read(g.stateDir)
			if err != nil {
				t.Fatal(err)
			}
			var checks []bool
			for _, e := range events {
				if p, ok := e.Data.(*journal.PreflightCheck); ok && p.Check == "watchdog" {
					checks = append(checks, p.OK)
				}
				if e.Kind == journal.KindSessionWarning {
					t.Fatalf("advisory unexpectedly journaled: %+v", e)
				}
			}
			var wantChecks []bool
			if tt.tuning {
				wantChecks = []bool{tt.active}
			}
			if diff := cmp.Diff(wantChecks, checks); diff != "" {
				t.Fatalf("watchdog events (-want +got): %s", diff)
			}
			const warning = "togi run: warning: no active hardware watchdog (no hardware watchdog state available); a freeze needs a manual reset. Start sessions from the tuning boot, especially after a breaking update.\n"
			gotWarning := ""
			for line := range strings.SplitSeq(stderr.String(), "\n") {
				if strings.HasPrefix(line, "togi run: warning:") {
					gotWarning += line + "\n"
				}
			}
			wantWarning := ""
			if tt.warn {
				wantWarning = warning
				t.Log(strings.TrimSpace(gotWarning))
			}
			if diff := cmp.Diff(wantWarning, gotWarning); diff != "" {
				t.Fatalf("warning (-want +got): %s", diff)
			}
		})
	}
}
