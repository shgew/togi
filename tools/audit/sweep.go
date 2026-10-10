package main

import (
	"encoding/json"
	"encoding/json/jsontext"
	jsonv2 "encoding/json/v2"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
)

type scenario struct {
	Name     string   `json:"name"`
	Machine  string   `json:"machine,omitzero"`
	Machines []string `json:"machines,omitzero"`
	Replay   bool     `json:"replay,omitzero"`
	Dev      []uint64 `json:"dev"`
	Holdout  []uint64 `json:"holdout,omitzero"`
	Smoke    []uint64 `json:"smoke,omitzero"`
}
type suite struct {
	// Gate is a ruleset gate of the bench suite; a sweep covers other seeds, so its output omits it.
	Gate      map[string]any `json:"gate,omitzero"`
	Scenarios []scenario     `json:"scenarios"`
}
type runRecord struct {
	Scenario string  `json:"scenario"`
	Seed     uint64  `json:"seed"`
	Split    string  `json:"split"`
	Status   string  `json:"status"`
	WallS    float64 `json:"wall_s"`
}

func sweep(o options, _, stderr io.Writer) (string, string, error) {
	input, err := loadSweepSuite(o.suite, o.seeds)
	if err != nil {
		return "", "", err
	}
	parent := o.keep
	if parent == "" {
		parent, err = os.MkdirTemp("", "togi-audit-")
	} else {
		err = os.MkdirAll(parent, 0755)
	}
	if err != nil {
		return "", "", fmt.Errorf("create evidence directory: %w", err)
	}
	// Go package discovery skips underscore-prefixed evidence trees kept in a checkout.
	evidence, err := os.MkdirTemp(parent, "_sweep-")
	if err != nil {
		return "", "", fmt.Errorf("create sweep directory: %w", err)
	}
	suitePath := filepath.Join(evidence, "suite.json")
	if err := writeSweepSuite(suitePath, input); err != nil {
		return "", "", err
	}
	records := filepath.Join(evidence, "records.jsonl")
	log, err := os.Create(filepath.Join(evidence, "bench.log"))
	if err != nil {
		return "", "", fmt.Errorf("create bench log: %w", err)
	}
	cmd := exec.Command("go", "run", "./tools/bench", "--suite", suitePath, "--split", "dev", "--keep", evidence, "--out", records, "--jobs", fmt.Sprint(o.jobs), "--timeout", o.timeout.String())
	cmd.Stdout, cmd.Stderr = io.MultiWriter(stderr, log), io.MultiWriter(stderr, log)
	fmt.Fprintf(stderr, "audit: sweep scenarios=%d seeds=%d runs=%d evidence=%s\n", len(input.Scenarios), o.seeds, len(input.Scenarios)*o.seeds, evidence)
	runErr := cmd.Run()
	closeErr := log.Close()
	if runErr != nil {
		return "", "", fmt.Errorf("bench sweep (evidence %s): %w", evidence, runErr)
	}
	if closeErr != nil {
		return "", "", fmt.Errorf("close bench log: %w", closeErr)
	}
	// The evidence path may contain glob metacharacters, so list it literally.
	entries, err := os.ReadDir(evidence)
	if err != nil {
		return "", "", fmt.Errorf("find retained bench root: %w", err)
	}
	var roots []string
	for _, entry := range entries {
		if entry.IsDir() && strings.HasPrefix(entry.Name(), "bench-") {
			roots = append(roots, filepath.Join(evidence, entry.Name()))
		}
	}
	if len(roots) != 1 {
		return "", "", fmt.Errorf("find retained bench root in %s: got %d roots", evidence, len(roots))
	}
	fmt.Fprintf(stderr, "audit: records=%s root=%s\n", records, roots[0])
	return records, roots[0], nil
}

