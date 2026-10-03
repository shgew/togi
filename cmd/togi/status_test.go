package main

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/watch/watchtest"
)

func TestStatus(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	var stdout, stderr bytes.Buffer
	watchtest.Install(t, dir, "concluded")
	_, st, _, err := replayDir(dir)
	if err != nil {
		t.Fatal(err)
	}

	stdout.Reset()
	if code := cli([]string{"--state-dir", dir, "status"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("status: exit %d, stderr %s", code, stderr.String())
	}
	status := stdout.String()
	if want := fmt.Sprintf("clean laps since last deepening: %d, latest lap %d", st.Checking.CleanLaps, st.Checking.LastCleanLap); !strings.Contains(status, want) {
		t.Fatalf("status lacks %q:\n%s", want, status)
	}
	checkRows(t, "status", status, regexp.MustCompile(`(?m)^(\d\d)  +\d  +\d  +(-?\d+)  `), st)
	golden(t, "status", status)
}

func TestHistoricalTierChangeReadOnlyViews(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	watchtest.Install(t, dir, "concluded")
	events, st, _, err := replayDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	var before bytes.Buffer
	writeStatus(&before, st, events)
	raw := fmt.Sprintf(`{"seq":%d,"time":"2026-10-02T02:00:00Z","boot":"historical","kind":"tier.change","msg":"historical tier earned","tier":"bronze","clean_s":3600}`, events[len(events)-1].Seq+1)
	f, err := os.OpenFile(filepath.Join(dir, "events.jsonl"), os.O_APPEND|os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := fmt.Fprintln(f, raw); err != nil {
		_ = f.Close()
		t.Fatal(err)
	}
	if err := f.Close(); err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		command []string
		want    string
		exact   bool
	}{
		{command: []string{"status"}, want: before.String(), exact: true},
		{command: []string{"events"}, want: "historical tier earned"},
		{command: []string{"events", "--json"}, want: raw + "\n"},
	} {
		var out, diagnostics bytes.Buffer
		args := append([]string{"--state-dir", dir}, tc.command...)
		if code := cli(args, &out, &diagnostics); code != exitOK {
			t.Fatalf("%v: exit %d: %s", tc.command, code, diagnostics.String())
		}
		if tc.exact {
			if diff := cmp.Diff(tc.want, out.String()); diff != "" {
				t.Fatalf("historical tier changed status (-want +got):\n%s", diff)
			}
		} else if !strings.Contains(out.String(), tc.want) {
			t.Fatalf("%v hides historical event %q:\n%s", tc.command, tc.want, out.String())
		}
	}
}

func TestBetweenTrialMCEReadOnlyViews(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	build := watchtest.Install(t, dir, "concluded")
	_, before, _, err := replayDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	j, err := journal.Open(dir, journal.Options{Boot: "between-trials", Build: build})
	if err != nil {
		t.Fatal(err)
	}
	_, err = j.Append(&journal.MCE{CPU: 0, Core: 0, Corrected: true, BetweenTrials: true, Lines: []string{"between-trial hardware error"}})
	if err != nil {
		t.Fatal(err)
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	for _, command := range []string{"status", "events"} {
		var out, diagnostics bytes.Buffer
		if code := cli([]string{"--state-dir", dir, command}, &out, &diagnostics); code != exitOK {
			t.Fatalf("%s: exit %d: %s", command, code, diagnostics.String())
		}
		if !strings.Contains(out.String(), "between trials (recorded only)") {
			t.Fatalf("%s hides between-trial MCE: %s", command, out.String())
		}
	}
	_, after, _, err := replayDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(before.Cores, after.Cores); diff != "" {
		t.Fatalf("between-trial MCE changed tuning state: %s", diff)
	}
}

func TestStatusCombinationAndOpenHunt(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name  string
		until journal.Kind
	}{
		{name: "hunt", until: journal.KindHuntGroup},
		{name: "combination"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			watchtest.Install(t, dir, tc.name)
			events, st, _, err := replayDir(dir)
			if err != nil {
				t.Fatal(err)
			}
			if tc.until == journal.KindHuntGroup && (st.Hunt == nil || len(st.Hunt.Groups) == 0) {
				t.Fatal("hunt fixture has no open group")
			}
			if tc.until == "" && len(st.Combinations) == 0 {
				t.Fatal("combination fixture has no combination")
			}
			var out bytes.Buffer
			writeStatus(&out, st, events)
			golden(t, "status-"+tc.name, out.String())
		})
	}
}

