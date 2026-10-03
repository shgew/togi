package main

import (
	"bytes"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/charmbracelet/x/ansi"
	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/tuner"
	"github.com/shgew/togi/internal/watch"
)

type recordedEvent struct {
	wall   time.Duration
	boot   string
	fields string
}

func recordedJournal(t *testing.T, recorded ...recordedEvent) (string, []journal.Event) {
	t.Helper()
	var data strings.Builder
	start := time.Unix(1000, 0).UTC()
	for i, e := range recorded {
		kind, payload := "session.notice", `,"notice":"nonzero_baseline"`
		if i == 0 {
			kind, payload = "session.start", fmt.Sprintf(`,"schema":%d,"ruleset":%d,"session":"replay","cores":[{"core":0,"ccd":0,"cpus":[0,1]}]`, journal.Schema, tuner.Ruleset)
		}
		fmt.Fprintf(&data, "{\"seq\":%d,\"time\":%q,\"boot\":%q,\"kind\":%q,\"msg\":%q%s%s}\n", i+1, start.Add(e.wall).Format(time.RFC3339Nano), e.boot, kind, fmt.Sprintf("event %d", i+1), payload, e.fields)
	}
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte(data.String()), 0o600); err != nil {
		t.Fatal(err)
	}
	events, _, err := journal.Read(dir)
	if err != nil {
		t.Fatal(err)
	}
	return dir, events
}

func assertPlayback(t *testing.T, p *playback, elapsed time.Duration, wantN int, wantClock time.Time) {
	t.Helper()
	n, clock := p.advance(elapsed)
	if n != wantN {
		t.Errorf("at elapsed %s: consumed %d events, want %d", elapsed, n, wantN)
	}
	if diff := cmp.Diff(wantClock, clock); diff != "" {
		t.Errorf("at elapsed %s: display clock (-want +got):\n%s", elapsed, diff)
	}
}

func TestPlaybackWallClockCorrections(t *testing.T) {
	t.Parallel()
	for _, jump := range []time.Duration{-time.Minute, time.Minute} {
		t.Run(jump.String(), func(t *testing.T) {
			_, events := recordedJournal(t,
				recordedEvent{0, "boot", `,"mono_ms":0`},
				recordedEvent{jump + 10*time.Second, "boot", `,"mono_ms":10000`},
				recordedEvent{jump + 20*time.Second, "boot", `,"mono_ms":20000`},
				recordedEvent{jump + 30*time.Second, "boot", `,"mono_ms":30000`})
			p := newPlayback(events, 1, 1)
			for _, step := range []struct {
				elapsed time.Duration
				n       int
			}{
				{0, 1},
				{9 * time.Second, 1},
				{10 * time.Second, 2},
				{15 * time.Second, 2},
				{20*time.Second - time.Nanosecond, 2},
				{20 * time.Second, 3},
				{35 * time.Second, 4},
			} {
				wall := step.elapsed
				if step.n > 1 {
					wall += jump
				}
				assertPlayback(t, p, step.elapsed, step.n, events[0].Time.Add(wall))
			}
		})
	}
}

func TestPlaybackZeroMono(t *testing.T) {
	t.Parallel()
	_, events := recordedJournal(t,
		recordedEvent{0, "boot", `,"mono_ms":0`},
		recordedEvent{time.Hour, "boot", `,"mono_ms":0`},
		recordedEvent{time.Hour + 20*time.Second, "boot", `,"mono_ms":20000`})
	p := newPlayback(events, 1, 1)
	assertPlayback(t, p, 0, 2, events[1].Time)
	assertPlayback(t, p, time.Second, 2, events[1].Time.Add(time.Second))
	assertPlayback(t, p, 20*time.Second, 3, events[2].Time)
}

func TestPlaybackFallback(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name   string
		before recordedEvent
		after  recordedEvent
	}{
		{"missing both", recordedEvent{0, "boot", ""}, recordedEvent{10 * time.Second, "boot", ""}},
		{"missing before", recordedEvent{0, "boot", ""}, recordedEvent{10 * time.Second, "boot", `,"mono_ms":0`}},
		{"missing after", recordedEvent{0, "boot", `,"mono_ms":0`}, recordedEvent{10 * time.Second, "boot", ""}},
		{"nested mono is not a timestamp", recordedEvent{0, "boot", `,"mono_ms":0`}, recordedEvent{10 * time.Second, "boot", `,"source":{"mono_ms":0},"detail":"\"mono_ms\":0"`}},
		{"cross boot", recordedEvent{0, "before", `,"mono_ms":100000`}, recordedEvent{10 * time.Second, "after", `,"mono_ms":0`}},
		{"unknown boot", recordedEvent{0, "", `,"mono_ms":0`}, recordedEvent{10 * time.Second, "", `,"mono_ms":0`}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			_, events := recordedJournal(t, tc.before, tc.after)
			p := newPlayback(events, 1, 1)
			assertPlayback(t, p, 9*time.Second, 1, events[0].Time.Add(9*time.Second))
			assertPlayback(t, p, 10*time.Second, 2, events[1].Time)
			assertPlayback(t, p, 12*time.Second, 2, events[1].Time.Add(2*time.Second))
		})
	}
}

