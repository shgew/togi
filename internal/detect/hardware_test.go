//go:build hardware && linux

package detect

import (
	"encoding/json"
	"errors"
	"os/exec"
	"testing"
	"time"

	"github.com/shgew/togi/internal/hostlock"
	"github.com/shgew/togi/internal/machine"
	"golang.org/x/sys/unix"
)

func TestHardwareKernelLog(t *testing.T) {
	lock, err := hostlock.Acquire(hostlock.Path)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := lock.Close(); err != nil {
			t.Error(err)
		}
	})
	out, err := exec.Command("journalctl", "--list-boots", "-o", "json").Output()
	if err != nil {
		t.Fatal(err)
	}
	var boots []struct {
		Index  int    `json:"index"`
		BootID string `json:"boot_id"`
	}
	if err := json.Unmarshal(out, &boots); err != nil {
		t.Fatal(err)
	}
	var prev string
	for _, b := range boots {
		if b.Index == -1 {
			prev = b.BootID
		}
	}
	if prev == "" {
		t.Fatal("no previous boot in the system journal")
	}
	current, err := BootID()
	if err != nil {
		t.Fatal(err)
	}
	var ts unix.Timespec
	if err := unix.ClockGettime(unix.CLOCK_MONOTONIC, &ts); err != nil {
		t.Fatal(err)
	}
	k := NewKernel([]machine.CoreInfo{{Core: 0, CPUs: []int{0}}})
	for _, c := range []struct {
		name    string
		boot    string
		since   time.Duration
		wantErr error
	}{
		{"previous boot", prev, 0, nil},
		{"current boot, last hour", current, max(0, time.Duration(ts.Nano())-time.Hour), nil},
		{"unknown boot", "00000000000000000000000000000000", 0, machine.ErrBootMissing},
	} {
		mces, err := k.MCEs(c.boot, c.since)
		if !errors.Is(err, c.wantErr) {
			t.Fatalf("%s: error = %v, want %v", c.name, err, c.wantErr)
		}
		t.Logf("%s %s: %d machine checks", c.name, c.boot, len(mces))
		if c.name == "unknown boot" && len(mces) != 0 {
			t.Fatalf("unknown boot returned %d machine checks", len(mces))
		}
	}
	reason, err := k.ResetReason(current)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("current boot's reset reason: %+v", reason)
}
