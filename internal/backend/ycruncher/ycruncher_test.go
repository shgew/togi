package ycruncher

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/backend"
	"github.com/shgew/togi/internal/machine"
)

func fakePackage(t *testing.T, zen5 bool) string {
	t.Helper()
	pkg := t.TempDir()
	root := filepath.Join(pkg, "lib/y-cruncher/Binaries")
	if err := os.MkdirAll(root, 0755); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"11-SNB ~ Hina", "05-A64 ~ Kasumi"} {
		if err := os.WriteFile(filepath.Join(root, name), []byte("binary"), 0755); err != nil {
			t.Fatal(err)
		}
	}
	if zen5 {
		if err := os.WriteFile(filepath.Join(root, "24-ZN5 ~ Komari"), []byte("binary"), 0755); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.Mkdir(filepath.Join(root, "Digits"), 0755); err != nil {
		t.Fatal(err)
	}
	return pkg
}

func TestMissingBinaries(t *testing.T) {
	for _, tt := range []struct {
		name, remove string
		zen5         bool
	}{
		{"directory", "lib/y-cruncher/Binaries", true},
		{"zen5", "", false},
	} {
		t.Run(tt.name, func(t *testing.T) {
			pkg := fakePackage(t, tt.zen5)
			if tt.remove != "" {
				if err := os.RemoveAll(filepath.Join(pkg, tt.remove)); err != nil {
					t.Fatal(err)
				}
			}
			_, err := New(pkg).Check()
			if diff := cmp.Diff(true, errors.Is(err, machine.ErrBackendMissing)); diff != "" {
				t.Fatalf("missing backend (-want +got):\n%s", diff)
			}
		})
	}
}

