package main

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
)

func TestCacheKey(t *testing.T) {
	write := func(path, content string) {
		t.Helper()
		if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(content), 0600); err != nil {
			t.Fatal(err)
		}
	}
	binary := func(content string) string {
		path := filepath.Join(t.TempDir(), "sim")
		write(path, content)
		return path
	}
	key := func(binary string, maxBoots int) string {
		t.Helper()
		got, err := cacheKey(binary, maxBoots)
		if err != nil {
			t.Fatal(err)
		}
		return got
	}
	base := key(binary("simulator"), 1000)
	if key(binary("simulator"), 1000) != base {
		t.Error("the key must depend on the simulator's bytes, not on where it is")
	}
	if key(binary("simulator 2"), 1000) == base {
		t.Error("changing the simulator must change the key")
	}
	if key(binary("simulator"), 5) == base {
		t.Error("changing the boot cap must change the key")
	}
	if _, err := cacheKey(filepath.Join(t.TempDir(), "missing"), 1); err == nil {
		t.Error("a missing simulator must not produce a key")
	}

	// A session's inputs are its identity and the machine and facts files it names, wherever the tree is.
	tree := func(machine string) (string, runSpec) {
		dir := t.TempDir()
		path := filepath.Join(dir, "machines", "m.toml")
		write(path, machine)
		return dir, runSpec{scenario: scenario{Name: "s", Machine: path}, split: "dev", seed: 1}
	}
	inputs := func(dir string, spec runSpec) string {
		t.Helper()
		got, err := runInputs(dir, spec)
		if err != nil {
			t.Fatal(err)
		}
		return got
	}
	dir, spec := tree("cores = 8\n")
	want := inputs(dir, spec)
	otherDir, otherSpec := tree("cores = 8\n")
	if inputs(otherDir, otherSpec) != want {
		t.Error("the same inputs in another tree must hash alike")
	}
	changedDir, changed := tree("cores = 16\n")
	if inputs(changedDir, changed) == want {
		t.Error("changing a machine file must change the inputs")
	}
	for name, mutate := range map[string]func(*runSpec){
		"seed":   func(s *runSpec) { s.seed = 2 },
		"split":  func(s *runSpec) { s.split = "holdout" },
		"replay": func(s *runSpec) { s.scenario.Replay = true },
		"name":   func(s *runSpec) { s.scenario.Name = "t" },
	} {
		other := spec
		mutate(&other)
		if inputs(dir, other) == want {
			t.Errorf("changing the %s must change the inputs", name)
		}
	}
}

func TestSessionCache(t *testing.T) {
	root := t.TempDir()
	cache, err := openSessionCache(root, "k")
	if err != nil {
		t.Fatal(err)
	}
	key := sessionKey{"s", "dev", 1}
	if _, ok := cache.get(key, "in"); ok {
		t.Fatal("empty cache hit")
	}
	want := sessionRecord{Digest: "abc", Exit: 3, WallS: 1.5, CPUS: 1.25, Inputs: "in"}
	if err := cache.put(key, want); err != nil {
		t.Fatal(err)
	}
	if got, ok := cache.get(key, "in"); !ok || got != want {
		t.Fatalf("get = %+v, %v; want %+v", got, ok, want)
	}
	if _, ok := cache.get(key, "other"); ok {
		t.Error("a record made from other inputs must miss")
	}
	if err := os.WriteFile(cache.path(key), []byte("{"), 0600); err != nil {
		t.Fatal(err)
	}
	if _, ok := cache.get(key, "in"); ok {
		t.Error("a corrupt record must miss")
	}
	var none *sessionCache
	if _, ok := none.get(key, "in"); ok || none.put(key, want) != nil {
		t.Error("a nil cache must miss and store nothing")
	}
}

