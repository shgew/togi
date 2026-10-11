// Command compare is the Go side of the comparison harness: it is the reference subject, writing what Go decodes,
// re-encodes and projects into state.json for a journal, and it lists the bench suite's sessions and the committed
// fixtures, goldens and inputs (the suite file, machines, recorded facts and every other testdata file the comparisons
// read) the manifests freeze. It is a development program and never ships.
package main

import (
	"bytes"
	"compress/gzip"
	"crypto/sha256"
	"encoding/json"
	jsonv2 "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"strings"

	"github.com/shgew/togi/internal/journal"
)

const usage = `usage:
  compare live|history JOURNAL OUTDIR  write OUTDIR/encoded.jsonl and OUTDIR/state.json for JOURNAL
  compare suite SUITE                  print the suite's sessions: scenario, split, seed and machine
  compare fixtures ROOT                print each committed testdata JSONL file under ROOT: its kind, path and hash
  compare goldens ROOT                 print the path and hash of each golden file the comparison protects
  compare inputs ROOT                  print the path and hash of the suite file, machines and recorded facts, and of
                                       every other file in the ported packages' testdata that no other manifest lists`

func main() {
	os.Exit(run(os.Args[1:], os.Stdout, os.Stderr))
}

func run(args []string, stdout, stderr io.Writer) int {
	var err error
	switch {
	case len(args) == 3 && (args[0] == "live" || args[0] == "history"):
		err = expect(args[0], args[1], args[2])
	case len(args) == 2 && args[0] == "suite":
		err = listSuite(args[1], stdout)
	case len(args) == 2 && args[0] == "fixtures":
		err = fixtures(args[1], stdout)
	case len(args) == 2 && args[0] == "goldens":
		err = listFiles(args[1], stdout, goldenRoots, isGolden)
	case len(args) == 2 && args[0] == "inputs":
		err = inputs(args[1], stdout)
	default:
		fmt.Fprintln(stderr, usage)
		return 2
	}
	if err != nil {
		fmt.Fprintf(stderr, "compare: %v\n", err)
		return 1
	}
	return 0
}

// expect decodes a journal the way the mode names, live as togi resumes it or history as stats and replay read older
// schemas, and writes each event re-encoded and the state the events fold into.
func expect(mode, path, dir string) error {
	var events []journal.Event
	var err error
	if mode == "live" {
		events, _, err = journal.ReadFile(path)
	} else {
		events, err = journal.ReadHistory(path)
	}
	if err != nil {
		return err
	}
	var encoded bytes.Buffer
	for _, e := range events {
		line, err := e.MarshalJSON()
		if err != nil {
			return fmt.Errorf("encode event %d: %w", e.Seq, err)
		}
		encoded.Write(stamped(e, line))
		encoded.WriteByte('\n')
	}
	var state journal.State
	journal.Replay(events, &state)
	projected, err := jsonv2.Marshal(state, json.DefaultOptionsV1())
	if err != nil {
		return fmt.Errorf("encode state: %w", err)
	}
	projected = append(projected, '\n')
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	for name, data := range map[string][]byte{"encoded.jsonl": encoded.Bytes(), "state.json": projected} {
		if err := os.WriteFile(filepath.Join(dir, name), data, 0o644); err != nil {
			return err
		}
	}
	return nil
}

// stamped puts back the zero monotonic stamp that Event.MarshalJSON leaves out, as the journal's own append does for a
// journal that stamps: the stamp is present in the line the event was decoded from, directly after the sequence number.
func stamped(e journal.Event, line []byte) []byte {
	seq := fmt.Sprintf(`{"seq":%d`, e.Seq)
	if e.Mono != 0 || !bytes.HasPrefix(e.Raw, []byte(seq+`,"mono_ms":`)) || !bytes.HasPrefix(line, []byte(seq+",")) {
		return line
	}
	return append([]byte(seq+`,"mono_ms":0`), line[len(seq):]...)
}

type scenario struct {
	Name     string   `json:"name"`
	Machine  string   `json:"machine"`
	Machines []string `json:"machines"`
	Dev      []uint64 `json:"dev"`
	Holdout  []uint64 `json:"holdout"`
}

type session struct {
	scenario, split, machine string
	seed                     uint64
}

func suiteLine(s session) string {
	machine := s.machine
	if machine == "" {
		machine = "-"
	}
	return fmt.Sprintf("%s\t%s\t%d\t%s", s.scenario, s.split, s.seed, machine)
}

// listSuite prints the suite's dev and holdout sessions, sorted, with each machine chosen as tools/bench chooses it:
// the seed's index in its split modulo the scenario's machine ensemble, the path resolved beside the suite file.
func listSuite(path string, w io.Writer) error {
	data, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	var suite struct {
		Scenarios []scenario `json:"scenarios"`
	}
	if err := json.Unmarshal(data, &suite); err != nil {
		return fmt.Errorf("decode suite %s: %w", path, err)
	}
	var lines []string
	for _, s := range suite.Scenarios {
		machines := s.Machines
		if len(machines) == 0 {
			machines = []string{s.Machine}
		}
		for _, group := range []struct {
			split string
			seeds []uint64
		}{{"dev", s.Dev}, {"holdout", s.Holdout}} {
			for i, seed := range group.seeds {
				machine := machines[i%len(machines)]
				if machine != "" && !filepath.IsAbs(machine) {
					machine = filepath.Join(filepath.Dir(path), machine)
				}
				lines = append(lines, suiteLine(session{s.Name, group.split, machine, seed}))
			}
		}
	}
	if len(lines) == 0 {
		return errors.New("suite has no sessions")
	}
	slices.Sort(lines)
	_, err = io.WriteString(w, strings.Join(lines, "\n")+"\n")
	return err
}

