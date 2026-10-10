package trialfiles

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestCompressionFailureRetainsReadablePlainFile(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	const contents = "{\"elapsed_ms\":1000}\n"
	write(t, filepath.Join(dir, Samples), contents)
	// A directory blocks publication of the gzip file, after all its bytes were written.
	if err := os.Mkdir(filepath.Join(dir, Samples+".gz"), 0755); err != nil {
		t.Fatal(err)
	}
	if err := Compress(dir); err == nil {
		t.Fatal("blocked gzip publication succeeded")
	}
	data, err := readTrialFile(filepath.Join(dir, Samples))
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(contents, string(data)); diff != "" {
		t.Fatal(diff)
	}
	if _, err := os.Stat(filepath.Join(dir, Samples+".gz.tmp")); !os.IsNotExist(err) {
		t.Fatalf("failed compression left a temporary file: %v", err)
	}
	if err := os.Remove(filepath.Join(dir, Samples+".gz")); err != nil {
		t.Fatal(err)
	}
	if err := Compress(dir); err != nil {
		t.Fatal(err)
	}
	data, err = readTrialFile(filepath.Join(dir, Samples))
	if err != nil || string(data) != contents {
		t.Fatalf("retry contents = %q, %v", data, err)
	}
}

func TestCompressionIgnoresSymlinkInstanceDirectories(t *testing.T) {
	t.Parallel()
	dir, outside := t.TempDir(), t.TempDir()
	write(t, filepath.Join(outside, "stdout.log"), "outside\n")
	if err := os.Symlink(outside, filepath.Join(dir, "work")); err != nil {
		t.Fatal(err)
	}
	if err := Compress(dir); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(filepath.Join(outside, "stdout.log"))
	if err != nil || string(data) != "outside\n" {
		t.Fatalf("symlink directory changed external file: %q, %v", data, err)
	}
	if err := Compress(filepath.Join(dir, "missing")); err != nil {
		t.Fatal(err)
	}
}