func TestOpenSessionCachePrunesLeastRecentlyUsed(t *testing.T) {
	root := t.TempDir()
	now := time.Now()
	for i := range keptKeys + 3 {
		dir := filepath.Join(root, "same", string(rune('a'+i)))
		if err := os.MkdirAll(dir, 0755); err != nil {
			t.Fatal(err)
		}
		age := now.Add(time.Duration(i-keptKeys-3) * time.Hour)
		if err := os.Chtimes(dir, age, age); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := openSessionCache(root, "new"); err != nil {
		t.Fatal(err)
	}
	entries, err := os.ReadDir(filepath.Join(root, "same"))
	if err != nil {
		t.Fatal(err)
	}
	var got []string
	for _, e := range entries {
		got = append(got, e.Name())
	}
	want := []string{"e", "f", "g", "h", "i", "j", "k", "new"}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("kept caches (-want +got):\n%s", diff)
	}
}

func TestCostTable(t *testing.T) {
	costs := loadCosts(filepath.Join(t.TempDir(), "costs.json"))
	keys := []sessionKey{{"s", "dev", 1}, {"s", "dev", 2}, {"s", "dev", 3}, {"s", "dev", 4}, {"s", "dev", 5}}
	costs.set("fast", keys[0], 5)
	costs.set("fast", keys[1], 50)
	costs.set("fast", keys[3], 5)
	costs.set("slow", keys[1], 500)
	cost := func(scope string) func(int) (float64, bool) {
		return func(i int) (float64, bool) { return costs.get(scope, keys[i]) }
	}
	if diff := cmp.Diff([]int{2, 4, 1, 0, 3}, longestFirst(len(keys), cost("fast"))); diff != "" {
		t.Fatalf("unknown costs first in suite order, then longest first, ties in suite order (-want +got):\n%s", diff)
	}
	if got, ok := costs.get("slow", keys[0]); !ok || got != 5 {
		t.Errorf("a session this scope never ran takes its latest cost: %v, %v", got, ok)
	}
	if got, _ := costs.get("fast", keys[1]); got != 50 {
		t.Errorf("a scope's own cost must win over another scope's later one: %v", got)
	}
	if err := costs.save(); err != nil {
		t.Fatal(err)
	}
	if got, ok := loadCosts(costs.path).get("fast", keys[1]); !ok || got != 50 {
		t.Fatalf("costs did not survive a save: %v, %v", got, ok)
	}
	for i := range keptCostScopes + 2 {
		costs.set(string(rune('a'+i)), keys[0], 1)
	}
	if len(costs.data.Scopes) != keptCostScopes || costs.data.Scopes[0].Key != string(rune('a'+keptCostScopes+1)) {
		t.Errorf("scopes kept: %d, newest first %q", len(costs.data.Scopes), costs.data.Scopes[0].Key)
	}
	corrupt := filepath.Join(t.TempDir(), "costs.json")
	if err := os.WriteFile(corrupt, []byte("{"), 0600); err != nil {
		t.Fatal(err)
	}
	if _, ok := loadCosts(corrupt).get("fast", keys[0]); ok {
		t.Error("corrupt costs must load as empty")
	}
}

func TestRunPool(t *testing.T) {
	failure := errors.New("failed")
	var started []int
	err := runPool(context.Background(), 1, []int{3, 1, 2, 0}, func(_ context.Context, i int) error {
		started = append(started, i)
		if i == 1 {
			return failure
		}
		return nil
	})
	if !errors.Is(err, failure) {
		t.Fatalf("err = %v, want %v", err, failure)
	}
	if diff := cmp.Diff([]int{3, 1}, started); diff != "" {
		t.Fatalf("an error stops later calls and the order is kept (-want +got):\n%s", diff)
	}
	err = runPool(context.Background(), 2, []int{0, 1}, func(ctx context.Context, i int) error {
		if i == 0 {
			return failure
		}
		<-ctx.Done()
		return context.Canceled
	})
	if !errors.Is(err, failure) {
		t.Fatalf("cancellations must not mask the failure: %v", err)
	}
}
