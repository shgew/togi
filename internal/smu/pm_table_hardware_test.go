//go:build hardware && linux

package smu

import (
	"math"
	"os"
	"testing"
	"time"

	"github.com/shgew/togi/internal/hostlock"
)

func TestHardwarePMTable(t *testing.T) {
	lock, err := hostlock.Acquire(hostlock.Path)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := lock.Close(); err != nil {
			t.Error(err)
		}
	})
	cores, err := topology("/")
	if err != nil {
		t.Fatal(err)
	}
	c := NewConditions("/", cores, os.ReadFile)
	t.Cleanup(func() {
		c.mu.Lock()
		done := c.done
		c.mu.Unlock()
		if done != nil {
			<-done
		}
	})
	c.Check()
	time.Sleep(5 * time.Millisecond)
	check := c.Check()
	p := c.PMTable()
	if p == nil {
		t.Fatal(check.Detail)
	}
	for core := range p.C0Pct {
		sum := float64(p.C0Pct[core]) + float64(p.CC1Pct[core]) + float64(p.CC6Pct[core])
		if math.Abs(sum-100) > 1 {
			t.Errorf("core %02d C0+CC1+CC6 = %.4f%%, want 100%% within 1%%", core, sum)
		}
		t.Logf("core %02d: %.4f W, %.4f V requested, %.2f C, C0 %.3f%% CC1 %.3f%% CC6 %.3f%%", core, p.PowerW[core], p.VoltageRequestV[core], p.TemperatureC[core], p.C0Pct[core], p.CC1Pct[core], p.CC6Pct[core])
	}
}
