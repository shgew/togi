package trial

import (
	"compress/gzip"
	"context"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"
	"github.com/shgew/togi/internal/machine"
)

// readTrialFile reads a trial file in whichever form it has.
func readTrialFile(path string) ([]byte, error) {
	f, err := machine.OpenTrialFile(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	return io.ReadAll(f)
}

func writeGzip(t *testing.T, path, content string) {
	t.Helper()
	f, err := os.Create(path)
	if err != nil {
		t.Fatal(err)
	}
	zw := gzip.NewWriter(f)
	if _, err := zw.Write([]byte(content)); err != nil {
		t.Fatal(err)
	}
	if err := zw.Close(); err != nil {
		t.Fatal(err)
	}
	if err := f.Close(); err != nil {
		t.Fatal(err)
	}
}

func entries(t *testing.T, dir string) []string {
	t.Helper()
	list, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	var names []string
	for _, e := range list {
		names = append(names, e.Name())
	}
	return names
}

func TestCompressTrialFile(t *testing.T) {
	t.Parallel()
	const content = "one\ntwo\n"
	for _, tt := range []struct {
		name  string
		setup func(t *testing.T, dir string)
		want  []string
		read  string
	}{
		{"plain becomes compressed", func(t *testing.T, dir string) {
			write(t, filepath.Join(dir, "log"), content)
		}, []string{"log.gz"}, content},
		{"missing file is skipped", func(*testing.T, string) {}, nil, ""},
		{"already compressed is skipped", func(t *testing.T, dir string) {
			writeGzip(t, filepath.Join(dir, "log.gz"), content)
		}, []string{"log.gz"}, content},
		{"crash before rename leaves a stale temporary", func(t *testing.T, dir string) {
			write(t, filepath.Join(dir, "log"), content)
			write(t, filepath.Join(dir, "log.gz.tmp"), "torn")
		}, []string{"log.gz"}, content},
		{"crash after rename leaves both forms", func(t *testing.T, dir string) {
			write(t, filepath.Join(dir, "log"), content)
			writeGzip(t, filepath.Join(dir, "log.gz"), content)
		}, []string{"log.gz"}, content},
		{"torn compressed file beside the plain one is replaced", func(t *testing.T, dir string) {
			write(t, filepath.Join(dir, "log"), content)
			write(t, filepath.Join(dir, "log.gz"), "torn")
		}, []string{"log.gz"}, content},
		{"symlink is not followed", func(t *testing.T, dir string) {
			write(t, filepath.Join(dir, "victim"), "protected\n")
			if err := os.Symlink("victim", filepath.Join(dir, "log")); err != nil {
				t.Fatal(err)
			}
		}, []string{"log", "victim"}, ""},
	} {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			tt.setup(t, dir)
			if err := compressTrialFile(filepath.Join(dir, "log")); err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tt.want, entries(t, dir), cmpopts.EquateEmpty()); diff != "" {
				t.Fatalf("directory (-want +got):\n%s", diff)
			}
			if tt.read != "" {
				got, err := readTrialFile(filepath.Join(dir, "log"))
				if err != nil || string(got) != tt.read {
					t.Fatalf("read after compression = %q, %v", got, err)
				}
			}
		})
	}
}

func write(t *testing.T, path, content string) {
	t.Helper()
	if err := os.WriteFile(path, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
}

func TestTrialEndCompressesSamplesAndBackendLogs(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		o := fakeOptions(t, "work")
		o.SampleInterval = time.Second
		o.CPUFreq = t.TempDir()
		sensorFile(t, o.CPUFreq, "cpu0/cpufreq/scaling_cur_freq", "5000000")
		// Passing trials of earlier work stay untouched: every trial directory is kept.
		for i := range 205 {
			dir := filepath.Join(o.Dir, fmt.Sprintf("%04d", 9000+i))
			if err := os.Mkdir(dir, 0755); err != nil {
				t.Fatal(err)
			}
			write(t, filepath.Join(dir, "passed"), "")
			write(t, filepath.Join(dir, "samples.jsonl.gz"), "old")
		}
		before := entries(t, o.Dir)
		r := New(o)
		r.host = &fakeHost{}
		spec := testSpec("0001", machine.R7, 2500*time.Millisecond)
		spec.Cores, spec.CPUs = []int{0, 1}, []int{0, 1}
		run, err := r.Start(context.Background(), spec)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := run.Wait(context.Background(), &recorder{}); err != nil {
			t.Fatal(err)
		}
		root := filepath.Join(o.Dir, spec.ID)
		var plain, compressed []string
		err = filepath.WalkDir(root, func(path string, d os.DirEntry, err error) error {
			if err != nil || d.IsDir() {
				return err
			}
			rel, _ := filepath.Rel(root, path)
			switch {
			case strings.HasSuffix(rel, ".gz"):
				compressed = append(compressed, rel)
			case strings.HasSuffix(rel, ".log") || rel == machine.SamplesFile:
				plain = append(plain, rel)
			}
			return nil
		})
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff([]string(nil), plain); diff != "" {
			t.Errorf("plain files left after the trial ended (-want +got):\n%s", diff)
		}
		wantCompressed := []string{"c00/stderr.log.gz", "c00/stdout.log.gz", "c01/stderr.log.gz", "c01/stdout.log.gz", "samples.jsonl.gz"}
		if diff := cmp.Diff(wantCompressed, compressed); diff != "" {
			t.Errorf("compressed files (-want +got):\n%s", diff)
		}
		var got []int64
		for sample := range machine.ReadSamples(root) {
			got = append(got, sample.ElapsedMS)
		}
		if diff := cmp.Diff([]int64{1000, 2000}, got); diff != "" {
			t.Errorf("samples read back through the compressed form (-want +got):\n%s", diff)
		}
		stdout, err := readTrialFile(filepath.Join(root, "c00", "stdout.log"))
		if err != nil || !strings.Contains(string(stdout), "progress") {
			t.Errorf("stdout read through the compressed form = %q, %v", stdout, err)
		}
		after := entries(t, o.Dir)
		if diff := cmp.Diff(append([]string{spec.ID}, before...), after); diff != "" {
			t.Errorf("trial directories (-want +got):\n%s", diff)
		}
	})
}
