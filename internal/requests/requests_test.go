package requests

import (
	"testing"

	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/machine"
)

func ccdOf(core int) int { return core / 8 }

func samples(from, to int64, lanes func(elapsedMS int64) (map[int]float32, map[int]int)) func(func(machine.TrialConditions) bool) {
	return func(yield func(machine.TrialConditions) bool) {
		for ms := from; ms <= to; ms += 1000 {
			requests, mhz := lanes(ms)
			table := &machine.PMTable{}
			for core, v := range requests {
				table.VoltageRequestV[core] = v
			}
			if !yield(machine.TrialConditions{ElapsedMS: ms, CoreMHz: machine.PerCoreFrom(mhz), PMTable: table}) {
				return
			}
		}
	}
}

func TestSummarizeUsesSamplesAfterWarmup(t *testing.T) {
	seq := samples(1000, 24000, func(ms int64) (map[int]float32, map[int]int) {
		if ms < WarmupMS {
			return map[int]float32{0: 1.5, 1: 1.5, 8: 1.5}, map[int]int{0: 9000, 1: 9000, 8: 9000}
		}
		return map[int]float32{0: 1.100, 1: 1.0995, 8: 1.050}, map[int]int{0: 5200, 1: 5210, 8: 5100}
	})
	got, ok := Summarize(seq, []int{0, 1, 8}, ccdOf)
	if !ok {
		t.Fatal("20 samples after the warmup did not summarize")
	}
	want := Telemetry{
		Requests:      map[int]float64{0: float64(float32(1.100)), 1: float64(float32(1.0995)), 8: float64(float32(1.050))},
		TopRequesters: []int{0, 1, 8},
		CCDMHz:        map[int]int{0: 5205, 1: 5100},
	}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("Summarize (-want +got):\n%s", diff)
	}
}

func TestSummarizeNeedsMinSamplesAfterWarmup(t *testing.T) {
	steady := func(int64) (map[int]float32, map[int]int) {
		return map[int]float32{3: 1.1}, map[int]int{3: 5000}
	}
	if _, ok := Summarize(samples(0, WarmupMS+(MinSamples-2)*1000, steady), []int{3}, ccdOf); ok {
		t.Fatal("19 samples after the warmup summarized")
	}
	if _, ok := Summarize(samples(0, WarmupMS+(MinSamples-1)*1000, steady), []int{3}, ccdOf); !ok {
		t.Fatal("20 samples after the warmup did not summarize")
	}
}

func TestSummarizeNeedsMinClockSamplesPerCCD(t *testing.T) {
	// Core 8's clock is missing from the first qualifying sample, so CCD1 has
	// 19 sample medians while CCD0 has 20.
	seq := samples(WarmupMS, WarmupMS+(MinSamples-1)*1000, func(ms int64) (map[int]float32, map[int]int) {
		mhz := map[int]int{0: 5200, 8: 5100}
		if ms == WarmupMS {
			delete(mhz, 8)
		}
		return map[int]float32{0: 1.1, 8: 1.05}, mhz
	})
	got, ok := Summarize(seq, []int{0, 8}, ccdOf)
	if !ok {
		t.Fatal("20 samples after the warmup did not summarize")
	}
	want := Telemetry{
		Requests:      map[int]float64{0: float64(float32(1.1)), 8: float64(float32(1.05))},
		TopRequesters: []int{0, 8},
		CCDMHz:        map[int]int{0: 5200},
	}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("Summarize (-want +got):\n%s", diff)
	}
}

func TestSummarizeSkipsSamplesWithoutLanes(t *testing.T) {
	seq := func(yield func(machine.TrialConditions) bool) {
		for ms := int64(WarmupMS); ms < WarmupMS+60000; ms += 1000 {
			if !yield(machine.TrialConditions{ElapsedMS: ms, CoreMHz: machine.PerCoreFrom(map[int]int{2: 5000})}) {
				return
			}
		}
	}
	if _, ok := Summarize(seq, []int{2}, ccdOf); ok {
		t.Fatal("samples without decoded lanes summarized")
	}
}

func TestGroups(t *testing.T) {
	for _, tt := range []struct {
		name     string
		requests map[int]float64
		want     [][]int
	}{
		{"strict order", map[int]float64{1: 1.10, 2: 1.12, 3: 1.11}, [][]int{{2}, {3}, {1}}},
		{"within the tie joins the top", map[int]float64{1: 1.1000, 2: 1.1009, 3: 1.0950}, [][]int{{1, 2}, {3}}},
		{"ties measure from the group's highest", map[int]float64{1: 1.1000, 2: 1.0992, 3: 1.0984}, [][]int{{1, 2}, {3}}},
		{"equal requests group by core", map[int]float64{5: 1.1, 4: 1.1}, [][]int{{4, 5}}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			if diff := cmp.Diff(tt.want, Groups(tt.requests)); diff != "" {
				t.Fatalf("Groups (-want +got):\n%s", diff)
			}
		})
	}
}

func TestCounts(t *testing.T) {
	for _, tt := range []struct {
		v, target float64
		want      int
	}{
		{1.100, 1.100, 1},
		{1.100, 1.050, 1},
		{1.100, 1.100 + VoltsPerCount, 1},
		{1.100, 1.100 + VoltsPerCount + 0.0001, 2},
		{1.0322, 1.0443, 4},
	} {
		if got := Counts(tt.v, tt.target); got != tt.want {
			t.Errorf("Counts(%v, %v) = %d, want %d", tt.v, tt.target, got, tt.want)
		}
	}
}
