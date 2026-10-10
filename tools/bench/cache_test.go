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
	// A tree is a simulator binary and a machine file inside it.
	tree := func(binary, machine string) (string, string) {
		dir := t.TempDir()
		write(filepath.Join(dir, "sim"), binary)
		write(filepath.Join(dir, "machines", "m.toml"), machine)
		return dir, filepath.Join(dir, "machines", "m.toml")
	}
	key := func(dir, machine string, seed uint64, maxBoots int) string {
		t.Helper()
		runs := []runSpec{{scenario: scenario{Name: "s", Machine: machine}, split: "dev", seed: seed}}
		got, err := cacheKey(filepath.Join(dir, "sim"), dir, runs, maxBoots)
		if err != nil {
			t.Fatal(err)
		}
		return got
	}
	dir, machine := tree("binary", "cores = 8\n")
	base := key(dir, machine, 1, 1000)
	otherDir, otherMachine := tree("binary", "cores = 8\n")
	if key(otherDir, otherMachine, 1, 1000) != base {
		t.Error("trees with the same simulator and inputs must share a key wherever they are")
	}
	binaryDir, binaryMachine := tree("binary 2", "cores = 8\n")
	machineDir, machineMachine := tree("binary", "cores = 16\n")
	for name, got := range map[string]string{
		"simulator":    key(binaryDir, binaryMachine, 1, 1000),
		"machine file": key(machineDir, machineMachine, 1, 1000),
		"seed":         key(dir, machine, 2, 1000),
		"boot cap":     key(dir, machine, 1, 5),
	} {
		if got == base {
			t.Errorf("changing the %s must change the key", name)
		}
	}
	if _, err := cacheKey(filepath.Join(dir, "missing"), dir, nil, 1); err == nil {
		t.Error("a missing simulator must not produce a key")
	}
}

func TestSessionCache(t *testing.T) {
	root := t.TempDir()
	cache, err := openSessionCache(root, "k")
	if err != nil {
		t.Fatal(err)
	}
	key := sessionKey{"s", "dev", 1}
	if _, ok := cache.get(key); ok {
		t.Fatal("empty cache hit")
	}
	want := sessionRecord{Digest: "abc", Exit: 3, WallS: 1.5}
	if err := cache.put(key, want); err != nil {
		t.Fatal(err)
	}
	if got, ok := cache.get(key); !ok || got != want {
		t.Fatalf("get = %+v, %v; want %+v", got, ok, want)
	}
	if err := os.WriteFile(cache.path(key), []byte("{"), 0600); err != nil {
		t.Fatal(err)
	}
	if _, ok := cache.get(key); ok {
		t.Error("a corrupt record must miss")
	}
	var none *sessionCache
	if _, ok := none.get(key); ok || none.put(key, want) != nil {
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

func TestLongestFirst(t *testing.T) {
	costs := loadCosts(filepath.Join(t.TempDir(), "costs.json"))
	keys := []sessionKey{{"s", "dev", 1}, {"s", "dev", 2}, {"s", "dev", 3}, {"s", "dev", 4}, {"s", "dev", 5}}
	costs.set(keys[0], 5)
	costs.set(keys[1], 50)
	costs.set(keys[3], 5)
	got := longestFirst(len(keys), func(i int) sessionKey { return keys[i] }, costs)
	if diff := cmp.Diff([]int{2, 4, 1, 0, 3}, got); diff != "" {
		t.Fatalf("unknown costs first in suite order, then longest first, ties in suite order (-want +got):\n%s", diff)
	}
	if err := costs.save(); err != nil {
		t.Fatal(err)
	}
	if got, ok := loadCosts(costs.path).get(keys[1]); !ok || got != 50 {
		t.Fatalf("costs did not survive a save: %v, %v", got, ok)
	}
	corrupt := filepath.Join(t.TempDir(), "costs.json")
	if err := os.WriteFile(corrupt, []byte("{"), 0600); err != nil {
		t.Fatal(err)
	}
	if _, ok := loadCosts(corrupt).get(keys[0]); ok {
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
