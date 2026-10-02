package main

import (
	"bytes"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
)

func TestCarriedEventsLeaveStatusUnchanged(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	j, err := journal.Open(dir, journal.Options{Boot: "current", Build: session.Build(), Now: func() time.Time { return time.Unix(100, 0).UTC() }})
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	for _, payload := range []journal.Payload{
		&journal.SessionStart{Build: session.Build(), Session: "current", Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}}},
		&journal.SessionBaseline{Offsets: []int{0, 0}},
		&journal.CorePhase{Core: 0, To: journal.PhaseSearch, Offset: -10},
		&journal.CorePhase{Core: 1, To: journal.PhaseSearch, Offset: -20},
	} {
		if _, err := j.Append(payload); err != nil {
			t.Fatal(err)
		}
	}
	var before, diagnostics bytes.Buffer
	if code := cli([]string{"--state-dir", dir, "status"}, &before, &diagnostics); code != exitOK {
		t.Fatalf("status: exit %d: %s", code, diagnostics.String())
	}
	var trialRaw, failureRaw string
	for _, payload := range []journal.Payload{
		&journal.TrialCarried{Source: journal.FactSource{Session: "original", Seq: 304, Build: session.Build(), Trial: "0304", Evidence: 1}, Class: journal.TrialClass{Regime: machine.R7, Workload: "workload", Cores: []int{0}, DurationS: 120}, Profile: []int{-50, -50}, Condition: machine.Resident, Phase: journal.PhaseGuard, Outcome: journal.OutcomePass, DurationS: 120},
		&journal.FailureCarried{Source: journal.FactSource{Session: "original", Seq: 400, Build: session.Build(), Evidence: 1}, Class: journal.TrialClass{Regime: machine.R6, Cores: []int{0, 1}}, Signal: machine.Crash, Attribution: journal.Unattributed, Condition: machine.Resident, Profile: []int{-50, -50}},
	} {
		e, err := j.Append(payload)
		if err != nil {
			t.Fatal(err)
		}
		if e.Kind == journal.KindTrialCarried {
			trialRaw = string(e.Raw) + "\n"
		} else {
			failureRaw = string(e.Raw) + "\n"
		}
	}
	var after bytes.Buffer
	diagnostics.Reset()
	if code := cli([]string{"--state-dir", dir, "status"}, &after, &diagnostics); code != exitOK {
		t.Fatalf("status: exit %d: %s", code, diagnostics.String())
	}
	if diff := cmp.Diff(before.String(), after.String()); diff != "" {
		t.Fatalf("carried facts changed live status: %s", diff)
	}
	for _, tc := range []struct {
		selector string
		want     string
	}{
		{"trial", trialRaw},
		{"trial.carried", trialRaw},
		{"failure", failureRaw},
		{"failure.carried", failureRaw},
		{"trial,failure", trialRaw + failureRaw},
	} {
		t.Run(tc.selector, func(t *testing.T) {
			var out, diagnostics bytes.Buffer
			if code := cli([]string{"--state-dir", dir, "events", "--json", "--kind", tc.selector}, &out, &diagnostics); code != exitOK {
				t.Fatalf("events: exit %d: %s", code, diagnostics.String())
			}
			if diff := cmp.Diff(tc.want, out.String()); diff != "" {
				t.Fatalf("carried kind selection: %s", diff)
			}
		})
	}
}
