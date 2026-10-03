package main

import (
	"bufio"
	"bytes"
	"cmp"
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
	var runs [2][]runSpec
	for side, tree := range trees {
		suite := o.suite
		if !filepath.IsAbs(suite) {
			suite = filepath.Join(tree, suite)
		}
		runs[side], err = loadRuns(suite, "all")
		if err != nil {
			return false, fmt.Errorf("load %s suite: %w", tree, err)
		}
	}
	buildDir, err := os.MkdirTemp("", "togi-same-build-")
	if err != nil {
		return false, fmt.Errorf("create build directory: %w", err)
	}
	defer os.RemoveAll(buildDir)
	var binaries [2]string
	for side, tree := range trees {
		binaries[side] = filepath.Join(buildDir, fmt.Sprintf("sim-%d", side))
		args := []string{"build", "-C", tree, "-o", binaries[side]}
		if side == 0 {
			args = append(args, "-buildvcs=false")
		}
		build := exec.Command("go", append(args, "./tools/sim")...)
		build.Stdout, build.Stderr = stderr, stderr
		if err := build.Run(); err != nil {
			return false, fmt.Errorf("build simulator in %s: %w", tree, err)
		}
	}
	parent := o.keep
	if parent == "" {
		cache, err := os.UserCacheDir()
		if err != nil {
			return false, fmt.Errorf("resolve cache directory: %w", err)
		}
		parent = filepath.Join(cache, "togi")
	}
	if err := os.MkdirAll(parent, 0755); err != nil {
		return false, fmt.Errorf("create run parent directory: %w", err)
	}
	root, err := os.MkdirTemp(parent, "togi-same-runs-")
	if err != nil {
		return false, fmt.Errorf("create run directory: %w", err)
	}
	launch := func(side int, spec runSpec) (simulation, error) {
		run, err := launchSimulator(binaries[side], filepath.Join(root, sameSides[side]), spec, o.timeout)
		if err == nil && run.timedOut {
			err = fmt.Errorf("simulator timed out after %s", o.timeout)
		}
		return run, err
	}
	different, err := runSame(stdout, pairRuns(runs), o.jobs, o.keep != "", launch)
	if err != nil || different || o.keep != "" {
		fmt.Fprintf(stderr, "bench: keeping runs in %s\n", root)
	} else {
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

func runSame(w io.Writer, pairs []samePair, jobs int, keep bool, launch func(side int, spec runSpec) (simulation, error)) (bool, error) {
	diffs := make([]*journalDifference, len(pairs))
	errs := make([]error, len(pairs))
	queue := make(chan int)
	var wg sync.WaitGroup
	for range min(jobs, len(pairs)) {
		wg.Go(func() {
			for i := range queue {
				diffs[i], errs[i] = runSamePair(pairs[i], keep, launch)
			}
		})
	}
	for i := range pairs {
		queue <- i
	}
	close(queue)
	wg.Wait()
	if err := errors.Join(errs...); err != nil {
		return false, err
	}
	differences := 0
	for i, diff := range diffs {
		if diff != nil {
			differences++
			fmt.Fprintf(w, "same: %s\n  %s:%d\n  base: %s\n  head: %s\n", pairs[i].key, diff.file, diff.line, diff.base, diff.head)
		}
	}
	fmt.Fprintf(w, "same: %d of %d sessions differ\n", differences, len(pairs))
	return differences != 0, nil
}

func runSamePair(pair samePair, keep bool, launch func(side int, spec runSpec) (simulation, error)) (*journalDifference, error) {
	var sims [2]*simulation
	var errs []error
	for side, spec := range pair.runs {
		if spec == nil {
			continue
		}
		run, err := launch(side, *spec)
		if err != nil {
			errs = append(errs, fmt.Errorf("%s %s: %w", sameSides[side], pair.key, err))
			continue
		}
		sims[side] = &run
	}
	if len(errs) != 0 {
		return nil, errors.Join(errs...)
	}
	diff, err := compareSession(sims)
	if err != nil {
		return nil, fmt.Errorf("compare %s: %w", pair.key, err)
	}
	if diff == nil && !keep {
		for side, run := range sims {
			if err := os.RemoveAll(run.dir); err != nil {
				return nil, fmt.Errorf("remove matching %s %s: %w", sameSides[side], pair.key, err)
			}
		}
	}
	return diff, nil
}

func compareSession(sims [2]*simulation) (*journalDifference, error) {
	switch {
	case sims[0] == nil:
		return &journalDifference{file: "<session>", base: "<missing>", head: "<present>"}, nil
	case sims[1] == nil:
		return &journalDifference{file: "<session>", base: "<present>", head: "<missing>"}, nil
	}
	diff, err := compareJournals(sims[0].dir, sims[1].dir)
	if err != nil {
		return nil, err
	}
	if diff == nil && sims[0].exit != sims[1].exit {
		diff = &journalDifference{file: "<exit code>", base: fmt.Sprint(sims[0].exit), head: fmt.Sprint(sims[1].exit)}
	}
	return diff, nil
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

func normalizeBuild(line []byte) ([]byte, error) {
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
