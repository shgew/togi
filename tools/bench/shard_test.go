package main

import (
	"slices"
	"testing"
)

func TestParseShard(t *testing.T) {
	for _, tc := range []struct {
		in           string
		index, count int
		ok           bool
	}{
		{"0/1", 0, 1, true}, {"2/4", 2, 4, true},
		{"", 0, 0, false}, {"1", 0, 0, false}, {"4/4", 0, 0, false}, {"-1/4", 0, 0, false}, {"0/0", 0, 0, false}, {"a/4", 0, 0, false}, {"1/b", 0, 0, false},
	} {
		index, count, err := parseShard(tc.in)
		if (err == nil) != tc.ok || index != tc.index || count != tc.count {
			t.Errorf("parseShard(%q) = %d, %d, %v; want %d, %d, ok %t", tc.in, index, count, err, tc.index, tc.count, tc.ok)
		}
	}
}

func TestShardsHoldEverySessionOnceAndBalanceWeight(t *testing.T) {
	var runs []runSpec
	weights := map[string]float64{}
	for seed := uint64(1); seed <= 20; seed++ {
		r := runSpec{scenario: scenario{Name: "s"}, seed: seed, split: "dev"}
		runs = append(runs, r)
		if seed%7 != 0 {
			weights[sessionKey{"s", "dev", seed}.String()] = float64(seed*seed)/7 + 0.1
		}
	}
	const count = 4
	var seen []uint64
	var loads []float64
	for i := range count {
		load := 0.0
		for _, r := range shardRuns(runs, weights, i, count) {
			seen = append(seen, r.seed)
			if w, ok := weights[sessionKey{"s", "dev", r.seed}.String()]; ok {
				load += w
			}
		}
		loads = append(loads, load)
	}
	slices.Sort(seen)
	for i, seed := range seen {
		if seed != uint64(i+1) {
			t.Fatalf("shards hold sessions %v, want each of 1 to 20 once", seen)
		}
	}
	if got := slices.Max(loads) - slices.Min(loads); got > 60 {
		t.Errorf("shard weights %v differ by %v, want a balanced split", loads, got)
	}
	for range 50 {
		for i := range count {
			if !slices.EqualFunc(shardRuns(runs, weights, i, count), shardRuns(runs, weights, i, count), func(a, b runSpec) bool { return a.seed == b.seed }) {
				t.Fatalf("shard %d differs between two calls", i)
			}
		}
	}
}

func TestShardStatus(t *testing.T) {
	for _, tc := range []struct {
		run  simulation
		log  string
		want string
	}{
		{simulation{exit: 0}, "", "concluded"},
		{simulation{exit: 1}, "sim: dead end x: y", "deadend"},
		{simulation{exit: 1}, "sim: check tuner after event 3: stale", "error"},
		{simulation{exit: 3}, "", "censored"},
	} {
		if got := shardStatus(tc.run, tc.log); got != tc.want {
			t.Errorf("shardStatus(%+v, %q) = %s, want %s", tc.run, tc.log, got, tc.want)
		}
	}
}
