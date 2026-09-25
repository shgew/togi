package session

import (
	"bytes"
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/sim"
)

func TestRunRefusesDifferentRulesetWithoutAppending(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "events.jsonl")
	original := []byte(`{"seq":1,"time":"2026-10-02T01:14:07.000000000Z","boot":"old","kind":"session.start","msg":"session started","session":"old","schema":1,"ruleset":99,"version":"0.2.1","rev":"def5678","cores":[]}` + "\n")
	if err := os.WriteFile(path, original, 0o644); err != nil {
		t.Fatal(err)
	}
	in := simInput(dir, newSim(t, small()))
	bootloader := &fakeBootloader{}
	in.Bootloader = bootloader
	seams := in.Machine.Seams()
	boot, err := seams.Host.BootID()
	if err != nil {
		t.Fatal(err)
	}
	j, err := journal.Open(dir, journal.Options{Boot: boot, Now: in.Machine.Now})
	if err != nil {
		t.Fatal(err)
	}
	_, err = Run(context.Background(), Input{Config: in.Config, ConfigPath: in.ConfigPath, Boot: boot, Journal: j, Machine: seams, Bootloader: bootloader})
	var incompatible *journal.IncompatibleError
	if !errors.As(err, &incompatible) || incompatible.Field != "ruleset" || !strings.Contains(err.Error(), "Install shycler 0.2.1") {
		t.Fatalf("Run: %v", err)
	}
	if bootloader.calls != 1 {
		t.Fatalf("cleared saved entry %d times, want once", bootloader.calls)
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	after, err := os.ReadFile(path)
	if err != nil || !bytes.Equal(after, original) {
		t.Fatalf("refusal modified journal: %v", err)
	}
}

func TestResumeRecordsCurrentBuild(t *testing.T) {
	dir := t.TempDir()
	in := simInput(dir, newSim(t, small()))
	_, err := simulateBoot(context.Background(), in, wrapFor(in, killAt(2)))
	if !errors.Is(err, errKilled) {
		t.Fatalf("first run: %v", err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	_, err = simulateBoot(ctx, in, wrapFor(in, nil))
	if err != nil {
		t.Fatalf("resume: %v", err)
	}
	events := readEvents(t, dir)
	if len(events) < 3 || events[0].Data.(*journal.SessionStart).Build != Build() {
		t.Fatalf("session.start stamp missing: %d events", len(events))
	}
	var loaded *journal.ConfigLoaded
	for _, event := range events[2:] {
		if p, ok := event.Data.(*journal.ConfigLoaded); ok {
			loaded = p
			break
		}
	}
	if loaded == nil || loaded.Build != Build() {
		t.Fatalf("resume config.loaded stamp: %+v", loaded)
	}
}

func TestResumeAfterIncompatibleArchive(t *testing.T) {
	dir := t.TempDir()
	archive := filepath.Join(dir, "archive")
	if err := os.Mkdir(archive, 0o755); err != nil {
		t.Fatal(err)
	}
	data := []byte(`{"seq":1,"time":"2026-10-02T01:14:07Z","boot":"old","kind":"session.start","session":"old","schema":99}` + "\n" +
		`{"seq":2,"time":"2026-10-02T01:14:08Z","boot":"old","kind":"later.unknown"}` + "\n")
	if err := os.WriteFile(filepath.Join(archive, "old.jsonl"), data, 0o644); err != nil {
		t.Fatal(err)
	}
	got, err := Resume(dir, sim.Config{Seed: 1})
	want := time.Date(2026, 10, 2, 1, 14, 8, 0, time.UTC).Add(sim.RebootTime)
	if err != nil || got.Boots != 1 || !got.Start.Equal(want) {
		t.Fatalf("resume archived schema: %+v, %v; want one boot and start %s", got, err, want)
	}
}
