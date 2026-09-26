//go:build hardware

package detect

import (
	"encoding/json"
	"os/exec"
	"testing"
	"time"

	"github.com/shgew/shycler/internal/machine"
)

func TestHardwareKernelLog(t *testing.T) {
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
	k := NewKernel([]machine.CoreInfo{{Core: 0, CPUs: []int{0}}})
	for _, c := range []struct {
		name  string
		boot  string
		since time.Time
	}{
		{"previous boot", prev, time.Time{}},
		{"current boot, last hour", current, time.Now().Add(-time.Hour)},
		{"unknown boot", "00000000000000000000000000000000", time.Time{}},
	} {
		mces, err := k.MCEs(c.boot, c.since)
		if err != nil {
			t.Fatalf("%s: %v", c.name, err)
		}
		t.Logf("%s %s: %d machine checks", c.name, c.boot, len(mces))
		if c.name == "unknown boot" && len(mces) != 0 {
			t.Fatalf("unknown boot returned %d machine checks", len(mces))
		}
	}
}
