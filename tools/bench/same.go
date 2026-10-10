package main

import (
	"bufio"
	"bytes"
	"cmp"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"sync"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/tools/trialfacts"
)

type sessionKey struct {
	scenario, split string
	seed            uint64
}

func (k sessionKey) String() string {
	return fmt.Sprintf("%s/%s-%d", k.scenario, k.split, k.seed)
}

var sameSides = [2]string{"base", "head"}

type samePair struct {
	key  sessionKey
	runs [2]*runSpec
}

type journalDifference struct {
	file       string
	line       int
	base, head string
}

func executeSame(o options, stdout, stderr io.Writer) (bool, error) {
	head, err := os.Getwd()
	if err != nil {
		return false, fmt.Errorf("resolve current tree: %w", err)
	}
	base, err := filepath.Abs(o.same)
	if err != nil {
		return false, fmt.Errorf("resolve base tree: %w", err)
	}
	trees := [2]string{base, head}
	extracts := trialfacts.Extracts{}
	var runs [2][]runSpec
	for side, tree := range trees {
		suite := o.suite
		if !filepath.IsAbs(suite) {
			suite = filepath.Join(tree, suite)
		}
		runs[side], err = loadRuns(suite, "all", extracts)
		if err != nil {
			return false, fmt.Errorf("load %s suite: %w", tree, err)
		}
	}
	cacheRoot, err := o.cacheDir()
	if err != nil {
		return false, err
	}
	buildDir, err := os.MkdirTemp("", "togi-same-build-")
	if err != nil {
		return false, fmt.Errorf("create build directory: %w", err)
	}
	defer os.RemoveAll(buildDir)
	var binaries [2]string
	var buildErrs [2]error
	var builds sync.WaitGroup
	for side, tree := range trees {
		builds.Go(func() {
			binaries[side] = filepath.Join(buildDir, sameSides[side])
			args := append([]string{"build", "-C", tree, "-o", binaries[side]}, simulatorBuildFlags...)
			build := exec.CommandContext(o.ctx, "go", append(args, "./tools/sim")...)
			build.Stdout, build.Stderr = stderr, stderr
			if err := build.Run(); err != nil {
				buildErrs[side] = fmt.Errorf("build simulator in %s: %w", tree, err)
			}
		})
	}
	builds.Wait()
	if o.ctx.Err() != nil {
		return false, errors.New("interrupted")
	}
	if err := errors.Join(buildErrs[:]...); err != nil {
		return false, err
	}
	var keys [2]string
	var caches [2]*sessionCache
	for side, tree := range trees {
		if keys[side], err = cacheKey(binaries[side], tree, runs[side], o.maxBoots); err != nil {
			return false, fmt.Errorf("key %s cache: %w", sameSides[side], err)
		}
	}
	if keys[0] == keys[1] && !o.noCache && o.keep == "" {
		fmt.Fprintf(stdout, "same: both trees build the same simulator and read the same inputs; 0 of %d sessions differ\n", len(runs[0]))
		return false, nil
	}
	for side := range caches {
		if caches[side], err = openSessionCache(cacheRoot, keys[side]); err != nil {
			return false, err
		}
	}
	costs := loadCosts(filepath.Join(cacheRoot, "costs.json"))
	defer func() {
		if err := costs.save(); err != nil {
			fmt.Fprintf(stderr, "bench: save session costs: %v\n", err)
		}
	}()
	parent := o.keep
	if parent == "" {
		parent = filepath.Join(cacheRoot, "runs")
	}
	if err := os.MkdirAll(parent, 0755); err != nil {
		return false, fmt.Errorf("create run parent directory: %w", err)
	}
	root, err := os.MkdirTemp(parent, "togi-same-runs-")
	if err != nil {
		return false, fmt.Errorf("create run directory: %w", err)
	}
	launch := func(ctx context.Context, side int, spec runSpec) (simulation, error) {
		return launchSimulator(ctx, binaries[side], filepath.Join(root, sameSides[side]), spec, o.maxBoots, o.timeout)
	}
	different, err := runSame(o.ctx, stdout, pairRuns(runs), sameConfig{
		jobs:      o.jobs,
		keep:      o.keep != "",
		keepGoing: o.keepGoing,
		fresh:     o.noCache || o.keep != "",
		caches:    caches,
		costs:     costs,
		log:       stderr,
	}, launch)
	if o.ctx.Err() != nil {
		err = errors.New("interrupted")
	}
	switch {
	case o.keep != "" && o.ctx.Err() == nil, o.ctx.Err() == nil && (err != nil || different):
		fmt.Fprintf(stderr, "bench: keeping runs in %s\n", root)
	default:
		os.RemoveAll(root)
	}
	return different, err
}

