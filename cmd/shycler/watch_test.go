package main

import (
	"bytes"
	"fmt"
	"slices"
	"strings"
	"testing"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/shgew/shycler/internal/journal"
	"github.com/shgew/shycler/internal/watch"
)

type watchCut struct {
	name   string
	events []journal.Event
	sizes  [][2]int
	color  bool
}

func watchCuts(t *testing.T) []watchCut {
	t.Helper()
	dir := t.TempDir()
	simulated(t, dir)
	events, _, err := journal.Read(dir)
	if err != nil {
		t.Fatal(err)
	}
	through := func(first func(journal.Event) bool) []journal.Event {
		t.Helper()
		found := false
		for i, e := range events {
			if !found {
				found = first(e)
				continue
			}
			if _, ok := e.Data.(*journal.TrialStart); ok {
				return events[:i+1]
			}
		}
		t.Fatal("no cut point in the simulated journal")
		return nil
	}
	search := through(func(e journal.Event) bool {
		p, ok := e.Data.(*journal.CorePhase)
		return ok && p.To == journal.PhaseConfirmation
	})
	confirm := through(func(e journal.Event) bool {
		p, ok := e.Data.(*journal.CorePhase)
		return ok && p.To == journal.PhaseConfirmed
	})
	guard := through(func(e journal.Event) bool {
		p, ok := e.Data.(*journal.TunerDecision)
		return ok && p.Decision == journal.SuspectBackoff
	})
	last := events[len(events)-1]
	p := &journal.DeadEnd{Condition: journal.DeadEndNoEvidence, Detail: "five trials in a row proved nothing"}
	deadEnd := slices.Concat(events, []journal.Event{
		{Seq: last.Seq + 1, Time: last.Time.Add(time.Minute), Boot: last.Boot, Kind: journal.KindDeadEnd, Msg: p.Message(), Data: p},
	})
	all := [][2]int{{240, 67}, {160, 45}, {120, 33}}
	return []watchCut{
		{name: "search", events: search, sizes: all, color: true},
		{name: "confirm", events: confirm, sizes: all[:1]},
		{name: "guard", events: guard, sizes: all, color: true},
		{name: "deadend", events: deadEnd, sizes: all[:1]},
	}
}

func cutTime(events []journal.Event) time.Time {
	return events[len(events)-1].Time.Add(40 * time.Second).UTC()
}

func TestWatchFrames(t *testing.T) {
	t.Parallel()
	for _, c := range watchCuts(t) {
		s, now := watch.Project(c.events), cutTime(c.events)
		for _, size := range c.sizes {
			frame := watch.Render(s, size[0], size[1], now)
			golden(t, fmt.Sprintf("watch-%s-%dx%d", c.name, size[0], size[1]), ansi.Strip(frame)+"\n")
		}
		if c.color {
			golden(t, fmt.Sprintf("watch-%s-240x67-color", c.name), watch.Render(s, 240, 67, now)+"\n")
		}
	}
}

func TestWatchFrameFitsScreen(t *testing.T) {
	t.Parallel()
	for _, c := range watchCuts(t) {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			s, now := watch.Project(c.events), cutTime(c.events)
			for w := 1; w <= 300; w += 7 {
				for h := 2; h <= 100; h += 5 {
					lines := strings.Split(watch.Render(s, w, h, now), "\n")
					if len(lines) != h-1 {
						t.Errorf("%dx%d: %d lines, want %d", w, h, len(lines), h-1)
					}
					for i, ln := range lines {
						if lipgloss.Width(ln) > w-1 {
							t.Errorf("%dx%d: line %d is %d cells wide, want at most %d", w, h, i, lipgloss.Width(ln), w-1)
						}
					}
				}
			}
		})
	}
}

func TestWatchWithoutJournal(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	var stdout, stderr bytes.Buffer
	if code := cli([]string{"--state-dir", dir, "watch", "--width", "120", "--height", "33"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("exit %d, stderr %q", code, stderr.String())
	}
	if !strings.Contains(stdout.String(), "no session yet") {
		t.Errorf("stdout %q, want it to say no session yet", stdout.String())
	}
	if code := cli([]string{"--state-dir", dir, "watch", "--width", "0"}, &stdout, &stderr); code != exitUsage {
		t.Errorf("--width 0: exit %d, want %d", code, exitUsage)
	}
}
