package main

import (
	"bytes"
	"context"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/render"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
)

const watchdogWarning = "check hardware watchdog: no active hardware watchdog (no hardware watchdog state available), so a freeze needs a manual reset; start sessions from the tuning boot, especially after a breaking update; continuing the session"

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
	dir    string
	warn   bool
	writes *int
	cancel context.CancelFunc
}

func (s warningObservedSMU) SetOffset(core, offset int) error {
	s.observe()
	return s.SMU.SetOffset(core, offset)
}

func (s warningObservedSMU) SetAllOffsets(offset int) error {
	s.observe()
	return s.SMU.SetAllOffsets(offset)
}

func (s warningObservedSMU) observe() {
	*s.writes++
	events, _, err := journal.Read(s.dir)
	if err != nil {
		s.t.Fatal(err)
	}
	if got := len(watchdogWarnings(events)) > 0; got != s.warn {
		s.t.Fatalf("warning journaled before offset write = %t, want %t", got, s.warn)
	}
	s.cancel()
}

func watchdogWarnings(events []journal.Event) []string {
	var warnings []string
	for _, e := range events {
		if p, ok := e.Data.(*journal.SessionWarning); ok && p.Operation == "check hardware watchdog" {
			warnings = append(warnings, e.Msg)
		}
	}
	return warnings
}

func TestRunWatchdogWarning(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("hardware runs need Linux")
	}
	for _, tt := range []struct {
		name      string
		active    bool
		tuning    bool
		dashboard bool
		warn      bool
		polls     int
		code      int
	}{
		{name: "manual without watchdog", warn: true, polls: 1},
		{name: "manual without watchdog on the dashboard", dashboard: true, warn: true, polls: 1},
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
			seams.SMU = warningObservedSMU{SMU: seams.SMU, t: t, dir: g.stateDir, warn: tt.warn, writes: &writes, cancel: cancel}
			newMachine := func(config.Config, string) (machine.Machine, error) { return seams, nil }
			var bootloader session.Bootloader
			if tt.tuning {
				bootloader = &clearingBootloader{}
			}
			var dash *dashboard
			if tt.dashboard {
				out, err := os.Create(filepath.Join(t.TempDir(), "screen"))
				if err != nil {
					t.Fatal(err)
				}
				defer out.Close()
				dash = &dashboard{dir: g.stateDir, out: out, run: func(ctx context.Context, _ string, _ *os.File) error {
					<-ctx.Done()
					return nil
				}}
			}
			code := runHardware(ctx, &g, config.Default(), false, bootloader, 0, &stderr, render.Renderer{}, dash, newMachine)
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
			}
			var wantChecks []bool
			if tt.tuning {
				wantChecks = []bool{tt.active}
			}
			if diff := cmp.Diff(wantChecks, checks); diff != "" {
				t.Fatalf("watchdog events (-want +got): %s", diff)
			}
			var wantWarnings []string
			if tt.warn {
				wantWarnings = []string{watchdogWarning}
			}
			if diff := cmp.Diff(wantWarnings, watchdogWarnings(events)); diff != "" {
				t.Fatalf("journaled warning (-want +got): %s", diff)
			}
			if diff := cmp.Diff(tt.warn && !tt.dashboard, strings.Contains(stderr.String(), watchdogWarning)); diff != "" {
				t.Fatalf("warning line on stderr (-want +got): %s; stderr: %s", diff, &stderr)
			}
		})
	}
}
