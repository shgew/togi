//go:build integration

package sim

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/shgew/togi/internal/machine"
)

func TestSamplePersistenceFailuresRemainErrors(t *testing.T) {
	for _, duration := range []time.Duration{3 * time.Second, time.Hour} {
		t.Run(duration.String(), func(t *testing.T) {
			m := newMachine(t, Config{Cores: 2, Edges: flat(2, -50, -50)})
			dir := t.TempDir()
			trialDir := filepath.Join(dir, "0001")
			if err := os.Mkdir(trialDir, 0700); err != nil {
				t.Fatal(err)
			}
			if err := os.Symlink("/dev/full", filepath.Join(trialDir, "samples.jsonl")); err != nil {
				t.Fatal(err)
			}
			m.SetSamplesDir(dir)
			res, err := runSpec(t, m, "0001", machine.R1, machine.PickWorkload(machine.R1, 0), []int{0}, duration, nil)
			if !errors.Is(err, syscall.ENOSPC) || !strings.Contains(err.Error(), "persist simulated samples") || res.Ran != duration {
				t.Fatalf("sample persistence result = %+v, %v", res, err)
			}
		})
	}
}
