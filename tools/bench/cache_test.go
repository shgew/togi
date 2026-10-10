package main

import (
	"bytes"
	"context"
	"errors"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/tools/trialfacts"
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
		path := filepath.Join(dir, "machines", "m.json")
		write(path, machine)
		return dir, runSpec{scenario: scenario{Name: "s", Machine: path}, split: "dev", seed: 1, machineFiles: []string{path}}
	}
	inputs := func(dir string, spec runSpec) string {
		t.Helper()
		got, err := runInputs(dir, spec)
		if err != nil {
			t.Fatal(err)
		}
		return got
	}
	dir, spec := tree(`{"cores":8}`)
	want := inputs(dir, spec)
	otherDir, otherSpec := tree(`{"cores":8}`)
	if inputs(otherDir, otherSpec) != want {
		t.Error("the same inputs in another tree must hash alike")
	}
	changedDir, changed := tree(`{"cores":16}`)
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

// An overlay machine reads its parent too, so the parent's content and the declared order are session inputs.
func TestRunInputsHashEveryMachineFile(t *testing.T) {
	tree := func(parent string) (string, runSpec) {
		t.Helper()
		dir := t.TempDir()
		parentPath := filepath.Join(dir, "machines", "parent.json")
		leafPath := filepath.Join(dir, "machines", "leaf.json")
		for path, content := range map[string]string{parentPath: parent, leafPath: `{"extends":"parent.json"}`} {
			if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(path, []byte(content), 0600); err != nil {
				t.Fatal(err)
			}
		}
		return dir, runSpec{scenario: scenario{Name: "s", Machine: leafPath}, split: "dev", seed: 1, machineFiles: []string{parentPath, leafPath}}
	}
	inputs := func(dir string, spec runSpec) string {
		t.Helper()
		got, err := runInputs(dir, spec)
		if err != nil {
			t.Fatal(err)
		}
		return got
	}
	dir, spec := tree(`{"cores":8}`)
	want := inputs(dir, spec)
	otherDir, otherSpec := tree(`{"cores":8}`)
	if inputs(otherDir, otherSpec) != want {
		t.Error("equal trees in other places must hash alike")
	}
	changedDir, changed := tree(`{"cores":16}`)
	if inputs(changedDir, changed) == want {
		t.Error("changing only the parent must change the inputs")
	}
	reordered := spec
	reordered.machineFiles = []string{spec.machineFiles[1], spec.machineFiles[0]}
	if inputs(dir, reordered) == want {
		t.Error("the declared file order must be hashed")
	}
}

