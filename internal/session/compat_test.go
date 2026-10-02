package session

import (
	"bytes"
	"context"
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/shgew/togi/internal/journal"
)

func TestRunRefusesDifferentRulesetWithoutAppending(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "events.jsonl")
	original := []byte(`{"seq":1,"time":"2026-10-02T01:14:07.000000000Z","boot":"old","kind":"session.start","msg":"session started","session":"old","schema":2,"ruleset":99,"version":"0.2.1","rev":"def5678","cores":[]}` + "\n")
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
	if !errors.As(err, &incompatible) || incompatible.Field != "ruleset" || incompatible.Journal.Version != "0.2.1" {
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