func pairRuns(runs [2][]runSpec) []samePair {
	index := make(map[sessionKey]int)
	var pairs []samePair
	for side := range runs {
		for i := range runs[side] {
			spec := &runs[side][i]
			key := sessionKey{spec.scenario.Name, spec.split, spec.seed}
			n, ok := index[key]
			if !ok {
				n = len(pairs)
				index[key] = n
				pairs = append(pairs, samePair{key: key})
			}
			pairs[n].runs[side] = spec
		}
	}
	slices.SortFunc(pairs, func(a, b samePair) int {
		return cmp.Or(
			strings.Compare(a.key.scenario, b.key.scenario),
			strings.Compare(a.key.split, b.key.split),
			cmp.Compare(a.key.seed, b.key.seed),
		)
	})
	return pairs
}

type sameConfig struct {
	jobs int
	// keep retains every run directory, and keepGoing counts every differing session instead of stopping at the first.
	keep, keepGoing bool
	// fresh ignores cached records; the runs still write them.
	fresh  bool
	caches [2]*sessionCache
	costs  *costTable
	log    io.Writer
}

type sameLauncher func(ctx context.Context, side int, spec runSpec) (simulation, error)

type sameDifference struct {
	pair int
	*journalDifference
}

// sameRun is one comparison of every session of two trees.
type sameRun struct {
	pairs  []samePair
	cfg    sameConfig
	launch sameLauncher
	stop   context.CancelFunc

	mu     sync.Mutex
	states []pairState
	diffs  []sameDifference
}

// pairState holds what is known of one session: each side's record, and the run directory of a side that ran and
// whose pair is not yet compared.
type pairState struct {
	rec  [2]*sessionRecord
	dirs [2]string
}

func (s *pairState) complete(p samePair) bool {
	for side, spec := range p.runs {
		if spec != nil && s.rec[side] == nil {
			return false
		}
	}
	return true
}

// runSame compares every pair of sessions by digest. A side with a cached record is not run; every other side is run
// once, longest recorded session first, and its digest taken from its journals. A difference stops the comparison
// unless keepGoing, and a pair whose digests differ is shown by rerunning whichever side has no run directory left.
func runSame(parent context.Context, w io.Writer, pairs []samePair, cfg sameConfig, launch sameLauncher) (bool, error) {
	if cfg.log == nil {
		cfg.log = io.Discard
	}
	ctx, stop := context.WithCancel(parent)
	defer stop()
	r := &sameRun{pairs: pairs, cfg: cfg, launch: launch, stop: stop, states: make([]pairState, len(pairs))}
	type job struct{ pair, side int }
	var jobs []job
	var ready []int
	var cached, scheduled [2]int
	for i, p := range pairs {
		for side, spec := range p.runs {
			if spec == nil {
				continue
			}
			if rec, ok := cfg.caches[side].get(p.key); ok && !cfg.fresh {
				r.states[i].rec[side] = &rec
				cached[side]++
				continue
			}
			jobs = append(jobs, job{i, side})
			scheduled[side]++
		}
		if r.states[i].complete(p) {
			ready = append(ready, i)
		}
	}
	err := runPool(ctx, cfg.jobs, ready, r.resolve)
	if err == nil && ctx.Err() == nil {
		order := longestFirst(len(jobs), func(n int) sessionKey { return pairs[jobs[n].pair].key }, cfg.costs)
		err = runPool(ctx, cfg.jobs, order, func(ctx context.Context, n int) error {
			return r.runSide(ctx, jobs[n].pair, jobs[n].side)
		})
	}
	r.removeUnresolvedRuns()
	fmt.Fprintf(cfg.log, "bench: base %d cached, %d to run; head %d cached, %d to run\n", cached[0], scheduled[0], cached[1], scheduled[1])
	if err != nil {
		return false, err
	}
	slices.SortFunc(r.diffs, func(a, b sameDifference) int { return cmp.Compare(a.pair, b.pair) })
	for _, d := range r.diffs {
		fmt.Fprintf(w, "same: %s\n  %s:%d\n  base: %s\n  head: %s\n", pairs[d.pair].key, d.file, d.line, d.base, d.head)
	}
	switch {
	case len(r.diffs) != 0 && !cfg.keepGoing:
		fmt.Fprintln(w, "same: stopped at the first difference; --keep-going reports every differing session")
	case parent.Err() == nil:
		fmt.Fprintf(w, "same: %d of %d sessions differ\n", len(r.diffs), len(pairs))
	}
	return len(r.diffs) != 0, nil
}

