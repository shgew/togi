package main

import (
	"bufio"
	"bytes"
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

type sameSession struct {
	key sessionKey
	run simulation
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
	if o.keep != "" {
		if err := os.MkdirAll(o.keep, 0755); err != nil {
			return false, fmt.Errorf("create keep directory: %w", err)
		}
	}
	root, err := os.MkdirTemp(o.keep, "togi-same-runs-")
	if err != nil {
		return false, fmt.Errorf("create run directory: %w", err)
	}
	retain := true
	defer func() {
		if retain || o.keep != "" {
			fmt.Fprintf(stderr, "bench: keeping runs in %s\n", root)
		} else {
			os.RemoveAll(root)
		}
	}()
	type job struct{ side, index int }
	var sessions [2][]sameSession
	var errs [2][]error
	for side := range runs {
		sessions[side] = make([]sameSession, len(runs[side]))
		errs[side] = make([]error, len(runs[side]))
	}
	queue := make(chan job)
	var wg sync.WaitGroup
	for range min(o.jobs, len(runs[0])+len(runs[1])) {
		wg.Go(func() {
			for j := range queue {
				spec := runs[j.side][j.index]
				side := []string{"base", "head"}[j.side]
				run, err := launchSimulator(binaries[j.side], filepath.Join(root, side), spec, o.timeout)
				if err == nil && run.timedOut {
					err = fmt.Errorf("simulator timed out after %s", o.timeout)
				}
				sessions[j.side][j.index] = sameSession{sessionKey{spec.scenario.Name, spec.split, spec.seed}, run}
				if err != nil {
					errs[j.side][j.index] = fmt.Errorf("%s %s/%s-%d: %w", side, spec.scenario.Name, spec.split, spec.seed, err)
				}
			}
		})
	}
	for side := range runs {
		for index := range runs[side] {
			queue <- job{side, index}
		}
	}
	close(queue)
	wg.Wait()
	if err := errors.Join(append(errs[0], errs[1]...)...); err != nil {
		return false, err
	}
	different, err := compareSessions(stdout, sessions[0], sessions[1])
	if err != nil {
		return false, err
	}
	retain = different
	return different, nil
}

func compareSessions(w io.Writer, base, head []sameSession) (bool, error) {
	paired := make(map[sessionKey][2]*simulation)
	for side, sessions := range [2][]sameSession{base, head} {
		for _, session := range sessions {
			pair := paired[session.key]
			pair[side] = &session.run
			paired[session.key] = pair
		}
	}
	keys := make([]sessionKey, 0, len(paired))
	for key := range paired {
		keys = append(keys, key)
	}
	slices.SortFunc(keys, func(a, b sessionKey) int {
		if n := strings.Compare(a.scenario, b.scenario); n != 0 {
			return n
		}
		if n := strings.Compare(a.split, b.split); n != 0 {
			return n
		}
		if a.seed < b.seed {
			return -1
		}
		if a.seed > b.seed {
			return 1
		}
		return 0
	})
	differences := 0
	for _, key := range keys {
		pair := paired[key]
		var diff *journalDifference
		switch {
		case pair[0] == nil:
			diff = &journalDifference{file: "<session>", base: "<missing>", head: "<present>"}
		case pair[1] == nil:
			diff = &journalDifference{file: "<session>", base: "<present>", head: "<missing>"}
		default:
			var err error
			diff, err = compareJournals(pair[0].dir, pair[1].dir)
			if err != nil {
				return false, fmt.Errorf("compare %s/%s-%d: %w", key.scenario, key.split, key.seed, err)
			}
			if diff == nil && pair[0].exit != pair[1].exit {
				diff = &journalDifference{file: "<exit code>", base: fmt.Sprint(pair[0].exit), head: fmt.Sprint(pair[1].exit)}
			}
		}
		if diff != nil {
			differences++
			fmt.Fprintf(w, "same: %s/%s-%d\n  %s:%d\n  base: %s\n  head: %s\n", key.scenario, key.split, key.seed, diff.file, diff.line, diff.base, diff.head)
		}
	}
	fmt.Fprintf(w, "same: %d of %d sessions differ\n", differences, len(keys))
	return differences != 0, nil
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
		if first && separator != 1 {
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
