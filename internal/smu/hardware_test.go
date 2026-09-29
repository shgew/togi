//go:build hardware

package smu

import (
	"testing"

	"github.com/shgew/togi/internal/hostlock"
	"github.com/shgew/togi/internal/machine"
)

func TestHardwareSMU(t *testing.T) {
	lock, err := hostlock.Acquire(hostlock.Path)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := lock.Close(); err != nil {
			t.Error(err)
		}
	})
	d, err := Open("/", Sysfs("/"))
	if err != nil {
		t.Fatal(err)
	}
	for _, c := range []machine.Check{d.CheckCPU(), d.CheckDriver(), d.CheckSlotMapping(), d.CheckReadback()} {
		if !c.OK {
			t.Fatalf("check %s failed: %s", c.Name, c.Detail)
		}
		t.Logf("%s: %s", c.Name, c.Detail)
	}
	cores := d.Topology()
	originals := make(map[int]int, len(cores))
	for _, core := range cores {
		cur, err := d.Offset(core.Core)
		if err != nil {
			t.Fatal(err)
		}
		if cur != machine.ClampOffset(cur) {
			t.Fatalf("core %02d reads %d, outside [%d, %d]: writing it back would clamp it", core.Core, cur, machine.MinOffset, machine.MaxOffset)
		}
		originals[core.Core] = cur
	}
	t.Cleanup(func() {
		for _, core := range cores {
			if err := d.SetOffset(core.Core, originals[core.Core]); err != nil {
				t.Errorf("restore core %02d offset: %v", core.Core, err)
			}
		}
	})
	for _, core := range cores {
		cur := originals[core.Core]
		if err := d.SetOffset(core.Core, cur); err != nil {
			t.Fatal(err)
		}
		got, err := d.Offset(core.Core)
		if err != nil {
			t.Fatal(err)
		}
		if got != cur {
			t.Fatalf("core %02d wrote %d, read back %d", core.Core, cur, got)
		}
		t.Logf("core %02d: wrote %d, read back %d", core.Core, cur, got)
	}
	ctx, err := d.BIOSContext()
	if err != nil {
		t.Fatal(err)
	}
	if ctx.BoostLimitMHz <= 0 {
		t.Fatalf("boost limit %d MHz", ctx.BoostLimitMHz)
	}
	t.Logf("BIOS context: %+v", ctx)
}
