package main

import (
	"cmp"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"hash"
	"io"
	"io/fs"
	"math"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"time"
)

const (
	cacheFormat = "togi-bench-same-v2"
	keptKeys    = 8
)

// sessionRecord is what the same cache keeps for one finished simulation: the digest of its normalized journals, which
// stands in for the journals themselves, its exit status, the CPU time of its process, and the digest of the inputs the
// simulator read for it besides its own binary.
type sessionRecord struct {
	Digest string  `json:"digest"`
	Exit   int     `json:"exit"`
	WallS  float64 `json:"wall_s"`
	CPUS   float64 `json:"cpu_s"`
	Inputs string  `json:"inputs"`
}

// sessionCache holds one tree's session records under a key that names the tree's contents and build environment. A
// nil cache misses everything and stores nothing.
type sessionCache struct{ dir string }

// openSessionCache returns the cache for key under root, marks it recently used and drops the least recently used
// others beyond keptKeys.
func openSessionCache(root, key string) (*sessionCache, error) {
	parent := filepath.Join(root, "same")
	dir := filepath.Join(parent, key)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return nil, fmt.Errorf("create session cache: %w", err)
	}
	now := time.Now()
	if err := os.Chtimes(dir, now, now); err != nil {
		return nil, fmt.Errorf("touch session cache: %w", err)
	}
	entries, err := os.ReadDir(parent)
	if err != nil {
		return nil, fmt.Errorf("list session caches: %w", err)
	}
	type aged struct {
		name string
		time time.Time
	}
	var others []aged
	for _, entry := range entries {
		info, err := entry.Info()
		if entry.IsDir() && entry.Name() != key && err == nil {
			others = append(others, aged{entry.Name(), info.ModTime()})
		}
	}
	slices.SortFunc(others, func(a, b aged) int { return b.time.Compare(a.time) })
	for _, old := range others[min(len(others), keptKeys-1):] {
		os.RemoveAll(filepath.Join(parent, old.name))
	}
	return &sessionCache{dir}, nil
}

func (c *sessionCache) path(key sessionKey) string {
	return filepath.Join(c.dir, key.scenario, fmt.Sprintf("%s-%d.json", key.split, key.seed))
}

func (c *sessionCache) get(key sessionKey, inputs string) (sessionRecord, bool) {
	if c == nil {
		return sessionRecord{}, false
	}
	b, err := os.ReadFile(c.path(key))
	var rec sessionRecord
	if err != nil || json.Unmarshal(b, &rec) != nil || rec.Digest == "" || rec.Inputs != inputs {
		return sessionRecord{}, false
	}
	return rec, true
}

func (c *sessionCache) put(key sessionKey, rec sessionRecord) error {
	if c == nil {
		return nil
	}
	b, err := json.Marshal(rec)
	if err != nil {
		return err
	}
	return writeFileAtomic(c.path(key), b)
}

func writeFileAtomic(path string, content []byte) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	tmp, err := os.CreateTemp(filepath.Dir(path), ".tmp-")
	if err != nil {
		return err
	}
	_, err = tmp.Write(content)
	if closeErr := tmp.Close(); err == nil {
		err = closeErr
	}
	if err == nil {
		err = os.Rename(tmp.Name(), path)
	}
	if err != nil {
		os.Remove(tmp.Name())
	}
	return err
}

// simulatorBuildFlags make the simulator's bytes depend only on the code and toolchain: -trimpath keeps the tree's
// location out, -buildvcs=false its revision.
var simulatorBuildFlags = []string{"-trimpath", "-buildvcs=false"}

