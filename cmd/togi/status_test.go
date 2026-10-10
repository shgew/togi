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

	"github.com/charmbracelet/x/ansi"
	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/watch/watchtest"
)

func installStatusFixture(t *testing.T, dir, name string) journal.Build {
	t.Helper()
	build := watchtest.Install(t, dir, name)
	currentJournalCopy(t, dir)
	build.Schema = journal.Schema
	return build
}

func TestStatus(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	var stdout, stderr bytes.Buffer
	installStatusFixture(t, dir, "concluded")
	_, st, _, err := replayDir(dir)
	if err != nil {
		t.Fatal(err)
	}

	stdout.Reset()
	if code := cli([]string{"--state-dir", dir, "status"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("status: exit %d, stderr %s", code, stderr.String())
	}
	status := stdout.String()
	if want := fmt.Sprintf("phase 2 concluded: clean cycles %d, latest cycle %d", st.Checking.CleanCycles, st.Checking.LastCleanCycle); !strings.Contains(strings.Join(strings.Fields(status), " "), want) {
		t.Fatalf("status lacks %q:\n%s", want, status)
	}
	checkRows(t, "status", status, regexp.MustCompile(`(?m)^(\d\d)  +\d  +\d  +(-?\d+)  +(-?\d+\*?|-)  `), st)
	golden(t, "status", status)
}

func TestHistoricalTierChangeReadOnlyViews(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	installStatusFixture(t, dir, "concluded")
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
	build := installStatusFixture(t, dir, "concluded")
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
			installStatusFixture(t, dir, tc.name)
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

func phase2Status() journal.State {
	return journal.State{
		Session: &journal.SessionInfo{ID: "20260101T000000Z", Start: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)},
		Phase:   string(journal.PhaseDeepening),
		Cores: []journal.CoreState{
			{Core: 3, CCD: 0, Offset: -9, Phase: journal.PhaseAtLimit},
			{Core: 5, CCD: 0, Offset: -6, Phase: journal.PhaseAtLimit},
		},
		Phases: &journal.PhasesState{
			Phase: 2, Phase1End: 30, RoundsLeft: 3,
			Candidates: []journal.CandidateState{
				{Core: 3, Offset: -9, SoloLimit: -12, Gap: 3, Moving: true},
				{Core: 5, Offset: -6, SoloLimit: -7, Gap: 1, Moving: true},
			},
		},
		BIOS: &journal.BIOSState{Offsets: []int{-9, -6}, Confirmed: 30},
		Deepening: &journal.DeepeningState{
			Round: 2, Seq: 42, Target: []int{-10, -7}, Profile: []int{-9, -6}, Cores: []int{3, 5},
			Checks: []journal.CheckState{
				{Regime: machine.R1, Workload: "mprime-sse-4k-21k", Cores: []int{3}, Passes: 5, Needed: 5},
				{Regime: machine.R2, Workload: "mprime-avx2-36k-248k", Cores: []int{3}, Passes: 2, Needed: 5},
			},
		},
	}
}

func TestStatusPhases(t *testing.T) {
	t.Parallel()
	unconfirmed := phase2Status()
	unconfirmed.Deepening = nil
	unconfirmed.Phase = string(journal.PhaseChecking)
	unconfirmed.Phases = &journal.PhasesState{Phase: 2, Phase1End: 30, Confirming: true}
	unconfirmed.BIOS = &journal.BIOSState{Offsets: []int{-8, -6}, Confirmed: 30, Unconfirmed: []int{3}, Since: 44}
	phase1 := phase2Status()
	phase1.Deepening = nil
	phase1.Phase = string(journal.PhaseSearch)
	phase1.Phases = &journal.PhasesState{Phase: 1}
	phase1.BIOS = nil
	for _, tc := range []struct {
		name  string
		st    journal.State
		wants []string
	}{
		{"phase2", phase2Status(), []string{"phase 2 round 2, checks 1/2 | phase 2: at most 3 rounds left, then one full cycle", "BIOS profile confirmed by the passed full cycle [#30]"}},
		{"phase1", phase1, []string{"| phase 1", "BIOS profile: none confirmed yet"}},
		{"unconfirmed", unconfirmed, []string{"phase 2: confirmation cycle", "-8*", "unconfirmed"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			var out bytes.Buffer
			writeStatus(&out, tc.st, nil)
			flat := strings.Join(strings.Fields(out.String()), " ")
			for _, want := range tc.wants {
				if !strings.Contains(flat, want) {
					t.Errorf("status lacks %q:\n%s", want, out.String())
				}
			}
			golden(t, "status-"+tc.name, out.String())
		})
	}
}

func TestStatusShowsUnresetDefectResetCommands(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	var stdout, stderr bytes.Buffer
	build := installStatusFixture(t, dir, "concluded")
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
				st.Checking = &journal.CheckingState{Cycle: 4, Steps: []machine.Regime{machine.R1}, Profile: []int{-9, -8}, Missing: []string{"core 03 has no R1 pass"}}
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
	p := &journal.TrialIntent{Trial: "partial", Cores: []int{1, 2, 3, 4, 5, 6, 7}, RecordOnly: true, Step: 1, Cycle: 1, Regime: machine.R7, Workload: "AVX2", Condition: machine.Together, Phase: journal.PhaseChecking, DurationS: 120}
	st := journal.State{
		Session:  &journal.SessionInfo{ID: "s1", Start: time.Unix(100, 0).UTC()},
		Phase:    string(journal.PhaseChecking),
		Checking: &journal.CheckingState{Cycle: 1, CycleOpen: true, Steps: []machine.Regime{machine.R7}},
		InFlight: &journal.InFlight{Seq: 20, Msg: p.Message()},
	}
	var out bytes.Buffer
	writeStatus(&out, st, nil)
	if !strings.Contains(out.String(), "record-only") || !strings.Contains(out.String(), "cores 01, 02, 03, 04, 05, 06, 07") {
		t.Fatalf("status did not identify the partial: %s", out.String())
	}
}

func TestWriteStatusWrapsWideBIOS(t *testing.T) {
	t.Parallel()
	board := strings.TrimSpace(strings.Repeat("日本 ", 20))
	st := journal.State{
		Session: &journal.SessionInfo{
			ID: "s1", Start: time.Unix(100, 0).UTC(),
			BIOSContext: &machine.BIOSContext{BIOSVersion: "b", Board: board, CPUModel: "c", Microcode: "m"},
		},
	}
	var out bytes.Buffer
	writeStatus(&out, st, nil)
	for line := range strings.SplitSeq(strings.TrimSuffix(out.String(), "\n"), "\n") {
		if cells := ansi.StringWidth(line); cells > statusWidth {
			t.Errorf("status line uses %d cells, want at most %d: %q", cells, statusWidth, line)
		}
	}
	_, bios, ok := strings.Cut(out.String(), "\nBIOS ")
	if !ok {
		t.Fatalf("status has no BIOS context:\n%s", out.String())
	}
	bios, _, ok = strings.Cut(bios, "\nin flight: none\n")
	if !ok {
		t.Fatalf("status has no end to BIOS context:\n%s", out.String())
	}
	lines := strings.Split("BIOS "+bios, "\n")
	if len(lines) < 2 {
		t.Fatalf("wide BIOS context did not wrap:\n%s", bios)
	}
	for _, line := range lines[1:] {
		if !strings.HasPrefix(line, "  ") || strings.HasPrefix(line, "   ") {
			t.Errorf("BIOS continuation is not indented two cells: %q", line)
		}
	}
	want := fmt.Sprintf("BIOS b on %s, c, microcode m, boost limit 0 MHz", board)
	if diff := cmp.Diff(want, strings.Join(strings.Fields(strings.Join(lines, " ")), " ")); diff != "" {
		t.Fatalf("wrapped BIOS context changed text (-want +got):\n%s", diff)
	}
}

func TestWriteStatusWrapsWideDecisionNotes(t *testing.T) {
	t.Parallel()
	queued := strings.TrimSpace(strings.Repeat("日本 ", 20))
	decision := strings.TrimSpace(strings.Repeat("日本 ", 24))
	st := journal.State{
		Session: &journal.SessionInfo{ID: "s1", Start: time.Unix(100, 0).UTC()},
		Cores: []journal.CoreState{
			{Core: 3, CCD: 0, Offset: -9, Phase: journal.PhaseAtLimit, Queued: queued, LastDecision: &journal.DecisionRef{Seq: 42, Msg: decision}},
			{Core: 7, CCD: 0, Offset: -8, Phase: journal.PhaseAtLimit, LastDecision: &journal.DecisionRef{Seq: 43, Msg: "next core"}},
		},
	}
	var out bytes.Buffer
	writeStatus(&out, st, nil)
	for line := range strings.SplitSeq(strings.TrimSuffix(out.String(), "\n"), "\n") {
		if cells := ansi.StringWidth(line); cells > statusWidth {
			t.Errorf("status line uses %d cells, want at most %d: %q", cells, statusWidth, line)
		}
	}
	const prefix = "  last decision "
	before, note, ok := strings.Cut(out.String(), "\n"+prefix+"[#42] ")
	if !ok {
		t.Fatalf("status has no core 03 decision:\n%s", out.String())
	}
	rowStart := strings.LastIndex(before, "\n03 ")
	if rowStart < 0 {
		t.Fatalf("decision has no preceding core 03 row:\n%s", before)
	}
	rowLines := strings.Split(before[rowStart+1:], "\n")
	if len(rowLines) < 2 {
		t.Fatalf("wide core row did not wrap:\n%s", before[rowStart+1:])
	}
	for _, line := range rowLines[1:] {
		if !strings.HasPrefix(line, "  ") || strings.HasPrefix(line, "   ") {
			t.Errorf("core row continuation is not indented two cells: %q", line)
		}
	}
	wantRow := "03 0 3 -9 - AT LIMIT - " + queued
	if diff := cmp.Diff(wantRow, strings.Join(strings.Fields(strings.Join(rowLines, " ")), " ")); diff != "" {
		t.Fatalf("wrapped core row changed values (-want +got):\n%s", diff)
	}
	note, nextRow, ok := strings.Cut(note, "\n07 ")
	if !ok {
		t.Fatalf("core 03 decision has no following core 07 row:\n%s", out.String())
	}
	noteLines := strings.Split(note, "\n")
	if len(noteLines) < 2 {
		t.Fatalf("wide decision did not wrap:\n%s", note)
	}
	indent := strings.Repeat(" ", ansi.StringWidth(prefix))
	for _, line := range noteLines[1:] {
		if !strings.HasPrefix(line, indent) || strings.HasPrefix(line, indent+" ") {
			t.Errorf("decision continuation does not align with its value: %q", line)
		}
	}
	if diff := cmp.Diff(decision, strings.Join(strings.Fields(note), " ")); diff != "" {
		t.Fatalf("wrapped decision changed text (-want +got):\n%s", diff)
	}
	if !strings.Contains(nextRow, "\n"+prefix+"[#43] next core\n") {
		t.Fatalf("core 07 lost its own decision:\n%s", nextRow)
	}
}

func TestWriteStatusPreservesUnicodeWords(t *testing.T) {
	t.Parallel()
	const prefix = "  last decision "
	const first = prefix + "[#42] "
	rest := strings.Repeat(" ", ansi.StringWidth(prefix))
	for _, tc := range []struct {
		name     string
		word     string
		overlong bool
	}{
		{name: "wide exact fit", word: strings.Repeat("界", (statusWidth-ansi.StringWidth(first))/2)},
		{name: "combining exact fit", word: strings.Repeat("e\u0301", statusWidth-ansi.StringWidth(first))},
		{name: "overlong word", word: strings.Repeat("界", statusWidth/2+1), overlong: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			st := journal.State{
				Session: &journal.SessionInfo{ID: "s1", Start: time.Unix(100, 0).UTC()},
				Cores: []journal.CoreState{
					{Core: 3, CCD: 0, Offset: -9, Phase: journal.PhaseAtLimit, LastDecision: &journal.DecisionRef{Seq: 42, Msg: tc.word + " tail"}},
				},
			}
			var out bytes.Buffer
			writeStatus(&out, st, nil)
			_, note, ok := strings.Cut(out.String(), "\n"+prefix)
			if !ok {
				t.Fatalf("status has no decision:\n%s", out.String())
			}
			want := first + tc.word + "\n" + rest + "tail\n"
			if tc.overlong {
				want = strings.TrimSuffix(first, " ") + "\n" + rest + tc.word + "\n" + rest + "tail\n"
			} else if cells := ansi.StringWidth(first + tc.word); cells != statusWidth {
				t.Fatalf("exact-fit fixture uses %d cells, want %d", cells, statusWidth)
			}
			got, _, _ := strings.Cut(prefix+note, "tail\n")
			if diff := cmp.Diff(want, got+"tail\n"); diff != "" {
				t.Fatalf("word wrapping changed words or alignment (-want +got):\n%s", diff)
			}
			for line := range strings.SplitSeq(strings.TrimSuffix(out.String(), "\n"), "\n") {
				if tc.overlong && strings.Contains(line, tc.word) {
					continue
				}
				if cells := ansi.StringWidth(line); cells > statusWidth {
					t.Errorf("status line uses %d cells, want at most %d: %q", cells, statusWidth, line)
				}
			}
		})
	}
}
