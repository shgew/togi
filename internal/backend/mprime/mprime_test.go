package mprime

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

func fakePackage(t *testing.T) string {
	t.Helper()
	pkg := t.TempDir()
	if err := os.Mkdir(filepath.Join(pkg, "bin"), 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(pkg, "bin/mprime"), []byte("#!/bin/sh\n"), 0755); err != nil {
		t.Fatal(err)
	}
	return pkg
}

func TestMissingBinary(t *testing.T) {
	pkg := fakePackage(t)
	if err := os.Remove(filepath.Join(pkg, "bin/mprime")); err != nil {
		t.Fatal(err)
	}
	_, err := New(pkg).Check()
	if diff := cmp.Diff(true, errors.Is(err, machine.ErrBackendMissing)); diff != "" {
		t.Fatalf("missing backend (-want +got):\n%s", diff)
	}
}

func TestPrepare(t *testing.T) {
	pkg := fakePackage(t)
	m := New(pkg)
	if m.Name() != "mprime" {
		t.Fatalf("Name = %q", m.Name())
	}
	if detail, err := m.Check(); err != nil || detail != filepath.Join(pkg, "bin/mprime") {
		t.Fatalf("Check = %q, %v", detail, err)
	}
	for _, tc := range []struct {
		name, base, fft, flags string
		cpus                   []int
		threads, hyper         int
	}{
		{"sse", "mprime-sse-4k-21k", "4,21", "0,0,0,0", []int{2}, 1, 0},
		{"avx512", "mprime-avx512-36k-248k", "36,248", "1,1,1,1", []int{2, 18}, 2, 1},
		{"avx2", "mprime-avx2-36k-248k", "36,248", "1,1,1,0", []int{2}, 1, 0},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			got, err := m.Prepare(machine.Workload{ID: tc.base + "-smt", Base: tc.base}, dir, tc.cpus)
			if err != nil {
				t.Fatal(err)
			}
			want := backend.Launch{Argv: []string{filepath.Join(pkg, "bin/mprime"), "-t", "-W" + dir}, Files: []string{"prime.txt", "local.txt"}, Watch: []string{"results.txt"}}
			if diff := cmp.Diff(want, got); diff != "" {
				t.Fatalf("launch mismatch (-want +got):\n%s", diff)
			}
			fft := strings.Split(tc.fft, ",")
			flags := strings.Split(tc.flags, ",")
			settings := "NumCPUs=1\nCoresPerTest=1\nMinTortureFFT=" + fft[0] + "\nMaxTortureFFT=" + fft[1] + "\nTortureTime=1\n" +
				"TortureHyperthreading=" + string(rune('0'+tc.hyper)) + "\nTortureThreads=" + string(rune('0'+tc.threads)) + "\n" +
				"CpuSupportsAVX=" + flags[0] + "\nCpuSupportsFMA3=" + flags[1] + "\nCpuSupportsFMA4=0\nCpuSupportsAVX2=" + flags[2] + "\nCpuSupportsAVX512F=" + flags[3] + "\n"
			for _, file := range []struct{ name, want string }{
				{"prime.txt", "V30OptionsConverted=1\nStressTester=1\nUsePrimenet=0\n" + settings + "EnableSetAffinity=0\n"},
				{"local.txt", "ErrorCheck=1\nSumInputsErrorCheck=1\n" + settings},
			} {
				data, err := os.ReadFile(filepath.Join(dir, file.name))
				if err != nil {
					t.Fatal(err)
				}
				if string(data) != file.want {
					t.Errorf("%s = %q, want %q", file.name, data, file.want)
				}
			}
		})
	}
	_, err := m.Prepare(machine.Workload{ID: "unsupported-smt", Base: "unsupported"}, t.TempDir(), []int{2})
	if err == nil || errors.Is(err, machine.ErrBackendMissing) || !strings.Contains(err.Error(), "unsupported-smt") {
		t.Fatalf("unsupported workload must fail naming it, not as a missing backend: %v", err)
	}
}

