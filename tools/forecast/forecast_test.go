package forecast

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/simrun"
	"github.com/shgew/togi/internal/tuner"
)

var epoch = time.Date(2026, 10, 7, 0, 0, 0, 0, time.UTC)

// session builds a configured journal whose events are an hour apart, numbered from seq.
func session(t *testing.T, id string, seq int, payloads ...journal.Payload) Session {
	t.Helper()
	s := Session{ID: id, Replayable: true}
	for i, p := range payloads {
		e := journal.Event{Seq: seq + i, Time: epoch.Add(time.Duration(seq+i) * time.Hour), Boot: "boot", Kind: p.Kind(), Msg: p.Message(), Data: p}
		raw, err := e.MarshalJSON()
		if err != nil {
			t.Fatal(err)
		}
		e.Raw = raw
		s.Events = append(s.Events, e)
	}
	return s
}

// extend appends events numbered from seq to a copy of s.
func extend(t *testing.T, s Session, seq int, payloads ...journal.Payload) Session {
	t.Helper()
	s.Events = append(slices.Clone(s.Events), session(t, s.ID, seq, payloads...).Events...)
	return s
}

func start(id string, rev string) []journal.Payload {
	return []journal.Payload{
		&journal.SessionStart{Session: id, Version: "1.0.0", Rev: rev, Schema: journal.Schema, Ruleset: tuner.Ruleset, Cores: []machine.CoreInfo{{Core: 0}, {Core: 1, CCD: 1}}},
		&journal.SessionBaseline{Offsets: []int{-10, -20}},
	}
}

func anchorAt(s Session, seq int) Anchor {
	for i, e := range s.Events {
		if e.Seq == seq {
			return Anchor{Session: s.ID, Seq: seq, Time: e.Time, SHA256: hashThrough(s.Events, i)}
		}
	}
	panic("no anchor event")
}

