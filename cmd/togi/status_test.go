package main

import (
	"bytes"
	"compress/gzip"
	"fmt"
	"io"
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
	"github.com/shgew/togi/internal/watch"
)

func TestStatus(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	var stdout, stderr bytes.Buffer
	renderFixture(t, dir, "concluded")
	_, st, _, err := replayDir(dir)
	if err != nil {
		t.Fatal(err)
	}

	stdout.Reset()
	if code := cli([]string{"--state-dir", dir, "status"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("status: exit %d, stderr %s", code, stderr.String())
	}
	status := stdout.String()
	if want := fmt.Sprintf("qualified rotations since last deepening: %d, latest rotation %d", st.Guard.CleanRotations, st.Guard.LastQualifiedRotation); !strings.Contains(status, want) {
		t.Fatalf("status lacks %q:\n%s", want, status)
	}
	checkRows(t, "status", status, regexp.MustCompile(`(?m)^(\d\d)  +\d  +\d  +(-?\d+)  `), st)
	golden(t, "status", status)
}

func TestHistoricalTierChangeReadOnlyViews(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	renderFixture(t, dir, "concluded")
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
	build := renderFixture(t, dir, "concluded")
	_, before, _, err := replayDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	j, err := journal.Open(dir, journal.Options{Boot: "between-trials", Build: build})
	if err != nil {
		t.Fatal(err)
	}
	e, err := j.Append(&journal.MCE{CPU: 0, Core: 0, Corrected: true, BetweenTrials: true, Lines: []string{"between-trial hardware error"}})
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
	events, after, _, err := replayDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(before.Cores, after.Cores); diff != "" {
		t.Fatalf("between-trial MCE changed tuning state: %s", diff)
	}
	frame := ansi.Strip(watch.Render(watch.Project(events), 240, 67, e.Time))
	if !strings.Contains(frame, "between trials (recorded only)") {
		t.Fatalf("watch hides between-trial MCE: %s", frame)
	}
}

func TestStatusJointMarkAndOpenHunt(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name  string
		until journal.Kind
	}{
		{name: "hunt", until: journal.KindHuntMask},
		{name: "mark"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			renderFixture(t, dir, tc.name)
			events, st, _, err := replayDir(dir)
			if err != nil {
				t.Fatal(err)
			}
			if tc.until == journal.KindHuntMask && (st.Hunt == nil || len(st.Hunt.Masks) == 0) {
				t.Fatal("hunt fixture has no open mask")
			}
			if tc.until == "" && len(st.JointMarks) == 0 {
				t.Fatal("joint fixture has no joint mark")
			}
			var out bytes.Buffer
			writeStatus(&out, st, events)
			golden(t, "status-"+tc.name, out.String())
			frameEvents := events
			if tc.until != "" {
				maskStarted := false
				for i, e := range events {
					if e.Kind == journal.KindHuntMask {
						maskStarted = true
					}
					if maskStarted && e.Kind == journal.KindTrialStart {
						frameEvents = events[:i+1]
						break
					}
				}
			}
			frame := watch.Render(watch.Project(frameEvents), 240, 67, frameEvents[len(frameEvents)-1].Time.Add(40*time.Second))
			golden(t, "watch-"+tc.name+"-240x67", ansi.Strip(frame)+"\n")
		})
	}
}

func TestStatusOpenRefinement(t *testing.T) {
	t.Parallel()
	st := journal.State{
		Session: &journal.SessionInfo{ID: "20260101T000000Z", Start: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)},
		Phase:   string(journal.PhaseRefine),
		Cores:   []journal.CoreState{{Core: 3, CCD: 0, Offset: -9, Phase: journal.PhaseDone}},
		Refine: &journal.RefineState{
			Round: 2, Seq: 42, Target: []int{-10}, Profile: []int{-9}, Cores: []int{3},
			Checks: []journal.CheckState{
				{Regime: machine.R1, Workload: "mprime-sse-4k-21k", Cores: []int{3}, Passes: 5, Needed: 5},
				{Regime: machine.R2, Workload: "mprime-avx2-36k-248k", Cores: []int{3}, Passes: 2, Needed: 5},
			},
		},
	}
	var out bytes.Buffer
	writeStatus(&out, st, nil)
	golden(t, "status-refine", out.String())
}

func TestStatusShowsUnresetDefectResetCommands(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	var stdout, stderr bytes.Buffer
	build := renderFixture(t, dir, "concluded")
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

func renderFixture(t *testing.T, dir, name string) journal.Build {
	t.Helper()
	f, err := os.Open(filepath.Join("testdata", "render-"+name+".jsonl.gz"))
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	r, err := gzip.NewReader(f)
	if err != nil {
		t.Fatal(err)
	}
	defer r.Close()
	out, err := os.Create(filepath.Join(dir, "events.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	defer out.Close()
	if _, err := io.Copy(out, r); err != nil {
		t.Fatal(err)
	}
	build, _, err := journal.Scan(dir)
	if err != nil {
		t.Fatal(err)
	}
	return build
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
