package hardware

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestRanking(t *testing.T) {
	root := t.TempDir()
	cores := []machine.CoreInfo{{Core: 0, CPUs: []int{4, 20}}, {Core: 8, CPUs: []int{8, 24}}}
	for _, row := range []struct{ cpu, value string }{{"4", "125\n"}, {"8", "174\n"}} {
		path := filepath.Join(root, "sys/devices/system/cpu/cpufreq/policy"+row.cpu)
		if err := os.MkdirAll(path, 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(path, "amd_pstate_prefcore_ranking"), []byte(row.value), 0644); err != nil {
			t.Fatal(err)
		}
	}
	got, err := ranking(root, cores)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]int{125, 174}, got); diff != "" {
		t.Fatalf("ranking (-want +got):\n%s", diff)
	}
	if err := os.Remove(filepath.Join(root, "sys/devices/system/cpu/cpufreq/policy8/amd_pstate_prefcore_ranking")); err != nil {
		t.Fatal(err)
	}
	if _, err := ranking(root, cores); err == nil || err.Error() == "" {
		t.Fatal("missing ranking must fail")
	}
}

func TestClockCancellation(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if err := (clock{}).Sleep(ctx, time.Hour); !errors.Is(err, context.Canceled) {
		t.Fatalf("sleep cancellation = %v, want %v", err, context.Canceled)
	}
	if (clock{}).Monotonic() <= 0 {
		t.Fatal("monotonic clock has not advanced")
	}
}