func TestStatusOpenDeepening(t *testing.T) {
	t.Parallel()
	st := journal.State{
		Session: &journal.SessionInfo{ID: "20260101T000000Z", Start: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)},
		Phase:   string(journal.PhaseDeepening),
		Cores:   []journal.CoreState{{Core: 3, CCD: 0, Offset: -9, Phase: journal.PhaseAtLimit}},
		Deepening: &journal.DeepeningState{
			Round: 2, Seq: 42, Target: []int{-10}, Profile: []int{-9}, Cores: []int{3},
			Checks: []journal.CheckState{
				{Regime: machine.R1, Workload: "mprime-sse-4k-21k", Cores: []int{3}, Passes: 5, Needed: 5},
				{Regime: machine.R2, Workload: "mprime-avx2-36k-248k", Cores: []int{3}, Passes: 2, Needed: 5},
			},
		},
	}
	var out bytes.Buffer
	writeStatus(&out, st, nil)
	golden(t, "status-deepening", out.String())
}

func TestStatusShowsUnresetDefectResetCommands(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	var stdout, stderr bytes.Buffer
	build := watchtest.Install(t, dir, "concluded")
	record := func(payload journal.Payload) {
		t.Helper()
		j, err := journal.Open(dir, journal.Options{Boot: "status-test", Build: build})
		if err != nil {
			t.Fatal(err)
		}
		if _, err := j.Append(payload); err != nil {
			t.Fatal(err)
		}
		if err := j.Close(); err != nil {
			t.Fatal(err)
		}
	}
	status := func() string {
		t.Helper()
		stdout.Reset()
		stderr.Reset()
		if code := cli([]string{"--state-dir", dir, "status"}, &stdout, &stderr); code != exitOK {
			t.Fatalf("status: exit %d, stderr %s", code, stderr.String())
		}
		return stdout.String()
	}
	check := func(want []string, absent []string) {
		t.Helper()
		out := status()
		for _, text := range want {
			if !strings.Contains(out, text) {
				t.Fatalf("status lacks %q:\n%s", text, out)
			}
		}
		for _, text := range absent {
			if strings.Contains(out, text) {
				t.Fatalf("status unexpectedly shows %q:\n%s", text, out)
			}
		}
	}
	title := "defect 1: False failure at power-off"
	core3, core7 := "togi reset --core 3", "togi reset --core 7"
	record(&journal.DefectFound{ID: 1, Title: "False failure at power-off", PR: 16, Direction: "too_cautious", Cores: []int{3, 7}, Decisions: []int{42}})
	check([]string{title, core3, core7}, nil)
	record(&journal.DefectAnswered{ID: 1, Cores: []int{3, 7}, Answer: "no"})
	check([]string{title, core3, core7}, nil)
	record(&journal.CommandReset{Core: new(3)})
	check([]string{title, core7, "affected cores [7]"}, []string{core3})
	record(&journal.CommandReset{Core: new(7)})
	check(nil, []string{title, core3, core7})
	record(&journal.DefectFound{ID: 1, Title: "False failure at power-off", PR: 16, Direction: "too_cautious", Cores: []int{3, 7}, Decisions: []int{42}})
	record(&journal.CommandReset{All: true})
	check(nil, []string{title, core3, core7})
}

