package ycruncher

import (
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"code.marleb.org/shgew/shycler/internal/backend"
	"code.marleb.org/shgew/shycler/internal/machine"
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
			if !reflect.DeepEqual(launch, want) {
				t.Fatalf("launch = %#v, want %#v", launch, want)
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
	if _, err := y.Prepare(machine.Workload{ID: "unsupported", Base: "unsupported"}, t.TempDir(), []int{2}); err == nil || err.Error() != "y-cruncher: no configuration for workload unsupported" {
		t.Fatalf("unsupported workload: %v", err)
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
		line   string
		kind   backend.Kind
		detail string
		cpu    int
	}{
		{"\x1b[31mFailed to set core affinity to core:  3\x1b[0m", backend.AffinityError, "Failed to set core affinity to core:  3", 3},
		{"Error(s) encountered on logical core 3.", backend.ComputationError, "Error(s) encountered on logical core 3.", 0},
		{"Coefficient is too large", backend.ComputationError, "Coefficient is too large", 0},
		{"Checksum mismatch", backend.ComputationError, "Checksum mismatch", 0},
		{"Running BKT: FAIL", backend.ComputationError, "Running BKT: FAIL", 0},
		{"InvalidParametersException", backend.SetupError, "InvalidParametersException", 0},
		{"Invalid Parameter: NOPE", backend.SetupError, "Invalid Parameter: NOPE", 0},
		{"\x1b[32mRunning BKT: Passed\x1b[0m", backend.Progress, "BKT passed", 0},
	} {
		got := y.Classify(tc.line)
		if got.Kind != tc.kind || got.Detail != tc.detail || got.CPU != tc.cpu {
			t.Errorf("Classify(%q) = %#v", tc.line, got)
		}
	}
	var details []string
	for _, line := range strings.Split("Running BKT: Passed\rRunning SFTv4: Passed", "\r") {
		result := y.Classify(line)
		if result.Kind != backend.Progress {
			t.Fatalf("carriage-return progress %q: %#v", line, result)
		}
		details = append(details, result.Detail)
	}
	if !reflect.DeepEqual(details, []string{"BKT passed", "SFTv4 passed"}) {
		t.Errorf("carriage-return progress = %q", details)
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