func TestPrepareCatalogWorkloads(t *testing.T) {
	pkg := fakePackage(t)
	b := New(pkg)
	for _, regime := range machine.Regimes {
		for _, w := range machine.Workloads(regime) {
			if w.Backend != machine.Mprime {
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

func TestCheckNonExecutable(t *testing.T) {
	pkg := fakePackage(t)
	if err := os.Chmod(filepath.Join(pkg, "bin/mprime"), 0644); err != nil {
		t.Fatal(err)
	}
	if _, err := New(pkg).Check(); err == nil {
		t.Fatal("non-executable binary passed Check")
	}
}

func TestClassify(t *testing.T) {
	m := New("")
	for _, line := range []string{
		"FATAL ERROR: Rounding was 0.5, expected less than 0.4", "ERROR: ILLEGAL SUMOUT", "Possible hardware failure", "Hardware failure detected running 288K FFT size, consult stress.txt file.",
		"Maximum number of warnings exceeded", "TORTURE TEST FAILED", "Torture Test completed 5000 iterations - 2 errors", "ERROR: SUM(INPUTS) != SUM(OUTPUTS)",
		"ERROR: Shift counter corrupt", "ERROR: Illegal double encountered", "ERROR: FFT data has been zeroed", "ERROR: Jacobi error check failed", "Warning: ILLEGAL SUMOUT", "Warning: SUMOUT MISMATCH",
	} {
		if got := m.Classify(strings.ToLower(line)); got.Kind != backend.ComputationError {
			t.Errorf("%q classified as %v", line, got)
		}
	}
	for _, line := range []string{"Error allocating memory for FFT data.", "Out of memory!", "Unable to allocate memory.  One possible cause is the operating system's swap area is too small.", "Cannot initialize FFT code, errcode=1002"} {
		if got := m.Classify(line); got.Kind != backend.SetupError {
			t.Errorf("%q classified as %v", line, got)
		}
	}
	if got := m.Classify("Self-test 21K (thread 1 of 2) passed!"); got.Kind != backend.Progress || got.Progress != "self-test 21K passed" {
		t.Errorf("progress: %#v", got)
	}
	if got := m.Classify("Torture Test completed 100 - 0 errors"); got.Kind != backend.Other {
		t.Errorf("zero errors: %#v", got)
	}
	for _, file := range []string{"pass-stdout.txt", "pass-results.txt", "error-spliced.txt"} {
		data, err := os.ReadFile(filepath.Join("testdata", file))
		if err != nil {
			t.Fatal(err)
		}
		found := map[backend.Kind]bool{}
		for _, line := range strings.FieldsFunc(string(data), func(r rune) bool { return r == '\n' || r == '\r' }) {
			found[m.Classify(line).Kind] = true
		}
		if file == "error-spliced.txt" && !found[backend.ComputationError] {
			t.Errorf("%s did not yield computation errors", file)
		}
		if file == "pass-stdout.txt" && !found[backend.Progress] {
			t.Errorf("%s did not yield self-test progress", file)
		}
		if file != "error-spliced.txt" && found[backend.ComputationError] {
			t.Errorf("%s classified as computation error", file)
		}
	}
}

func TestPrepareFailureProducesNoLaunch(t *testing.T) {
	for _, file := range []string{"binary", "prime.txt", "local.txt"} {
		t.Run(file, func(t *testing.T) {
			pkg, dir := fakePackage(t), t.TempDir()
			if file == "binary" {
				if err := os.Remove(filepath.Join(pkg, "bin/mprime")); err != nil {
					t.Fatal(err)
				}
			} else if err := os.Mkdir(filepath.Join(dir, file), 0755); err != nil {
				t.Fatal(err)
			}
			launch, err := New(pkg).Prepare(machine.Workload{Base: "mprime-sse-4k-21k"}, dir, []int{2})
			if err == nil {
				t.Fatal("failed preparation yielded success")
			}
			if file == "binary" {
				if !errors.Is(err, machine.ErrBackendMissing) {
					t.Fatalf("missing binary cause lost: %v", err)
				}
				if _, err := os.Stat(filepath.Join(dir, "prime.txt")); !errors.Is(err, os.ErrNotExist) {
					t.Fatal("prepared inputs for missing binary")
				}
			} else if !strings.Contains(err.Error(), filepath.Join(dir, file)) {
				t.Fatalf("input error did not name file: %v", err)
			}
			if diff := cmp.Diff(backend.Launch{}, launch); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestCheckStatErrorIsNotMissingBackend(t *testing.T) {
	pkg := fakePackage(t)
	bin := filepath.Join(pkg, "bin/mprime")
	if err := os.Remove(bin); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink("mprime", bin); err != nil {
		t.Fatal(err)
	}
	_, err := New(pkg).Check()
	if err == nil || errors.Is(err, machine.ErrBackendMissing) || !strings.Contains(err.Error(), bin) {
		t.Fatalf("stat failure misclassified: %v", err)
	}
}