func checkRows(t *testing.T, name, out string, row *regexp.Regexp, st journal.State) {
	t.Helper()
	rows := row.FindAllStringSubmatch(out, -1)
	if len(rows) != len(st.Cores) || len(rows) != 16 {
		t.Fatalf("%s: %d core rows, want 16:\n%s", name, len(rows), out)
	}
	for i, r := range rows {
		core, _ := strconv.Atoi(r[1])
		offset, _ := strconv.Atoi(r[2])
		if c := st.Cores[i]; core != c.Core || offset != c.Offset {
			t.Errorf("%s row %d: core %d offset %d, want core %d offset %d", name, i, core, offset, c.Core, c.Offset)
		}
	}
}

func TestStatusExceptionalActivity(t *testing.T) {
	t.Parallel()
	for _, name := range []string{"partial-checking", "dead-end", "parked-member-probe-group", "fallback-combination"} {
		t.Run(name, func(t *testing.T) {
			st := journal.State{
				Session: &journal.SessionInfo{ID: "s1", Start: time.Unix(100, 0).UTC()},
				Cores: []journal.CoreState{
					{Core: 3, CCD: 0, Offset: -9, Phase: journal.PhaseAtLimit},
					{Core: 7, CCD: 0, Offset: -8, Phase: journal.PhaseAtLimit},
				},
			}
			switch name {
			case "partial-checking":
				st.Phase = string(journal.PhaseChecking)
				st.Checking = &journal.CheckingState{Lap: 4, Steps: []machine.Regime{machine.R1}, Profile: []int{-9, -8}, Missing: []string{"core 03 has no R1 pass"}}
			case "dead-end":
				st.DeadEnd = &journal.DeadEndRef{Condition: journal.DeadEndNoEvidence, Seq: 42}
			case "parked-member-probe-group":
				st.Phase = string(journal.PhaseHunt)
				st.Cores[0].Offset = -10
				st.Hunt = &journal.HuntState{
					Hunt: 2, Seq: 40, Failure: 30, ParkedSeq: 20, Parked: []int{0, 0},
					Regime: machine.R7, Trial: "0001", Candidates: []int{3, 7},
					Groups: []journal.GroupState{{Group: 3, Probe: &journal.CombinationMember{Core: 3, Offset: -10}, Held: []journal.CombinationMember{{Core: 7, Offset: -8}}, Passes: 2, Needed: 5, Outcome: "running"}},
				}
			case "fallback-combination":
				st.Combinations = []journal.CombinationState{{Combination: 2, Hunt: 3, Seq: 42, Fallback: true, Members: []journal.CombinationMember{{Core: 3, Offset: -10}, {Core: 7, Offset: -8}}}}
				st.Cores[0].Combinations = []int{2}
				st.Cores[1].Combinations = []int{2}
			}
			var out bytes.Buffer
			var events []journal.Event
			if st.Hunt != nil {
				events = []journal.Event{{Seq: 30, Kind: journal.KindFailure, Data: &journal.Failure{Signal: machine.ComputationError}}}
			}
			writeStatus(&out, st, events)
			golden(t, "status-"+name, out.String())
		})
	}
}

func TestStatusRecordOnlyPartial(t *testing.T) {
	t.Parallel()
	p := &journal.TrialIntent{Trial: "partial", Cores: []int{1, 2, 3, 4, 5, 6, 7}, RecordOnly: true, Step: 1, Lap: 1, Regime: machine.R7, Workload: "AVX2", Condition: machine.Together, Phase: journal.PhaseChecking, DurationS: 120}
	st := journal.State{
		Session:  &journal.SessionInfo{ID: "s1", Start: time.Unix(100, 0).UTC()},
		Phase:    string(journal.PhaseChecking),
		Checking: &journal.CheckingState{Lap: 1, LapOpen: true, Steps: []machine.Regime{machine.R7}},
		InFlight: &journal.InFlight{Seq: 20, Msg: p.Message()},
	}
	var out bytes.Buffer
	writeStatus(&out, st, nil)
	if !strings.Contains(out.String(), "record-only") || !strings.Contains(out.String(), "cores 01, 02, 03, 04, 05, 06, 07") {
		t.Fatalf("status did not identify the partial: %s", out.String())
	}
}