// cacheKey names the simulator whose sessions a cache holds: the hash of its binary, which carries the code, the Go
// version and the platform, and the boot cap. Trees whose simulators are byte for byte equal share their records,
// whatever else differs between them.
func cacheKey(binary string, maxBoots int) (string, error) {
	if _, err := os.Stat(binary); err != nil {
		return "", fmt.Errorf("find simulator: %w", err)
	}
	h := sha256.New()
	fmt.Fprintf(h, "%s boots=%d\n", cacheFormat, maxBoots)
	if err := hashFile(h, binary, "simulator"); err != nil {
		return "", err
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

// runInputs hashes what one session's simulator reads besides its binary: the session's identity and the machine and
// facts files it names, by name relative to the tree when inside it.
func runInputs(tree string, spec runSpec) (string, error) {
	inTree := func(path string) string {
		if rel, err := filepath.Rel(tree, path); err == nil && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
			return rel
		}
		return path
	}
	h := sha256.New()
	fmt.Fprintf(h, "run %s %s %d replay=%t machine=%s\n", spec.scenario.Name, spec.split, spec.seed, spec.scenario.Replay, inTree(spec.scenario.Machine))
	for _, file := range []string{spec.scenario.Machine, factsPath(spec)} {
		if err := hashFile(h, file, inTree(file)); err != nil {
			return "", err
		}
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

func factsPath(spec runSpec) string {
	if spec.cfg.Facts == "" || spec.scenario.Machine == "" || filepath.IsAbs(spec.cfg.Facts) {
		return spec.cfg.Facts
	}
	return filepath.Join(filepath.Dir(spec.scenario.Machine), spec.cfg.Facts)
}

// fileSums remembers the hash of each file by name, size and modification time: sessions share their machine and facts
// files.
var fileSums sync.Map

// hashFile adds a file's content, or its absence, to h under name.
func hashFile(h hash.Hash, path, name string) error {
	if path == "" {
		return nil
	}
	info, err := os.Stat(path)
	if errors.Is(err, fs.ErrNotExist) {
		fmt.Fprintf(h, "absent %s\n", name)
		return nil
	} else if err != nil {
		return err
	}
	memo := fmt.Sprintf("%s|%d|%d", path, info.Size(), info.ModTime().UnixNano())
	if sum, ok := fileSums.Load(memo); ok {
		fmt.Fprintf(h, "file %s %x\n", name, sum)
		return nil
	}
	f, err := os.Open(path)
	if err != nil {
		return err
	}
	defer f.Close()
	sum := sha256.New()
	if _, err := io.Copy(sum, f); err != nil {
		return fmt.Errorf("hash %s: %w", path, err)
	}
	fileSums.Store(memo, sum.Sum(nil))
	fmt.Fprintf(h, "file %s %x\n", name, sum.Sum(nil))
	return nil
}

// costTable remembers how long each session took on the last run that finished it, to start the longest ones first.
type costTable struct {
	path string
	mu   sync.Mutex
	wall map[string]float64
}

func loadCosts(path string) *costTable {
	c := &costTable{path: path, wall: map[string]float64{}}
	if b, err := os.ReadFile(path); err == nil {
		if json.Unmarshal(b, &c.wall) != nil {
			c.wall = map[string]float64{}
		}
	}
	return c
}

func (c *costTable) set(key sessionKey, wall float64) {
	if c == nil || wall <= 0 {
		return
	}
	c.mu.Lock()
	c.wall[key.String()] = wall
	c.mu.Unlock()
}

func (c *costTable) get(key sessionKey) (float64, bool) {
	if c == nil {
		return 0, false
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	wall, ok := c.wall[key.String()]
	return wall, ok
}

func (c *costTable) save() error {
	if c == nil {
		return nil
	}
	c.mu.Lock()
	b, err := json.Marshal(c.wall)
	c.mu.Unlock()
	if err != nil {
		return err
	}
	return writeFileAtomic(c.path, b)
}

// longestFirst orders the indexes 0..n-1 by decreasing recorded cost. Sessions without a record come first, in their
// given order, as they may be the longest; equal costs keep their order.
func longestFirst(n int, key func(int) sessionKey, costs *costTable) []int {
	order := make([]int, n)
	cost := make([]float64, n)
	for i := range order {
		order[i] = i
		cost[i] = math.Inf(1)
		if wall, ok := costs.get(key(i)); ok {
			cost[i] = wall
		}
	}
	slices.SortStableFunc(order, func(a, b int) int { return cmp.Compare(cost[b], cost[a]) })
	return order
}

// runPool calls fn for each index of order, up to workers at a time and in that order. The first error that is not a
// cancellation cancels the context fn receives and stops starting more calls. It returns the errors that did occur.
func runPool(ctx context.Context, workers int, order []int, fn func(ctx context.Context, i int) error) error {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	var mu sync.Mutex
	var errs []error
	next := 0
	var wg sync.WaitGroup
	for range min(workers, len(order)) {
		wg.Go(func() {
			for {
				mu.Lock()
				if next == len(order) || ctx.Err() != nil {
					mu.Unlock()
					return
				}
				i := order[next]
				next++
				mu.Unlock()
				if err := fn(ctx, i); err != nil && !errors.Is(err, context.Canceled) {
					mu.Lock()
					errs = append(errs, err)
					mu.Unlock()
					cancel()
				}
			}
		})
	}
	wg.Wait()
	return errors.Join(errs...)
}

// cacheDir is where the session records and costs live: --cache, else under the user cache directory.
func (o options) cacheDir() (string, error) {
	if o.cache != "" {
		return filepath.Abs(o.cache)
	}
	dir, err := os.UserCacheDir()
	if err != nil {
		return "", fmt.Errorf("resolve cache directory: %w", err)
	}
	return filepath.Join(dir, "togi", "bench"), nil
}