func TestCheckBinaries(t *testing.T) {
	for _, tt := range []struct {
		name, file string
		mode       os.FileMode
		directory  bool
		wantError  bool
	}{
		{name: "regular executables"},
		{name: "non-executable lowest", file: "05-A64 ~ Kasumi", mode: 0644, wantError: true},
		{name: "non-executable Zen 5", file: "24-ZN5 ~ Komari", mode: 0644, wantError: true},
		{name: "directory lowest", file: "05-A64 ~ Kasumi", directory: true, wantError: true},
		{name: "directory Zen 5", file: "24-ZN5 ~ Komari", directory: true, wantError: true},
		{name: "unrelated earlier file", file: "00-README", mode: 0644},
		{name: "unrelated earlier executable", file: "00-README", mode: 0755},
		{name: "misleading Zen 5 prefix", file: "24-ZN5", mode: 0755},
	} {
		t.Run(tt.name, func(t *testing.T) {
			pkg := fakePackage(t, true)
			if tt.file != "" {
				file := filepath.Join(pkg, "lib/y-cruncher/Binaries", tt.file)
				if tt.directory {
					if err := os.Remove(file); err != nil {
						t.Fatal(err)
					}
					if err := os.Mkdir(file, 0755); err != nil {
						t.Fatal(err)
					}
				} else {
					if err := os.WriteFile(file, []byte("binary"), tt.mode); err != nil {
						t.Fatal(err)
					}
					if err := os.Chmod(file, tt.mode); err != nil {
						t.Fatal(err)
					}
				}
			}
			detail, err := New(pkg).Check()
			if tt.wantError {
				if err == nil || !strings.Contains(err.Error(), filepath.Join(pkg, "lib/y-cruncher/Binaries", tt.file)) {
					t.Fatalf("Check must reject and name %q: %v", tt.file, err)
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(pkg+" (05-A64 ~ Kasumi, 24-ZN5 ~ Komari)", detail); diff != "" {
				t.Fatalf("selected binaries (-want +got):\n%s", diff)
			}
		})
	}
}

func TestPrepare(t *testing.T) {
	pkg := fakePackage(t, true)
	y := New(pkg)
	if y.Name() != "y-cruncher" {
		t.Fatalf("Name = %q", y.Name())
	}
	if detail, err := y.Check(); err != nil || detail != pkg+" (05-A64 ~ Kasumi, 24-ZN5 ~ Komari)" {
		t.Fatalf("Check = %q, %v", detail, err)
	}
	for _, tc := range []struct {
		base, binary, tests string
		cpus                []int
		memory              string
	}{
		{"ycruncher-bkt-sftv4", "05-A64 ~ Kasumi", "            \"BKT\"\n            \"SFTv4\"\n", []int{2}, "33554432"},
		{"ycruncher-snt-svt", "05-A64 ~ Kasumi", "            \"SNT\"\n            \"SVT\"\n", []int{2, 18}, "67108864"},
		{"ycruncher-fftv4-n63-vt3", "24-ZN5 ~ Komari", "            \"FFTv4\"\n            \"N63\"\n            \"VT3\"\n", []int{2}, "33554432"},
	} {
		t.Run(tc.base, func(t *testing.T) {
			dir := t.TempDir()
			launch, err := y.Prepare(machine.Workload{ID: tc.base + "-derived", Base: tc.base}, dir, tc.cpus)
			if err != nil {
				t.Fatal(err)
			}
			cfg := filepath.Join(dir, "stress.cfg")
			want := backend.Launch{Argv: []string{filepath.Join(pkg, "lib/y-cruncher/Binaries", tc.binary), "skip-warnings", "pause:-2", "status:none", "config", cfg}, Files: []string{"stress.cfg"}}
			if diff := cmp.Diff(want, launch); diff != "" {
				t.Fatalf("launch mismatch (-want +got):\n%s", diff)
			}
			cpus := "2"
			if len(tc.cpus) > 1 {
				cpus = "2 18"
			}
			wantConfig := "{\n    Action : \"StressTest\"\n    StressTest : {\n        AllocateLocally : \"true\"\n        LogicalCores : [" + cpus + "]\n        TotalMemory : " + tc.memory + "\n        SecondsPerTest : 30\n        SecondsTotal : 0\n        StopOnError : \"true\"\n        Tests : [\n" + tc.tests + "        ]\n    }\n}\n"
			got, err := os.ReadFile(cfg)
			if err != nil {
				t.Fatal(err)
			}
			if string(got) != wantConfig {
				t.Fatalf("stress.cfg = %q, want %q", got, wantConfig)
			}
		})
	}
	_, err := y.Prepare(machine.Workload{ID: "unsupported-derived", Base: "unsupported"}, t.TempDir(), []int{2})
	if err == nil || errors.Is(err, machine.ErrBackendMissing) || !strings.Contains(err.Error(), "unsupported-derived") {
		t.Fatalf("unsupported workload must fail naming it, not as a missing backend: %v", err)
	}
}

func TestPrepareCatalogWorkloads(t *testing.T) {
	pkg := fakePackage(t, true)
	b := New(pkg)
	for _, regime := range machine.Regimes {
		for _, w := range machine.Workloads(regime) {
			if w.Backend != machine.Ycruncher {
				continue
			}
			t.Run(string(regime)+"/"+w.ID, func(t *testing.T) {
				dir := t.TempDir()
				launch, err := b.Prepare(w, dir, []int{2, 18}[:w.Threads])
				if err != nil {
					t.Fatal(err)
				}
				if len(launch.Argv) == 0 || !filepath.IsAbs(launch.Argv[0]) {
					t.Fatalf("launch argv = %q, want an absolute executable path", launch.Argv)
				}
				if rel, err := filepath.Rel(pkg, launch.Argv[0]); err != nil || !filepath.IsLocal(rel) {
					t.Fatalf("executable %s is outside the package tree %s", launch.Argv[0], pkg)
				}
				if info, err := os.Stat(launch.Argv[0]); err != nil || info.Mode()&0111 == 0 {
					t.Fatalf("executable %s: %v, %v", launch.Argv[0], info, err)
				}
				if len(launch.Files) == 0 {
					t.Fatal("launch writes no files")
				}
				for _, name := range launch.Files {
					if !filepath.IsLocal(name) {
						t.Fatalf("file %q is not inside the work directory", name)
					}
					if _, err := os.Stat(filepath.Join(dir, name)); err != nil {
						t.Fatalf("file %q was not written: %v", name, err)
					}
				}
			})
		}
	}
}

func TestMissingZen5(t *testing.T) {
	y := New(fakePackage(t, false))
	if _, err := y.Check(); err == nil || !strings.Contains(err.Error(), "24-ZN5") {
		t.Fatalf("Check without Zen 5 binary: %v", err)
	}
	if _, err := y.Prepare(machine.Workload{Base: "ycruncher-bkt-sftv4"}, t.TempDir(), []int{2}); err == nil || !strings.Contains(err.Error(), "24-ZN5") {
		t.Fatalf("Prepare without Zen 5 binary: %v", err)
	}
}

func TestClassify(t *testing.T) {
	y := New("")
	for _, tc := range []struct {
		line string
		want backend.Line
	}{
		{"\x1b[31mFailed to set core affinity to core:  3\x1b[0m", backend.Line{Kind: backend.AffinityError, CPU: 3}},
		{"Error(s) encountered on logical core 3.", backend.Line{Kind: backend.ComputationError}},
		{"Coefficient is too large", backend.Line{Kind: backend.ComputationError}},
		{"Checksum mismatch", backend.Line{Kind: backend.ComputationError}},
		{"Running BKT: FAIL", backend.Line{Kind: backend.ComputationError}},
		{"InvalidParametersException", backend.Line{Kind: backend.SetupError}},
		{"Invalid Parameter: NOPE", backend.Line{Kind: backend.SetupError}},
		{"\x1b[32mRunning BKT: Passed\x1b[0m", backend.Line{Kind: backend.Progress, Progress: "BKT passed"}},
	} {
		if diff := cmp.Diff(tc.want, y.Classify(tc.line)); diff != "" {
			t.Errorf("Classify(%q) (-want +got):\n%s", tc.line, diff)
		}
	}
	var passed []string
	for line := range strings.SplitSeq("Running BKT: Passed\rRunning SFTv4: Passed", "\r") {
		result := y.Classify(line)
		if result.Kind != backend.Progress {
			t.Fatalf("carriage-return progress %q: %#v", line, result)
		}
		passed = append(passed, result.Progress)
	}
	if diff := cmp.Diff([]string{"BKT passed", "SFTv4 passed"}, passed); diff != "" {
		t.Errorf("carriage-return progress mismatch (-want +got):\n%s", diff)
	}
	for _, file := range []struct {
		name string
		want backend.Kind
	}{
		{"lowest-pass.txt", backend.Progress},
		{"snt-pass.txt", backend.Progress},
		{"zen5-pass.txt", backend.Progress},
		{"setup-nope.txt", backend.SetupError},
		{"affinity-warning.txt", backend.AffinityError},
		{"error-spliced.txt", backend.ComputationError},
	} {
		data, err := os.ReadFile(filepath.Join("testdata", file.name))
		if err != nil {
			t.Fatal(err)
		}
		found := false
		for _, line := range strings.FieldsFunc(string(data), func(r rune) bool { return r == '\n' || r == '\r' }) {
			if y.Classify(line).Kind == file.want {
				found = true
			}
		}
		if !found {
			t.Errorf("%s has no %v lines", file.name, file.want)
		}
	}
}

func TestBinaryDiscoveryErrors(t *testing.T) {
	for _, kind := range []string{"no matching names", "dangling executable", "cyclic executable", "not a directory"} {
		t.Run(kind, func(t *testing.T) {
			pkg := fakePackage(t, true)
			root := filepath.Join(pkg, "lib/y-cruncher/Binaries")
			missing := kind == "no matching names" || kind == "dangling executable"
			switch kind {
			case "no matching names", "not a directory":
				if err := os.RemoveAll(root); err != nil {
					t.Fatal(err)
				}
				if kind == "no matching names" {
					if err := os.Mkdir(root, 0755); err != nil {
						t.Fatal(err)
					}
					if err := os.WriteFile(filepath.Join(root, "README"), nil, 0644); err != nil {
						t.Fatal(err)
					}
				} else if err := os.WriteFile(root, nil, 0644); err != nil {
					t.Fatal(err)
				}
			default:
				bin := filepath.Join(root, "05-A64 ~ Kasumi")
				if err := os.Remove(bin); err != nil {
					t.Fatal(err)
				}
				target := "absent"
				if kind == "cyclic executable" {
					target = filepath.Base(bin)
				}
				if err := os.Symlink(target, bin); err != nil {
					t.Fatal(err)
				}
			}
			_, err := New(pkg).Check()
			if err == nil || errors.Is(err, machine.ErrBackendMissing) != missing || !strings.Contains(err.Error(), root) {
				t.Fatalf("discovery failure misclassified: %v", err)
			}
		})
	}
}

func TestPrepareInputFailureProducesNoLaunch(t *testing.T) {
	pkg, dir := fakePackage(t, true), t.TempDir()
	path := filepath.Join(dir, "stress.cfg")
	if err := os.Mkdir(path, 0755); err != nil {
		t.Fatal(err)
	}
	launch, err := New(pkg).Prepare(machine.Workload{Base: "ycruncher-bkt-sftv4"}, dir, []int{2})
	if err == nil || !strings.Contains(err.Error(), path) {
		t.Fatalf("input failure lost: %v", err)
	}
	if diff := cmp.Diff(backend.Launch{}, launch); diff != "" {
		t.Fatal(diff)
	}
}