var (
	// goldenRoots are the ported packages whose .golden files the comparison protects; tools/board and tools/reviews
	// are rewritten rather than ported, so their goldens stay out.
	goldenRoots = []string{"cmd/togi", "internal/watch", "tools/fit", "tools/stats", "tools/bench"}
	// inputRoots are the bench suite's own inputs.
	inputRoots = []string{"tools/bench/suite.json", "tools/bench/machines", "tools/bench/facts"}
	// inputPackages are the packages whose testdata the comparisons read; every file there the fixtures or goldens
	// manifest does not list is an input.
	inputPackages = []string{
		"internal/carry", "internal/journal", "internal/watch", "internal/tuner", "internal/defect",
		"cmd/togi", "tools/replay", "tools/stats",
	}
)

// isFixture reports whether the fixtures manifest lists path: a JSONL file, plain or gzipped, in a testdata directory.
func isFixture(path string) bool {
	return strings.Contains(filepath.ToSlash(path), "/testdata/") &&
		(strings.HasSuffix(path, ".jsonl") || strings.HasSuffix(path, ".jsonl.gz"))
}

// isGolden reports whether path is a golden file; the goldens manifest lists those under goldenRoots.
func isGolden(path string) bool { return strings.HasSuffix(path, ".golden") }

// inputs prints "path hash" for the bench suite's inputs and for every file under an input package's testdata that
// neither the fixtures nor the goldens manifest lists, so each committed file the comparisons read is in exactly one
// manifest.
func inputs(root string, w io.Writer) error {
	roots := slices.Clone(inputRoots)
	for _, p := range inputPackages {
		roots = append(roots, p+"/testdata")
	}
	return listFiles(root, w, roots, func(path string) bool {
		if isFixture(path) {
			return false
		}
		if !isGolden(path) {
			return true
		}
		rel, err := filepath.Rel(root, path)
		if err != nil {
			return true
		}
		rel = filepath.ToSlash(rel)
		return !slices.ContainsFunc(goldenRoots, func(r string) bool { return strings.HasPrefix(rel, r+"/") })
	})
}

// listFiles prints "path hash" for each file under the roots, sorted, that the filter keeps.
func listFiles(root string, w io.Writer, roots []string, keep func(path string) bool) error {
	var lines []string
	for _, r := range roots {
		err := filepath.WalkDir(filepath.Join(root, r), func(path string, d fs.DirEntry, err error) error {
			if err != nil || d.IsDir() || !keep(path) {
				return err
			}
			line, err := hashLine(root, path)
			lines = append(lines, line)
			return err
		})
		if err != nil {
			return err
		}
	}
	return printSorted(w, lines)
}

func hashLine(root, path string) (string, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	rel, err := filepath.Rel(root, path)
	if err != nil {
		return "", err
	}
	return fmt.Sprintf("%s\t%x", filepath.ToSlash(rel), sha256.Sum256(data)), nil
}

func printSorted(w io.Writer, lines []string) error {
	if len(lines) == 0 {
		return errors.New("nothing to list")
	}
	slices.Sort(lines)
	_, err := io.WriteString(w, strings.Join(lines, "\n")+"\n")
	return err
}

// fixtures classifies every JSONL file in a testdata directory. A journal is a file Go reads whole, live when this
// schema resumes it, otherwise as history; anything else is a sample, an input or a fragment of a journal.
func fixtures(root string, w io.Writer) error {
	tmp, err := os.MkdirTemp("", "compare-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(tmp)
	var lines []string
	err = filepath.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
		if err != nil || d.IsDir() {
			return err
		}
		if !isFixture(path) {
			return nil
		}
		hashed, err := hashLine(root, path)
		if err != nil {
			return err
		}
		kind, err := classify(path, tmp)
		if err != nil {
			return err
		}
		lines = append(lines, kind+"\t"+hashed)
		return nil
	})
	if err != nil {
		return err
	}
	return printSorted(w, lines)
}

func classify(path, tmp string) (string, error) {
	plain := filepath.Join(tmp, "journal.jsonl")
	if err := unpack(path, plain); err != nil {
		return "", err
	}
	for _, mode := range []string{"live", "history"} {
		if err := expect(mode, plain, filepath.Join(tmp, "out")); err == nil {
			return "journal-" + mode, nil
		}
	}
	return "not-journal", nil
}

func unpack(path, to string) error {
	f, err := os.Open(path)
	if err != nil {
		return err
	}
	defer f.Close()
	var r io.Reader = f
	if strings.HasSuffix(path, ".gz") {
		z, err := gzip.NewReader(f)
		if err != nil {
			return err
		}
		defer z.Close()
		r = z
	}
	out, err := os.Create(to)
	if err != nil {
		return err
	}
	if _, err := io.Copy(out, r); err != nil {
		out.Close()
		return err
	}
	return out.Close()
}