// runSide runs one side of a session, records its digest and, if that completes the pair, compares the pair.
func (r *sameRun) runSide(ctx context.Context, i, side int) error {
	p := r.pairs[i]
	run, err := r.launch(ctx, side, *p.runs[side])
	if err != nil {
		if ctx.Err() != nil && run.dir != "" {
			os.RemoveAll(run.dir)
		}
		return fmt.Errorf("%s %s: %w", sameSides[side], p.key, err)
	}
	rec, err := digestRun(run)
	if err != nil {
		return fmt.Errorf("%s %s: %w", sameSides[side], p.key, err)
	}
	if err := r.cfg.caches[side].put(p.key, rec); err != nil {
		fmt.Fprintf(r.cfg.log, "bench: cache %s %s: %v\n", sameSides[side], p.key, err)
	}
	r.cfg.costs.set(p.key, run.wall)
	r.mu.Lock()
	st := &r.states[i]
	st.rec[side], st.dirs[side] = &rec, run.dir
	complete := st.complete(p)
	r.mu.Unlock()
	if !complete {
		return nil
	}
	return r.resolve(ctx, i)
}

// resolve compares a session whose sides all have records, and drops its run directories when they match.
func (r *sameRun) resolve(ctx context.Context, i int) error {
	p, st := r.pairs[i], &r.states[i]
	var diff *journalDifference
	switch {
	case p.runs[0] == nil:
		diff = &journalDifference{file: "<session>", base: "<missing>", head: "<present>"}
	case p.runs[1] == nil:
		diff = &journalDifference{file: "<session>", base: "<present>", head: "<missing>"}
	case st.rec[0].Digest != st.rec[1].Digest:
		var err error
		if diff, err = r.journalDifference(ctx, i); err != nil {
			return fmt.Errorf("compare %s: %w", p.key, err)
		}
	case st.rec[0].Exit != st.rec[1].Exit:
		diff = &journalDifference{file: "<exit code>", base: fmt.Sprint(st.rec[0].Exit), head: fmt.Sprint(st.rec[1].Exit)}
	}
	if diff == nil {
		return r.removeRuns(i)
	}
	r.mu.Lock()
	r.diffs = append(r.diffs, sameDifference{i, diff})
	r.mu.Unlock()
	if !r.cfg.keepGoing {
		r.stop()
	}
	return nil
}

// journalDifference finds the first differing line of a session whose digests differ, rerunning any side that has no
// run directory.
func (r *sameRun) journalDifference(ctx context.Context, i int) (*journalDifference, error) {
	p, st := r.pairs[i], &r.states[i]
	for side, spec := range p.runs {
		if st.dirs[side] != "" {
			continue
		}
		run, err := r.launch(ctx, side, *spec)
		if err != nil {
			return nil, fmt.Errorf("rerun %s: %w", sameSides[side], err)
		}
		st.dirs[side] = run.dir
	}
	diff, err := compareJournals(st.dirs[0], st.dirs[1])
	if diff == nil && err == nil {
		diff = &journalDifference{file: "<journal digest>", base: st.rec[0].Digest, head: st.rec[1].Digest}
	}
	return diff, err
}

func (r *sameRun) removeRuns(i int) error {
	if r.cfg.keep {
		return nil
	}
	st := &r.states[i]
	for side, dir := range st.dirs {
		if dir == "" {
			continue
		}
		if err := os.RemoveAll(dir); err != nil {
			return fmt.Errorf("remove matching %s %s: %w", sameSides[side], r.pairs[i].key, err)
		}
		st.dirs[side] = ""
	}
	return nil
}

// removeUnresolvedRuns drops the run directories of sessions stopped before their comparison.
func (r *sameRun) removeUnresolvedRuns() {
	differing := make(map[int]bool)
	for _, d := range r.diffs {
		differing[d.pair] = true
	}
	for i := range r.states {
		if !differing[i] {
			r.removeRuns(i)
		}
	}
}

// digestRun takes the digest of a finished simulation's journals.
func digestRun(run simulation) (sessionRecord, error) {
	digest, err := journalDigest(run.dir)
	return sessionRecord{Digest: digest, Exit: run.exit, WallS: run.wall}, err
}

