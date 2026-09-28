package ycruncher

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"

	"github.com/shgew/togi/internal/backend"
	"github.com/shgew/togi/internal/machine"
)

type Ycruncher struct {
	pkg string
}

func New(pkg string) *Ycruncher { return &Ycruncher{pkg: pkg} }

func (y *Ycruncher) Name() string { return "y-cruncher" }

func (y *Ycruncher) binaries() (lowest, zen5 string, err error) {
	root := filepath.Join(y.pkg, "lib/y-cruncher/Binaries")
	entries, err := os.ReadDir(root)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return "", "", fmt.Errorf("read y-cruncher binaries %s: %w: %w", root, machine.ErrBackendMissing, err)
		}
		return "", "", fmt.Errorf("read y-cruncher binaries %s: %w", root, err)
	}
	var names []string
	for _, entry := range entries {
		info, err := entry.Info()
		if err != nil {
			return "", "", fmt.Errorf("stat y-cruncher binary %s: %w", filepath.Join(root, entry.Name()), err)
		}
		if info.Mode().IsRegular() {
			names = append(names, entry.Name())
		}
	}
	sort.Strings(names)
	if len(names) == 0 {
		return "", "", fmt.Errorf("no y-cruncher binaries in %s: %w", root, machine.ErrBackendMissing)
	}
	lowest = names[0]
	for _, name := range names {
		if strings.HasPrefix(name, "24-ZN5") {
			zen5 = name
			break
		}
	}
	if zen5 == "" {
		return "", "", fmt.Errorf("no Zen 5 binary 24-ZN5 in %s: %w", root, machine.ErrBackendMissing)
	}
	return lowest, zen5, nil
}

func (y *Ycruncher) Check() (string, error) {
	lowest, zen5, err := y.binaries()
	if err != nil {
		return "", err
	}
	return fmt.Sprintf("%s (%s, %s)", y.pkg, lowest, zen5), nil
}

func (y *Ycruncher) Prepare(w machine.Workload, dir string, cpus []int) (backend.Launch, error) {
	var tests []string
	var useZen5 bool
	switch w.Base {
	case "ycruncher-bkt-sftv4":
		tests = []string{"BKT", "SFTv4"}
	case "ycruncher-snt-svt":
		tests = []string{"SNT", "SVT"}
	case "ycruncher-fftv4-n63-vt3":
		tests = []string{"FFTv4", "N63", "VT3"}
		useZen5 = true
	default:
		return backend.Launch{}, fmt.Errorf("y-cruncher: no configuration for workload %s", w.ID)
	}
	lowest, zen5, err := y.binaries()
	if err != nil {
		return backend.Launch{}, err
	}
	binary := lowest
	if useZen5 {
		binary = zen5
	}
	var cfg strings.Builder
	cfg.WriteString("{\n    Action : \"StressTest\"\n    StressTest : {\n        AllocateLocally : \"true\"\n        LogicalCores : [")
	for i, cpu := range cpus {
		if i != 0 {
			cfg.WriteByte(' ')
		}
		cfg.WriteString(strconv.Itoa(cpu))
	}
	fmt.Fprintf(&cfg, "]\n        TotalMemory : %d\n        SecondsPerTest : 30\n        SecondsTotal : 0\n        StopOnError : \"true\"\n        Tests : [\n", 33554432*len(cpus))
	for _, test := range tests {
		fmt.Fprintf(&cfg, "            %q\n", test)
	}
	cfg.WriteString("        ]\n    }\n}\n")
	file := filepath.Join(dir, "stress.cfg")
	if err := os.WriteFile(file, []byte(cfg.String()), 0644); err != nil {
		return backend.Launch{}, fmt.Errorf("write %s: %w", file, err)
	}
	return backend.Launch{Argv: []string{filepath.Join(y.pkg, "lib/y-cruncher/Binaries", binary), "skip-warnings", "pause:-2", "status:none", "config", file}, Files: []string{"stress.cfg"}}, nil
}

var ansi = regexp.MustCompile(`\x1b\[[0-9;]*m`)
var affinity = regexp.MustCompile(`(?i)Failed to set core affinity to core: *(\d+)`)
var computation = regexp.MustCompile(`(?i)Error\(s\) encountered|Coefficient is too large|Checksum mismatch|Running\s+\S+:\s*FAIL`)
var setup = regexp.MustCompile(`(?i)InvalidParametersException|Invalid Parameter`)
var progress = regexp.MustCompile(`(?i)Running\s+(\S+):\s*Passed`)

func (y *Ycruncher) Classify(line string) backend.Line {
	line = ansi.ReplaceAllString(line, "")
	if match := affinity.FindStringSubmatch(line); match != nil {
		cpu, _ := strconv.Atoi(match[1])
		return backend.Line{Kind: backend.AffinityError, Detail: line, CPU: cpu}
	}
	if computation.MatchString(line) {
		return backend.Line{Kind: backend.ComputationError, Detail: line}
	}
	if setup.MatchString(line) {
		return backend.Line{Kind: backend.SetupError, Detail: line}
	}
	if match := progress.FindStringSubmatch(line); match != nil {
		return backend.Line{Kind: backend.Progress, Detail: match[1] + " passed"}
	}
	return backend.Line{Kind: backend.Other}
}