// loadSweepSuite strictly reads the audit suite at path and returns it as a dev-only sweep over seeds 1..seeds,
// with relative machine paths resolved against the suite's directory.
func loadSweepSuite(path string, seeds int) (suite, error) {
	var input suite
	data, err := os.ReadFile(path)
	if err != nil {
		return suite{}, fmt.Errorf("read suite: %w", err)
	}
	if err := jsonv2.Unmarshal(data, &input, jsonv2.RejectUnknownMembers(true)); err != nil {
		return suite{}, fmt.Errorf("read suite: %w", err)
	}
	if len(input.Scenarios) == 0 {
		return suite{}, fmt.Errorf("suite has no scenarios")
	}
	source, err := filepath.Abs(filepath.Dir(path))
	if err != nil {
		return suite{}, fmt.Errorf("resolve suite: %w", err)
	}
	return sweepSuite(input, source, seeds), nil
}

// writeSweepSuite writes s as the suite file tools/bench reads.
func writeSweepSuite(path string, s suite) error {
	encoded, err := jsonv2.Marshal(s, jsontext.WithIndent("  "))
	if err != nil {
		return fmt.Errorf("write sweep suite: %w", err)
	}
	if err := os.WriteFile(path, append(encoded, '\n'), 0o644); err != nil {
		return fmt.Errorf("write sweep suite: %w", err)
	}
	return nil
}

func sweepSuite(input suite, base string, seeds int) suite {
	input.Gate = nil
	for i := range input.Scenarios {
		s := &input.Scenarios[i]
		if s.Machine != "" && !filepath.IsAbs(s.Machine) {
			s.Machine = filepath.Join(base, s.Machine)
		}
		for j, path := range s.Machines {
			if path != "" && !filepath.IsAbs(path) {
				s.Machines[j] = filepath.Join(base, path)
			}
		}
		s.Dev = make([]uint64, seeds)
		for j := range seeds {
			s.Dev[j] = uint64(j + 1)
		}
		s.Holdout, s.Smoke = nil, nil
	}
	return input
}

func readRecords(path string) ([]runRecord, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, fmt.Errorf("open run records: %w", err)
	}
	defer f.Close()
	decoder := json.NewDecoder(f)
	var records []runRecord
	for {
		var r runRecord
		err := decoder.Decode(&r)
		if err == io.EOF {
			break
		}
		if err != nil {
			return nil, fmt.Errorf("decode run records: %w", err)
		}
		records = append(records, r)
	}
	if len(records) == 0 {
		return nil, fmt.Errorf("run records are empty")
	}
	for _, r := range records {
		if r.Scenario == "" || filepath.Base(r.Scenario) != r.Scenario || r.Scenario == "." || r.Scenario == ".." || (r.Split != "dev" && r.Split != "holdout") || r.WallS < 0 {
			return nil, fmt.Errorf("invalid run record for scenario %q seed %d", r.Scenario, r.Seed)
		}
	}
	return records, nil
}
func runDirectory(root string, r runRecord) string {
	return filepath.Join(root, r.Scenario, fmt.Sprintf("%s-%d", r.Split, r.Seed))
}
func timingViolations(records []runRecord, root string) []violation {
	times := make(map[string][]float64)
	for _, r := range records {
		times[r.Scenario] = append(times[r.Scenario], r.WallS)
	}
	medians := make(map[string]float64)
	for name, values := range times {
		slices.Sort(values)
		n := len(values)
		m := values[n/2]
		if n%2 == 0 {
			m = (values[n/2-1] + m) / 2
		}
		medians[name] = m
	}
	var found []violation
	for _, r := range records {
		issue := violation{Directory: runDirectory(root, r), Scenario: r.Scenario, Seed: r.Seed, Check: "wall_time"}
		if r.WallS > 10*medians[r.Scenario] {
			issue.Reason = fmt.Sprintf("wall time %.3fs exceeds 10x scenario median %.3fs", r.WallS, medians[r.Scenario])
			found = append(found, issue)
		}
		if r.Status != "concluded" && r.Status != "deadend" {
			issue.Check = "termination"
			issue.Reason = "bench run ended with status " + r.Status
			found = append(found, issue)
		}
	}
	return found
}