func TestAfter(t *testing.T) {
	crash := &journal.CrashDetected{PreviousBoot: "boot"}
	carriedHunt := []journal.Payload{&journal.HuntStart{Hunt: 1, Failure: 2}, &journal.HuntEnd{Hunt: 1, Result: "direct"}}
	before := session(t, "a", 1, append(start("a", "abc1234"), crash, &journal.TrialIntent{Trial: "0001", Hunt: 7})...)
	for _, tc := range []struct {
		name     string
		sessions func() []Session
		want     Outcome
	}{
		{
			name:     "nothing after the anchor",
			sessions: func() []Session { return []Session{before} },
			want:     Outcome{Status: Censored, Profile: []int{-10, -20}, Depth: -30},
		},
		{
			name: "events before the anchor are excluded",
			sessions: func() []Session {
				s := extend(t, before, 5, crash, crash, &journal.TrialIntent{Trial: "0002", Hunt: 8}, &journal.TrialIntent{Trial: "0003", Hunt: 8})
				return []Session{s}
			},
			want: Outcome{Status: Censored, Hours: 4, Crashes: 2, Hunts: 1, Profile: []int{-10, -20}, Depth: -30},
		},
		{
			name: "a hunt answered only by carried facts runs no trial",
			sessions: func() []Session {
				s := extend(t, before, 5, carriedHunt...)
				return []Session{s}
			},
			want: Outcome{Status: Censored, Hours: 2, Profile: []int{-10, -20}, Depth: -30},
		},
		{
			name: "concluded at the clean cycle the run stops at",
			sessions: func() []Session {
				s := extend(t, before, 5, crash, &journal.Shutdown{Reason: journal.ShutdownCycles, Cycles: 1})
				return []Session{s}
			},
			want: Outcome{Status: Concluded, Hours: 2, Crashes: 1, Profile: []int{-10, -20}, Depth: -30},
		},
		{
			name: "nothing after a cycles shutdown counts",
			sessions: func() []Session {
				s := extend(t, before, 5, crash, &journal.Shutdown{Reason: journal.ShutdownCycles, Cycles: 1}, crash, &journal.TrialIntent{Trial: "0002", Hunt: 8})
				next := session(t, "b", 1, &journal.SessionStart{Session: "b", Schema: journal.Schema, Ruleset: tuner.Ruleset, Cores: []machine.CoreInfo{{Core: 0}, {Core: 1, CCD: 1}}}, &journal.SessionBaseline{Offsets: []int{-12, -22}}, crash, &journal.TrialIntent{Trial: "0001", Hunt: 7}, &journal.DeadEnd{Condition: journal.DeadEndCondition("failure_at_zero")})
				return []Session{s, next}
			},
			want: Outcome{Status: Concluded, Hours: 2, Crashes: 1, Profile: []int{-10, -20}, Depth: -30},
		},
		{
			name: "a session after the terminal point need not be replayable",
			sessions: func() []Session {
				s := extend(t, before, 5, &journal.Shutdown{Reason: journal.ShutdownCycles, Cycles: 1})
				next := session(t, "b", 1, start("b", "abc1234")...)
				next.Replayable = false
				return []Session{s, next}
			},
			want: Outcome{Status: Concluded, Hours: 1, Profile: []int{-10, -20}, Depth: -30},
		},
		{
			name: "dead end",
			sessions: func() []Session {
				s := extend(t, before, 5, crash, &journal.DeadEnd{Condition: journal.DeadEndCondition("failure_at_zero")}, crash, &journal.Shutdown{Reason: journal.ShutdownCycles, Cycles: 1})
				return []Session{s}
			},
			want: Outcome{Status: DeadEnd, Hours: 2, Crashes: 1, Profile: []int{-10, -20}, Depth: -30},
		},
		{
			name: "a new session without a terminal point supplies the profile",
			sessions: func() []Session {
				next := session(t, "b", 1, &journal.SessionStart{Session: "b", Schema: journal.Schema, Ruleset: tuner.Ruleset, Cores: []machine.CoreInfo{{Core: 0}, {Core: 1, CCD: 1}}}, &journal.SessionBaseline{Offsets: []int{-12, -22}}, crash, &journal.TrialIntent{Trial: "0001", Hunt: 7})
				return []Session{before, next}
			},
			want: Outcome{Status: Censored, Hours: 0, Crashes: 1, Hunts: 1, Profile: []int{-12, -22}, Depth: -34},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got, err := After(tc.sessions(), anchorAt(before, 4))
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestAfterRequiresAReplayableNewestSession(t *testing.T) {
	s := session(t, "a", 1, start("a", "abc1234")...)
	anchor := anchorAt(s, 2)
	s.Replayable = false
	if _, err := After([]Session{s}, anchor); err == nil {
		t.Fatal("projected a session this build cannot replay")
	}
}

// simulate runs a simulated session in dir, resuming its journal, until it reaches cycles clean cycles (0 checks
// endlessly) or until accepts an event.
func simulate(t *testing.T, dir string, seed uint64, cycles int, until func(journal.Event) bool) {
	t.Helper()
	simulateNear(t, dir, seed, cycles, 0, until)
}

// simulateNear is simulate with a near-limit failure rate, so a profile that sits at its limits can still crash.
func simulateNear(t *testing.T, dir string, seed uint64, cycles int, near float64, until func(journal.Event) bool) {
	t.Helper()
	cfg, err := sim.Resume(dir, sim.Config{Seed: seed})
	if near > 0 {
		model := sim.DefaultModel()
		model.NearLimitRate = near
		cfg.Model = &model
	}
	if err != nil {
		t.Fatal(err)
	}
	m, err := sim.New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	_, err = simrun.Simulate(context.Background(), simrun.Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Log: io.Discard, Cycles: cycles, InMemoryJournal: true, Until: until})
	if err != nil && !errors.Is(err, simrun.ErrBootCap) {
		t.Fatal(err)
	}
}

func copyState(t *testing.T, src string) string {
	t.Helper()
	dst := t.TempDir()
	for _, name := range []string{"events.jsonl", "state.json"} {
		data, err := os.ReadFile(filepath.Join(src, name))
		if err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(dst, name), data, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	return dst
}

func isCycleStart(e journal.Event) bool {
	c, ok := e.Data.(*journal.CheckingCycle)
	return ok && c.Event == journal.CycleStart
}

const nearLimitRate = 1e-5

func TestAfterScoresAnEndlessRunAsACyclesOneRun(t *testing.T) {
	history := t.TempDir()
	simulate(t, history, 42, 1, func(e journal.Event) bool { return e.Kind == journal.KindCrashDetected })
	anchor, err := AnchorOf(history)
	if err != nil {
		t.Fatal(err)
	}
	stopped := copyState(t, history)
	starts := 0
	simulateNear(t, stopped, 31, 1, nearLimitRate, func(e journal.Event) bool {
		if isCycleStart(e) {
			starts++
		}
		return false
	})
	// A --cycles 1 run stops at the end of the confirmation cycle. The endless run goes on to check one more cycle,
	// which crashes with seed 31 and the near-limit rate, and stops at the start of the cycle after it.
	endless := copyState(t, history)
	seen := 0
	simulateNear(t, endless, 31, 0, nearLimitRate, func(e journal.Event) bool {
		if isCycleStart(e) {
			seen++
		}
		return seen == starts+2
	})
	outcome := func(dir string) (Outcome, []Session) {
		sessions, err := ReadDir(dir, anchor.Session)
		if err != nil {
			t.Fatal(err)
		}
		o, err := After(sessions, anchor)
		if err != nil {
			t.Fatal(err)
		}
		return o, sessions
	}
	want, _ := outcome(stopped)
	got, sessions := outcome(endless)
	if want.Status != Concluded {
		t.Fatalf("the --cycles 1 run did not conclude: %+v", want)
	}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatal(diff)
	}
	crashes := 0
	for _, s := range sessions {
		for _, e := range s.Events {
			if e.Kind == journal.KindCrashDetected && (s.ID != anchor.Session || e.Seq > anchor.Seq) {
				crashes++
			}
		}
	}
	if crashes <= got.Crashes {
		t.Fatalf("the endless run recorded %d crashes after the anchor, none after its conclusion", crashes)
	}
}

func TestReadDirRefusesAnotherRuleset(t *testing.T) {
	dir := t.TempDir()
	simulate(t, dir, 42, 1, func(e journal.Event) bool { return e.Kind == journal.KindTrialEnd })
	path := filepath.Join(dir, "events.jsonl")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	field := fmt.Appendf(nil, `"ruleset":%d,`, tuner.Ruleset)
	if !bytes.Contains(data, field) {
		t.Fatalf("journal records no %s", field)
	}
	data = bytes.ReplaceAll(data, field, fmt.Appendf(nil, `"ruleset":%d,`, tuner.Ruleset-1))
	if err := os.WriteFile(path, data, 0o644); err != nil {
		t.Fatal(err)
	}
	if _, _, err := journal.ReadFile(path); err != nil {
		t.Fatalf("read the journal of ruleset %d: %v", tuner.Ruleset-1, err)
	}
	sessions, err := ReadDir(dir, "")
	if err != nil {
		t.Fatal(err)
	}
	if len(sessions) != 1 || sessions[0].Replayable {
		t.Fatalf("read %d sessions, replayable %v", len(sessions), len(sessions) == 1 && sessions[0].Replayable)
	}
	anchor := anchorAt(sessions[0], sessions[0].Events[1].Seq)
	want := fmt.Sprintf("session %s, ruleset %d, cannot be replayed by this build's ruleset %d", sessions[0].ID, tuner.Ruleset-1, tuner.Ruleset)
	if _, err := After(sessions, anchor); err == nil || err.Error() != want {
		t.Errorf("After error %v, want %s", err, want)
	}
	record := Record{Anchor: anchor, Ruleset: tuner.Ruleset, Runs: []Run{{Status: Concluded}}}
	if _, err := record.Score(sessions); err == nil || err.Error() != want {
		t.Errorf("Score error %v, want %s", err, want)
	}
}

func TestSummarize(t *testing.T) {
	run := func(status string, hours float64, crashes, hunts int, profile ...int) Run {
		o := Outcome{Status: status, Hours: hours, Crashes: crashes, Hunts: hunts, Profile: profile}
		for _, p := range profile {
			o.Depth += p
		}
		return Run{Machine: "m.json", Outcome: o}
	}
	for _, tc := range []struct {
		name string
		runs []Run
		want Summary
	}{
		{
			name: "odd count",
			runs: []Run{run(Concluded, 30, 3, 1, -5, -9), run(Concluded, 10, 1, 0, -1, -7), run(Concluded, 20, 2, 2, -3, -8)},
			want: Summary{
				Runs: 3, Concluded: 3,
				Hours:   &Range{Min: 10, P10: 10, Median: 20, P90: 30, Max: 30},
				Crashes: Range{Min: 1, P10: 1, Median: 2, P90: 3, Max: 3},
				Hunts:   Range{Min: 0, P10: 0, Median: 1, P90: 2, Max: 2},
				Depth:   Range{Min: -14, P10: -14, Median: -11, P90: -8, Max: -8},
				Cores:   []Range{{Min: -5, P10: -5, Median: -3, P90: -1, Max: -1}, {Min: -9, P10: -9, Median: -8, P90: -7, Max: -7}},
			},
		},
		{
			name: "even count averages the middle two",
			runs: []Run{run(Concluded, 4, 0, 0, -1), run(Concluded, 1, 0, 0, -2), run(Concluded, 3, 0, 0, -4), run(Concluded, 2, 0, 0, -3)},
			want: Summary{
				Runs: 4, Concluded: 4,
				Hours: &Range{Min: 1, P10: 1, Median: 2.5, P90: 4, Max: 4},
				Depth: Range{Min: -4, P10: -4, Median: -2.5, P90: -1, Max: -1},
				Cores: []Range{{Min: -4, P10: -4, Median: -2.5, P90: -1, Max: -1}},
			},
		},
		{
			name: "censored and dead-end runs count everywhere but hours",
			runs: []Run{run(Concluded, 5, 1, 1, -6), run(Censored, 50, 9, 3, -2), run(DeadEnd, 7, 4, 2, 0)},
			want: Summary{
				Runs: 3, Concluded: 1, DeadEnds: 1, Censored: 1,
				Hours:   &Range{Min: 5, P10: 5, Median: 5, P90: 5, Max: 5},
				Crashes: Range{Min: 1, P10: 1, Median: 4, P90: 9, Max: 9},
				Hunts:   Range{Min: 1, P10: 1, Median: 2, P90: 3, Max: 3},
				Depth:   Range{Min: -6, P10: -6, Median: -2, P90: 0, Max: 0},
				Cores:   []Range{{Min: -6, P10: -6, Median: -2, P90: 0, Max: 0}},
			},
		},
		{
			name: "no run concluded",
			runs: []Run{run(Censored, 50, 9, 3, -2), run(DeadEnd, 7, 4, 2, 0)},
			want: Summary{
				Runs: 2, DeadEnds: 1, Censored: 1,
				Crashes: Range{Min: 4, P10: 4, Median: 6.5, P90: 9, Max: 9},
				Hunts:   Range{Min: 2, P10: 2, Median: 2.5, P90: 3, Max: 3},
				Depth:   Range{Min: -2, P10: -2, Median: -1, P90: 0, Max: 0},
				Cores:   []Range{{Min: -2, P10: -2, Median: -1, P90: 0, Max: 0}},
			},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got, err := Summarize(tc.runs)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestPercentilesUseNearestRank(t *testing.T) {
	values := make([]float64, 36)
	for i := range values {
		values[i] = float64(i + 1)
	}
	want := Range{Min: 1, P10: 4, Median: 18.5, P90: 33, Max: 36}
	if diff := cmp.Diff(want, rangeOf(values)); diff != "" {
		t.Fatal(diff)
	}
}

type row struct {
	Metric     string
	Actual     float64
	Bound      bool
	Placement  string
	Error      float64
	Percentile float64
}

func rows(s Score) []row {
	out := make([]row, len(s.Rows))
	for i, r := range s.Rows {
		out[i] = row{r.Metric.Name, r.Actual, r.Bound, r.Placement, r.Error, r.Percentile}
	}
	return out
}

func TestScore(t *testing.T) {
	before := session(t, "a", 1, start("a", "abc1234")...)
	anchor := anchorAt(before, 2)
	record := Record{Anchor: anchor, Commit: "abc1234", Ruleset: tuner.Ruleset, Runs: []Run{
		{Status: Concluded, Hours: 2, Crashes: 0, Hunts: 0, Profile: []int{-12, -20}, Depth: -32},
		{Status: Concluded, Hours: 4, Crashes: 1, Hunts: 1, Profile: []int{-10, -22}, Depth: -32},
		{Status: DeadEnd, Hours: 6, Crashes: 2, Hunts: 1, Profile: []int{-8, -24}, Depth: -32},
	}}
	crash := &journal.CrashDetected{PreviousBoot: "boot"}
	for _, tc := range []struct {
		name      string
		after     []journal.Payload
		wantRows  []row
		wantFlags []string
	}{
		{
			name:  "concluded with the forecast's build",
			after: []journal.Payload{&journal.ConfigLoaded{Version: "1.0.0", Rev: "abc1234"}, crash, &journal.Shutdown{Reason: journal.ShutdownCycles}},
			wantRows: []row{
				{"core 00", -10, false, Inside, 0, 50},
				{"core 01", -20, false, Inside, 2, 100 * 2.5 / 3},
				{"depth", -30, false, Above, 2, 100},
				{"hours", 3, false, Inside, 0, 50},
				{"crashes", 1, false, Inside, 0, 50},
				{"hunts", 0, false, Inside, -1, 100 * 0.5 / 3},
			},
		},
		{
			name:  "censored session bounds hours, crashes and hunts",
			after: []journal.Payload{&journal.ConfigLoaded{Version: "1.0.0", Rev: "abc1234"}, crash, crash, crash},
			wantRows: []row{
				{"core 00", -10, false, Inside, 0, 50},
				{"core 01", -20, false, Inside, 2, 100 * 2.5 / 3},
				{"depth", -30, false, Above, 2, 100},
				{"hours", 4, true, Open, 1, 50},
				{"crashes", 3, true, Above, 2, 100},
				{"hunts", 0, true, Open, -1, 0},
			},
		},
		{
			name:  "another build is flagged",
			after: []journal.Payload{&journal.ConfigLoaded{Version: "1.0.1", Rev: "fedcba9-dirty"}, &journal.Shutdown{Reason: journal.ShutdownCycles}},
			wantRows: []row{
				{"core 00", -10, false, Inside, 0, 50},
				{"core 01", -20, false, Inside, 2, 100 * 2.5 / 3},
				{"depth", -30, false, Above, 2, 100},
				{"hours", 2, false, Inside, -1, 25},
				{"crashes", 0, false, Inside, -1, 100 * 0.5 / 3},
				{"hunts", 0, false, Inside, -1, 100 * 0.5 / 3},
			},
			wantFlags: []string{"build revision fedcba9-dirty ran after the anchor; the forecast is at commit abc1234"},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s := extend(t, before, 3, tc.after...)
			got, err := record.Score([]Session{s})
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.wantRows, rows(got)); diff != "" {
				t.Error(diff)
			}
			if diff := cmp.Diff(tc.wantFlags, got.Flags); diff != "" {
				t.Error(diff)
			}
		})
	}
}

func TestScoreBuildsCoverTheScoredSpan(t *testing.T) {
	before := session(t, "a", 1, start("a", "0123456")...)
	record := Record{Anchor: anchorAt(before, 2), Commit: "abc1234", Ruleset: tuner.Ruleset, Runs: []Run{{Status: Concluded, Profile: []int{-10, -20}, Depth: -30}}}
	inEffect := journal.Build{Version: "1.0.0", Rev: "0123456", Ruleset: tuner.Ruleset}
	forecastBuild := journal.Build{Version: "1.0.0", Rev: "abc1234", Ruleset: tuner.Ruleset}
	inEffectFlag := "build revision 0123456 ran after the anchor; the forecast is at commit abc1234"
	crash := &journal.CrashDetected{PreviousBoot: "boot"}
	shutdown := &journal.Shutdown{Reason: journal.ShutdownCycles, Cycles: 1}
	for _, tc := range []struct {
		name       string
		sessions   func() []Session
		wantBuilds []journal.Build
		wantFlags  []string
	}{
		{
			name:       "the invocation running at the anchor keeps writing",
			sessions:   func() []Session { return []Session{extend(t, before, 3, crash, shutdown)} },
			wantBuilds: []journal.Build{inEffect},
			wantFlags:  []string{inEffectFlag},
		},
		{
			name: "the invocation running at the anchor writes before a new stamp",
			sessions: func() []Session {
				return []Session{extend(t, before, 3, crash, &journal.ConfigLoaded{Version: "1.0.0", Rev: "abc1234"}, shutdown)}
			},
			wantBuilds: []journal.Build{inEffect, forecastBuild},
			wantFlags:  []string{inEffectFlag},
		},
		{
			name: "a new stamp right after the anchor",
			sessions: func() []Session {
				return []Session{extend(t, before, 3, &journal.ConfigLoaded{Version: "1.0.0", Rev: "abc1234"}, crash, shutdown)}
			},
			wantBuilds: []journal.Build{forecastBuild},
		},
		{
			name: "a new session after the anchor",
			sessions: func() []Session {
				return []Session{before, session(t, "b", 1, append(start("b", "abc1234"), shutdown)...)}
			},
			wantBuilds: []journal.Build{forecastBuild},
		},
		{
			name: "stamps after the terminal point are ignored",
			sessions: func() []Session {
				return []Session{extend(t, before, 3, &journal.ConfigLoaded{Version: "1.0.0", Rev: "abc1234"}, shutdown, &journal.ConfigLoaded{Version: "1.0.1", Rev: "fedcba9"})}
			},
			wantBuilds: []journal.Build{forecastBuild},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got, err := record.Score(tc.sessions())
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.wantBuilds, got.Builds); diff != "" {
				t.Error(diff)
			}
			if diff := cmp.Diff(tc.wantFlags, got.Flags); diff != "" {
				t.Error(diff)
			}
		})
	}
}

func TestScoreWithoutConcludedRuns(t *testing.T) {
	before := session(t, "a", 1, start("a", "abc1234")...)
	record := Record{Anchor: anchorAt(before, 2), Commit: "abc1234", Ruleset: tuner.Ruleset, Runs: []Run{
		{Status: DeadEnd, Hours: 6, Crashes: 2, Hunts: 1, Profile: []int{-8, -24}, Depth: -32},
		{Status: Censored, Hours: 5, Crashes: 1, Hunts: 0, Profile: []int{-10, -20}, Depth: -30},
	}}
	s := extend(t, before, 3, &journal.ConfigLoaded{Version: "1.0.0", Rev: "abc1234"}, &journal.CrashDetected{PreviousBoot: "boot"})
	got, err := record.Score([]Session{s})
	if err != nil {
		t.Fatal(err)
	}
	want := []row{
		{"core 00", -10, false, Inside, -1, 25},
		{"core 01", -20, false, Inside, 2, 75},
		{"depth", -30, false, Inside, 1, 75},
		{"hours", 2, true, Unforecast, 0, 0},
		{"crashes", 1, true, Open, -0.5, 0},
		{"hunts", 0, true, Open, -0.5, 0},
	}
	if diff := cmp.Diff(want, rows(got)); diff != "" {
		t.Fatal(diff)
	}
}

func TestScoreRefusesAnotherCoreCount(t *testing.T) {
	before := session(t, "a", 1, start("a", "abc1234")...)
	record := Record{Anchor: anchorAt(before, 2), Commit: "abc1234", Ruleset: tuner.Ruleset, Runs: []Run{{Status: Concluded, Profile: []int{-10, -20, -30}, Depth: -60}}}
	_, err := record.Score([]Session{extend(t, before, 3, &journal.Shutdown{Reason: journal.ShutdownCycles, Cycles: 1})})
	if err == nil || err.Error() != "run has 2 cores, forecast 3" {
		t.Fatalf("error %v, want run has 2 cores, forecast 3", err)
	}
}

func TestScoreFlagsRulesetAndDirtyForecast(t *testing.T) {
	before := session(t, "a", 1, start("a", "abc1234")...)
	record := Record{Anchor: anchorAt(before, 2), Commit: "abc1234", Dirty: true, Ruleset: tuner.Ruleset - 1, Runs: []Run{{Status: Concluded, Profile: []int{-10, -20}, Depth: -30}}}
	s := extend(t, before, 3, &journal.ConfigLoaded{Version: "1.0.0", Rev: "abc1234"})
	got, err := record.Score([]Session{s})
	if err != nil {
		t.Fatal(err)
	}
	want := []string{
		"the forecast was made from a dirty tree at commit abc1234",
		fmt.Sprintf("ruleset %d ran after the anchor; the forecast used ruleset %d", tuner.Ruleset, tuner.Ruleset-1),
	}
	if diff := cmp.Diff(want, got.Flags); diff != "" {
		t.Fatal(diff)
	}
}

func TestScoreRefusesMissingOrChangedAnchor(t *testing.T) {
	before := session(t, "a", 1, start("a", "abc1234")...)
	record := Record{Anchor: anchorAt(before, 2), Commit: "abc1234", Ruleset: tuner.Ruleset, Runs: []Run{{Status: Concluded, Profile: []int{-10, -20}, Depth: -30}}}
	changed := session(t, "a", 1, append(start("a", "abc1234")[:1], &journal.SessionBaseline{Offsets: []int{-11, -20}})...)
	for _, tc := range []struct {
		name     string
		sessions []Session
		want     error
	}{
		{"no sessions", nil, ErrMissingAnchor},
		{"another session", []Session{session(t, "b", 1, start("b", "abc1234")...)}, ErrMissingAnchor},
		{"journal ends before the anchor", []Session{session(t, "a", 1, start("a", "abc1234")[:1]...)}, ErrMissingAnchor},
		{"journal changed through the anchor", []Session{changed}, ErrChangedAnchor},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if _, err := record.Score(tc.sessions); !errors.Is(err, tc.want) {
				t.Fatalf("error %v, want %v", err, tc.want)
			}
		})
	}
}