// journalDigest hashes a run's current and archived journals after build normalization, with their names.
func journalDigest(dir string) (string, error) {
	files, err := journalFiles(dir)
	if err != nil {
		return "", fmt.Errorf("list journals: %w", err)
	}
	if len(files) == 0 {
		return "", errors.New("missing journals")
	}
	names := make([]string, 0, len(files))
	for name := range files {
		names = append(names, name)
	}
	slices.Sort(names)
	total := sha256.New()
	nonempty := false
	for _, name := range names {
		sum, size, err := normalizedDigest(files[name])
		if err != nil {
			return "", fmt.Errorf("digest journal %s: %w", name, err)
		}
		nonempty = nonempty || size > 0
		fmt.Fprintf(total, "%s %d %x\n", name, size, sum)
	}
	if !nonempty {
		return "", errors.New("journals contain no events")
	}
	return hex.EncodeToString(total.Sum(nil)), nil
}

// normalizedDigest hashes one journal file line by line after build normalization, and returns its normalized size.
func normalizedDigest(path string) ([]byte, int64, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, 0, err
	}
	defer f.Close()
	h := sha256.New()
	reader := bufio.NewReaderSize(f, 1<<20)
	var long []byte
	var size int64
	for number := 1; ; number++ {
		line, err := reader.ReadSlice('\n')
		if errors.Is(err, bufio.ErrBufferFull) {
			long = append(long[:0], line...)
			for errors.Is(err, bufio.ErrBufferFull) {
				line, err = reader.ReadSlice('\n')
				long = append(long, line...)
			}
			line = long
		}
		if len(line) > 0 {
			normalized, normalizeErr := normalizeBuild(line)
			if normalizeErr != nil {
				return nil, 0, fmt.Errorf("line %d: %w", number, normalizeErr)
			}
			h.Write(normalized)
			size += int64(len(normalized))
		}
		if errors.Is(err, io.EOF) {
			return h.Sum(nil), size, nil
		} else if err != nil {
			return nil, 0, err
		}
	}
}

func journalFiles(dir string) (map[string]string, error) {
	files := make(map[string]string)
	err := filepath.WalkDir(dir, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(dir, path)
		if err != nil {
			return err
		}
		if entry.IsDir() {
			if rel != "." && rel != "archive" {
				return filepath.SkipDir
			}
			return nil
		}
		if rel == "events.jsonl" || (filepath.Dir(rel) == "archive" && strings.HasSuffix(rel, ".jsonl")) {
			files[rel] = path
		}
		return nil
	})
	return files, err
}

func compareJournals(base, head string) (*journalDifference, error) {
	a, err := journalFiles(base)
	if err != nil {
		return nil, fmt.Errorf("list base journals: %w", err)
	}
	b, err := journalFiles(head)
	if err != nil {
		return nil, fmt.Errorf("list head journals: %w", err)
	}
	if len(a) == 0 || len(b) == 0 {
		return nil, fmt.Errorf("missing journals: base has %d, head has %d", len(a), len(b))
	}
	for side, files := range [2]map[string]string{a, b} {
		nonempty := false
		for _, path := range files {
			info, err := os.Stat(path)
			if err != nil {
				return nil, fmt.Errorf("inspect journal %s: %w", path, err)
			}
			nonempty = nonempty || info.Size() > 0
		}
		if !nonempty {
			return nil, fmt.Errorf("%s journals contain no events", []string{"base", "head"}[side])
		}
	}
	paths := make([]string, 0, len(a)+len(b))
	for path := range a {
		paths = append(paths, path)
	}
	for path := range b {
		if _, ok := a[path]; !ok {
			paths = append(paths, path)
		}
	}
	slices.Sort(paths)
	for _, path := range paths {
		diff, err := compareJournalFile(path, [2]string{a[path], b[path]})
		if err != nil {
			return nil, fmt.Errorf("compare journal %s: %w", path, err)
		}
		if diff != nil {
			return diff, nil
		}
	}
	return nil, nil
}

func compareJournalFile(path string, files [2]string) (*journalDifference, error) {
	var readers [2]io.Reader
	for side, file := range files {
		if file == "" {
			readers[side] = strings.NewReader("")
			continue
		}
		f, err := os.Open(file)
		if err != nil {
			return nil, fmt.Errorf("open journal %s: %w", file, err)
		}
		defer f.Close()
		readers[side] = f
	}
	diff, err := firstJournalDifference(readers[0], readers[1])
	if err != nil {
		return nil, err
	}
	if diff == nil && (files[0] == "" || files[1] == "") {
		diff = &journalDifference{line: 1, base: "<empty file>", head: "<empty file>"}
	}
	if diff != nil {
		diff.file = path
		if files[0] == "" {
			diff.base = "<missing file>"
		}
		if files[1] == "" {
			diff.head = "<missing file>"
		}
	}
	return diff, nil
}

