package trialfiles

import (
	"compress/gzip"
	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"
	"io"
	"os"
	"path/filepath"
	"testing"
)

// readTrialFile reads a trial file in whichever form it has.
func readTrialFile(path string) ([]byte, error) {
	f, err := Open(path)
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
			t.Helper()
			write(t, filepath.Join(dir, "log"), content)
		}, []string{"log.gz"}, content},
		{"missing file is skipped", func(*testing.T, string) {}, nil, ""},
		{"already compressed is skipped", func(t *testing.T, dir string) {
			t.Helper()
			writeGzip(t, filepath.Join(dir, "log.gz"), content)
		}, []string{"log.gz"}, content},
		{"crash before rename leaves a stale temporary", func(t *testing.T, dir string) {
			t.Helper()
			write(t, filepath.Join(dir, "log"), content)
			write(t, filepath.Join(dir, "log.gz.tmp"), "torn")
		}, []string{"log.gz"}, content},
		{"crash after rename leaves both forms", func(t *testing.T, dir string) {
			t.Helper()
			write(t, filepath.Join(dir, "log"), content)
			writeGzip(t, filepath.Join(dir, "log.gz"), content)
		}, []string{"log.gz"}, content},
		{"torn compressed file beside the plain one is replaced", func(t *testing.T, dir string) {
			t.Helper()
			write(t, filepath.Join(dir, "log"), content)
			write(t, filepath.Join(dir, "log.gz"), "torn")
		}, []string{"log.gz"}, content},
		{"symlink is not followed", func(t *testing.T, dir string) {
			t.Helper()
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
			if err := compressFile(filepath.Join(dir, "log")); err != nil {
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
