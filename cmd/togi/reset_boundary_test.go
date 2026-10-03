package main

import (
	"bytes"
	"os"
	"path/filepath"
	"testing"

	"github.com/shgew/togi/internal/carry"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
)

func resetBoundaryJournal(t *testing.T, dir, id string, build journal.Build, ctx machine.BIOSContext, soloLimit, failurePoint int) *journal.Journal {
	t.Helper()
	j, err := journal.Open(dir, journal.Options{Boot: "fixture"})
	if err != nil {
		t.Fatal(err)
	}
	for _, p := range []journal.Payload{
		&journal.SessionStart{Build: build, Session: id, Evidence: 1, Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}}},
		&journal.SessionContext{BIOSContext: ctx},
		&journal.TrialIntent{Trial: "0001", Core: new(0), Offset: new(soloLimit), Regime: machine.R1, Workload: machine.Workloads(machine.R1)[0].ID, Condition: machine.Alone, Phase: journal.PhaseSearch, DurationS: 90, Profile: []int{soloLimit, 0}},
		&journal.TrialEnd{Trial: "0001", Outcome: journal.OutcomePass, DurationS: 90},
		&journal.TrialIntent{Trial: "0002", Core: new(1), Offset: new(failurePoint), Regime: machine.R1, Workload: machine.Workloads(machine.R1)[0].ID, Condition: machine.Alone, Phase: journal.PhaseSearch, DurationS: 90, Profile: []int{0, failurePoint}},
		&journal.TrialEnd{Trial: "0002", Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, DurationS: 11},
		&journal.Failure{Trial: "0002", Core: new(1), Offset: new(failurePoint), Attribution: journal.Attributed, Condition: machine.Alone, Signal: machine.ComputationError},
	} {
		if _, err := j.Append(p); err != nil {
			t.Fatal(err)
		}
	}
	return j
}

func TestResetAllPathsCannotReviveFactsSoloLimitsOrFailurePoints(t *testing.T) {
	for _, path := range []string{"normal", "pending before journal", "pending empty journal", "incompatible schema", "recovered incompatible archive", "recorded incompatible archive"} {
		t.Run(path, func(t *testing.T) {
			dir := t.TempDir()
			oldID, newID := "20261001T000000Z", "20261002T000000Z"
			ctx := machine.BIOSContext{Board: "board", BIOSVersion: "bios", CPUModel: "cpu", Microcode: "microcode", BoostLimitMHz: 5700}
			oldBuild := session.Build()
			oldBuild.Ruleset--
			if path == "incompatible schema" || path == "recovered incompatible archive" || path == "recorded incompatible archive" {
				oldBuild.Schema++
			}
			j := resetBoundaryJournal(t, dir, oldID, oldBuild, ctx, -30, -5)
			if path == "recorded incompatible archive" {
				if _, err := j.Append(&journal.SessionArchived{Session: oldID, Path: filepath.Join("archive", oldID+".jsonl")}); err != nil {
					t.Fatal(err)
				}
			}
			if err := j.Close(); err != nil {
				t.Fatal(err)
			}
			archive := filepath.Join(dir, "archive")
			if err := os.MkdirAll(archive, 0o755); err != nil {
				t.Fatal(err)
			}
			if path == "pending before journal" || path == "pending empty journal" || path == "recovered incompatible archive" {
				if err := os.Rename(filepath.Join(dir, "events.jsonl"), filepath.Join(archive, oldID+".jsonl")); err != nil {
					t.Fatal(err)
				}
				suffix := "-carry-pending"
				if path == "recovered incompatible archive" {
					suffix = "-compat-pending"
				}
				if err := os.WriteFile(filepath.Join(archive, oldID+suffix), nil, 0o644); err != nil {
					t.Fatal(err)
				}
				if path == "pending empty journal" {
					if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), nil, 0o644); err != nil {
						t.Fatal(err)
					}
				}
			}
			var out, diagnostics bytes.Buffer
			g := &globals{stateDir: dir, config: filepath.Join(t.TempDir(), "missing.toml"), hostLockPath: filepath.Join(t.TempDir(), "host.lock")}
			if code := runReset(g, []string{"--all"}, &out, &diagnostics); code != exitOK {
				t.Fatalf("reset exit %d: %s", code, diagnostics.String())
			}
			boundary, err := journal.ResetBoundary(dir)
			if err != nil || boundary != oldID {
				t.Fatalf("reset boundary %q, error %v", boundary, err)
			}
			// A later ruleset transition must reuse only the fresh session's evidence.
			j = resetBoundaryJournal(t, dir, newID, session.Build(), ctx, -10, -20)
			if err := j.Close(); err != nil {
				t.Fatal(err)
			}
			j, err = journal.Lock(dir, journal.Options{})
			if err != nil {
				t.Fatal(err)
			}
			defer j.Close()
			build := session.Build()
			build.Ruleset++
			c, err := carry.Prepare(j, build, nil, &ctx)
			if err != nil {
				t.Fatal(err)
			}
			if c == nil || len(c.Facts) != 2 || len(c.Cores) != 2 {
				t.Fatalf("fresh session evidence: %+v", c)
			}
			for _, f := range c.Facts {
				if f.Session != newID {
					t.Fatalf("reset revived old fact: %+v", f)
				}
			}
			for _, core := range c.Cores {
				if core.SoloLimit != nil && core.SoloLimitSession != newID || core.FailurePoint != nil && core.FailurePointSession != newID {
					t.Fatalf("reset revived old solo limit/failure point: %+v", core)
				}
			}
		})
	}
}