func TestPlaybackBackwardFallbackRetainsFollowingInterval(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name   string
		events []recordedEvent
	}{
		{"legacy", []recordedEvent{
			{0, "boot", ""},
			{-time.Minute, "boot", ""},
			{-time.Minute + 10*time.Second, "boot", ""},
		}},
		{"cross boot", []recordedEvent{
			{0, "before", `,"mono_ms":100000`},
			{-time.Minute, "after", `,"mono_ms":0`},
			{-time.Minute + 10*time.Second, "after", `,"mono_ms":10000`},
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			_, events := recordedJournal(t, tc.events...)
			p := newPlayback(events, 1, 1)
			assertPlayback(t, p, 0, 2, events[1].Time)
			assertPlayback(t, p, 9*time.Second, 2, events[1].Time.Add(9*time.Second))
			assertPlayback(t, p, 10*time.Second, 3, events[2].Time)
		})
	}
}

func TestPlaybackFrom(t *testing.T) {
	t.Parallel()
	_, events := recordedJournal(t,
		recordedEvent{0, "boot", `,"mono_ms":0`},
		recordedEvent{-50 * time.Second, "boot", `,"mono_ms":10000`},
		recordedEvent{80 * time.Second, "boot", `,"mono_ms":20000`})
	p := newPlayback(events, prefix(events, 2), 1)
	assertPlayback(t, p, 0, 2, events[1].Time)
	assertPlayback(t, p, 9*time.Second, 2, events[1].Time.Add(9*time.Second))
	assertPlayback(t, p, 10*time.Second, 3, events[2].Time)
	assertPlayback(t, p, 12*time.Second, 3, events[2].Time.Add(2*time.Second))
}

func TestPlaybackSpeed(t *testing.T) {
	t.Parallel()
	_, events := recordedJournal(t,
		recordedEvent{0, "boot", `,"mono_ms":0`},
		recordedEvent{10 * time.Second, "boot", `,"mono_ms":10000`},
		recordedEvent{20 * time.Second, "boot", `,"mono_ms":20000`})
	for _, tc := range []struct {
		name    string
		speed   float64
		elapsed time.Duration
		n       int
		wall    time.Duration
	}{
		{"slower", 0.5, 30 * time.Second, 2, 15 * time.Second},
		{"normal", 300, 50 * time.Millisecond, 2, 15 * time.Second},
		{"huge at start", math.MaxFloat64, 0, 1, 0},
		{"huge", math.MaxFloat64, time.Nanosecond, 3, time.Duration(1<<63 - 1)},
		{"huge later", math.MaxFloat64, time.Second, 3, time.Duration(1<<63 - 1)},
	} {
		t.Run(tc.name, func(t *testing.T) {
			p := newPlayback(events, 1, tc.speed)
			assertPlayback(t, p, tc.elapsed, tc.n, events[0].Time.Add(tc.wall))
		})
	}
}

func TestRunRejectsInvalidSpeedBeforeReading(t *testing.T) {
	t.Parallel()
	for _, speed := range []string{"NaN", "Inf", "+Inf", "-Inf", "0", "-0", "-1"} {
		t.Run(speed, func(t *testing.T) {
			var errOut bytes.Buffer
			err := run([]string{"--state-dir", filepath.Join(t.TempDir(), "missing"), "--speed", speed}, nil, &errOut)
			if err == nil || !strings.Contains(err.Error(), "--speed must be finite and positive") {
				t.Fatalf("speed %s: got %v, want finite-positive speed rejection before journal access", speed, err)
			}
		})
	}
}

func TestRunFrameRetainsRecordedClock(t *testing.T) {
	t.Parallel()
	dir, events := recordedJournal(t,
		recordedEvent{0, "boot", `,"mono_ms":0`},
		recordedEvent{-time.Minute, "boot", `,"mono_ms":10000`},
		recordedEvent{time.Hour, "boot", `,"mono_ms":20000`})
	for _, view := range []struct {
		name string
		view watch.View
	}{
		{"main", watch.MainView},
		{"help", watch.HelpView},
		{"log", watch.LogView},
	} {
		t.Run(view.name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "frame")
			out, err := os.Create(path)
			if err != nil {
				t.Fatal(err)
			}
			t.Cleanup(func() { _ = out.Close() })
			var errOut bytes.Buffer
			if err := run([]string{"--state-dir", dir, "--at", "2", "--after", "40s", "--view", view.name, "--width", "80", "--height", "67", "--speed", "1.7976931348623157e308"}, out, &errOut); err != nil {
				t.Fatal(err)
			}
			got, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			sc := watch.Screen{View: view.view, Keys: true, Width: 80, Height: 67}
			if view.view == watch.LogView {
				sc.Scroll = -1
			}
			want, _ := watch.RenderView(watch.Project(events[:2]), sc, events[1].Time.Add(40*time.Second))
			if diff := cmp.Diff(ansi.Strip(want)+"\n", string(got)); diff != "" {
				t.Errorf("--at frame must use the selected event wall clock plus --after (-want +got):\n%s", diff)
			}
		})
	}
}