// A session record holds only for the whole chain loadRuns resolves: the full shared-voltage root, an overlay of it and
// an overlay of that overlay. Equal chains in other trees share the record; editing only the grandparent misses it.
func TestSessionCacheMissesWhenOnlyOverlayGrandparentChanges(t *testing.T) {
	fixture := func(name string) []byte {
		t.Helper()
		b, err := os.ReadFile(filepath.Join("machines", name))
		if err != nil {
			t.Fatal(err)
		}
		return b
	}
	const (
		rootName   = "target-shared-voltage.json"
		parentName = "target-r7-vf-boost.json"
		leafName   = "target-r7-request-gap.json"
	)
	root, parent, gap := fixture(rootName), fixture(parentName), fixture(leafName)
	facts, err := os.ReadFile(filepath.Join("facts", "target.jsonl.gz"))
	if err != nil {
		t.Fatal(err)
	}
	extendsRoot := []byte(`"extends": "` + rootName + `"`)
	if bytes.Count(parent, extendsRoot) != 1 || bytes.Count(gap, extendsRoot) != 1 {
		t.Fatal("both overlay fixtures must extend the shared-voltage root")
	}
	// The leaf is the request-gap overlay re-pointed at the VF-boost overlay, so it reads three files.
	leaf := bytes.Replace(gap, extendsRoot, []byte(`"extends": "`+parentName+`"`), 1)
	const suite = `{"scenarios":[{"name":"chain","machine":"machines/` + leafName + `","dev":[1,2],"holdout":[101],"smoke":[101]}]}`
	write := func(path string, content []byte) {
		t.Helper()
		if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, content, 0600); err != nil {
			t.Fatal(err)
		}
	}
	tree := func() string {
		t.Helper()
		dir := t.TempDir()
		write(filepath.Join(dir, "suite.json"), []byte(suite))
		write(filepath.Join(dir, "facts", "target.jsonl.gz"), facts)
		for name, content := range map[string][]byte{rootName: root, parentName: parent, leafName: leaf} {
			write(filepath.Join(dir, "machines", name), content)
		}
		return dir
	}
	type run struct {
		key    sessionKey
		inputs string
	}
	// load runs the consumer path: loadRuns resolves the chain, and runInputs hashes every file it names.
	load := func(dir string) []run {
		t.Helper()
		specs, err := loadRuns(filepath.Join(dir, "suite.json"), "all", trialfacts.Extracts{})
		if err != nil {
			t.Fatal(err)
		}
		machines := filepath.Join(dir, "machines")
		files := []string{filepath.Join(machines, leafName), filepath.Join(machines, parentName), filepath.Join(machines, rootName)}
		var runs []run
		for _, spec := range specs {
			if diff := cmp.Diff(files, spec.machineFiles); diff != "" {
				t.Fatalf("machine files of %s %s %d, leaf first (-want +got):\n%s", spec.scenario.Name, spec.split, spec.seed, diff)
			}
			inputs, err := runInputs(dir, spec)
			if err != nil {
				t.Fatal(err)
			}
			runs = append(runs, run{sessionKey{spec.scenario.Name, spec.split, spec.seed}, inputs})
		}
		want := []sessionKey{{"chain", "dev", 1}, {"chain", "dev", 2}, {"chain", "holdout", 101}}
		var got []sessionKey
		for _, r := range runs {
			got = append(got, r.key)
		}
		if diff := cmp.Diff(want, got, cmp.AllowUnexported(sessionKey{})); diff != "" {
			t.Fatalf("runs (-want +got):\n%s", diff)
		}
		return runs
	}
	dir := tree()
	runs := load(dir)
	cache, err := openSessionCache(t.TempDir(), "k")
	if err != nil {
		t.Fatal(err)
	}
	for _, r := range runs {
		if err := cache.put(r.key, sessionRecord{Digest: "d", Inputs: r.inputs}); err != nil {
			t.Fatal(err)
		}
	}
	for i, r := range load(tree()) {
		if r.inputs != runs[i].inputs {
			t.Errorf("%s: an equal chain in another tree must hash alike", r.key)
		}
		if _, ok := cache.get(r.key, r.inputs); !ok {
			t.Errorf("%s: an equal chain in another tree must hit the record", r.key)
		}
	}
	// Lengthening the edit keeps the file-hash memo, keyed by path, size and modification time, from aliasing it.
	note := []byte(`"Generated by just fit-shared-voltage"`)
	if bytes.Count(root, note) != 1 {
		t.Fatal("root fixture lost the note this test edits")
	}
	edited := bytes.Replace(root, note, []byte(`"Generated by just fit-shared-voltage. Edited."`), 1)
	write(filepath.Join(dir, "machines", rootName), edited)
	for i, r := range load(dir) {
		if r.inputs == runs[i].inputs {
			t.Errorf("%s: editing only the grandparent must change the inputs", r.key)
		}
		if _, ok := cache.get(r.key, r.inputs); ok {
			t.Errorf("%s: a record made before the grandparent edit must miss", r.key)
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

func TestCostTableSurvivesScopeWithoutWall(t *testing.T) {
	path := filepath.Join(t.TempDir(), "costs.json")
	if err := os.WriteFile(path, []byte(`{"scopes":[{"key":"a"},{"key":"b","wall":null}]}`), 0600); err != nil {
		t.Fatal(err)
	}
	costs := loadCosts(path)
	key := sessionKey{"s", "dev", 1}
	costs.set("a", key, 7)
	costs.set("b", key, 9)
	if got, ok := costs.get("a", key); !ok || got != 7 {
		t.Errorf("cost set on a scope loaded without wall: %v, %v", got, ok)
	}
	if err := costs.save(); err != nil {
		t.Fatal(err)
	}
	if got, ok := loadCosts(path).get("b", key); !ok || got != 9 {
		t.Errorf("costs did not survive a save: %v, %v", got, ok)
	}
}

func TestTouchDirRecreatesDirRemovedByPruning(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "same", "k")
	calls := 0
	chtimes := func(name string, atime, mtime time.Time) error {
		calls++
		if calls == 1 {
			if err := os.RemoveAll(name); err != nil {
				t.Fatal(err)
			}
		}
		return os.Chtimes(name, atime, mtime)
	}
	if err := touchDir(dir, chtimes); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(dir); err != nil {
		t.Errorf("cache dir missing after recovery: %v", err)
	}
	if calls != 2 {
		t.Errorf("touch attempts = %d, want 2", calls)
	}

	if err := touchDir(dir, func(name string, a, m time.Time) error {
		if err := os.RemoveAll(name); err != nil {
			t.Fatal(err)
		}
		return os.Chtimes(name, a, m)
	}); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("a dir removed on every attempt must fail after the retries: %v", err)
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