func firstJournalDifference(base, head io.Reader) (*journalDifference, error) {
	readers := [2]*bufio.Reader{bufio.NewReader(base), bufio.NewReader(head)}
	for number := 1; ; number++ {
		var lines [2][]byte
		for side, reader := range readers {
			line, err := reader.ReadBytes('\n')
			if err != nil && !errors.Is(err, io.EOF) {
				return nil, err
			}
			if len(line) > 0 {
				lines[side], err = normalizeBuild(line)
				if err != nil {
					return nil, fmt.Errorf("line %d: %w", number, err)
				}
			}
		}
		if bytes.Equal(lines[0], lines[1]) {
			if len(lines[0]) == 0 {
				return nil, nil
			}
			continue
		}
		var text [2]string
		for side, line := range lines {
			text[side] = strings.TrimSuffix(string(line), "\n")
			if len(line) == 0 {
				text[side] = "<missing event>"
			}
		}
		return &journalDifference{line: number, base: text[0], head: text[1]}, nil
	}
}

var buildStampKinds = [][]byte{[]byte(journal.KindSessionStart), []byte(journal.KindConfigLoaded)}

// normalizeBuild removes the build stamp from a session.start or config.loaded line and returns every other line as
// it is, without decoding it.
func normalizeBuild(line []byte) ([]byte, error) {
	if !slices.ContainsFunc(buildStampKinds, func(kind []byte) bool { return bytes.Contains(line, kind) }) {
		return line, nil
	}
	var kind struct {
		Kind journal.Kind `json:"kind"`
	}
	if err := json.Unmarshal(line, &kind); err != nil {
		return nil, fmt.Errorf("decode event: %w", err)
	}
	if kind.Kind != journal.KindSessionStart && kind.Kind != journal.KindConfigLoaded {
		return line, nil
	}
	var stamp struct {
		journal.Build
		Session string `json:"session"`
		Path    string `json:"path"`
		File    bool   `json:"file"`
	}
	if err := json.Unmarshal(line, &stamp); err != nil {
		return nil, fmt.Errorf("decode event: %w", err)
	}
	prefix := "session " + stamp.Session + " started by "
	if kind.Kind == journal.KindConfigLoaded {
		prefix = "no config at " + stamp.Path + "; using defaults with "
		if stamp.File {
			prefix = "config loaded from " + stamp.Path + " by "
		}
	}
	description := "a togi build from before version stamps"
	if stamp.Version != "" {
		description = "togi " + stamp.Version
		if stamp.Rev != "" {
			description += "+" + stamp.Rev
		}
	}
	encodedPrefix, _ := json.Marshal(prefix)
	encodedDescription, _ := json.Marshal(description)
	messageStart := append(bytes.Clone(encodedPrefix[:len(encodedPrefix)-1]), encodedDescription[1:len(encodedDescription)-1]...)
	decoder := json.NewDecoder(bytes.NewReader(line))
	if _, err := decoder.Token(); err != nil {
		return nil, err
	}
	end := int(decoder.InputOffset())
	opening := end
	normalized := bytes.Clone(line[:end])
	first := true
	for decoder.More() {
		separator := end
		key, err := decoder.Token()
		if err != nil {
			return nil, err
		}
		var raw json.RawMessage
		if err := decoder.Decode(&raw); err != nil {
			return nil, err
		}
		end = int(decoder.InputOffset())
		if key == "version" || key == "rev" {
			continue
		}
		start := separator
		if first && separator != opening {
			for start < end && line[start] != '"' {
				start++
			}
		}
		if key == "msg" && bytes.HasPrefix(raw, messageStart) && bytes.HasPrefix(raw[len(messageStart):], []byte(" (schema ")) {
			value := end - len(raw)
			normalized = append(normalized, line[start:value]...)
			normalized = append(normalized, encodedPrefix[:len(encodedPrefix)-1]...)
			normalized = append(normalized, raw[len(messageStart):]...)
		} else {
			normalized = append(normalized, line[start:end]...)
		}
		first = false
	}
	return append(normalized, line[end:]...), nil
}
