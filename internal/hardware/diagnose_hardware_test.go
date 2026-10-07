//go:build hardware && linux

package hardware

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/hostlock"
	"github.com/shgew/togi/internal/smu"
)

func TestHardwareDiagnoseLeavesOffsets(t *testing.T) {
	lock, err := hostlock.Acquire(hostlock.Path)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := lock.Close(); err != nil {
			t.Error(err)
		}
	})
	drv, err := smu.Open("/", smu.Sysfs("/"))
	if err != nil {
		t.Fatal(err)
	}
	offsets := func() map[int]int {
		t.Helper()
		values := map[int]int{}
		for _, core := range drv.Topology() {
			offset, err := drv.Offset(core.Core)
			if err != nil {
				t.Fatal(err)
			}
			values[core.Core] = offset
		}
		return values
	}
	before := offsets()
	ran, skipped, err := Diagnose(config.Default(), nil, true)
	if err != nil {
		t.Fatal(err)
	}
	for _, c := range ran {
		t.Logf("%s ok=%v: %s", c.Name, c.OK, c.Detail)
	}
	if len(skipped) != 0 {
		t.Fatalf("privileged Diagnose skipped checks: %+v", skipped)
	}
	if diff := cmp.Diff(before, offsets()); diff != "" {
		t.Fatalf("offsets changed by Diagnose (-before +after):\n%s", diff)
	}
}
