package trialfiles

import (
	"compress/gzip"
	"errors"
	"io"
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

// The directory sync after publishing the gzip file comes before the plain file's removal: when it fails, the plain
// file stays readable and a retry finishes the job.
func TestCompressionSyncFailureAfterPublicationRetainsPlainFile(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	path := filepath.Join(dir, Samples)
	const contents = "{\"elapsed_ms\":1000}\n"
	write(t, path, contents)
	errSync := errors.New("injected directory sync failure")
	syncs := 0
	err := compressFile(path, func(synced string) error {
		syncs++
		if syncs > 1 {
			t.Errorf("directory synced again after a failed sync")
			return nil
		}
		if synced != dir {
			t.Errorf("synced %s, want %s", synced, dir)
		}
		if plain, err := os.ReadFile(path); err != nil || string(plain) != contents {
			t.Errorf("plain file at first sync = %q, %v", plain, err)
		}
		if compressed, err := gunzip(path + ".gz"); err != nil || compressed != contents {
			t.Errorf("gzip file at first sync = %q, %v", compressed, err)
		}
		return errSync
	})
	if !errors.Is(err, errSync) {
		t.Fatalf("compression with failed directory sync = %v, want %v", err, errSync)
	}
	if plain, err := os.ReadFile(path); err != nil || string(plain) != contents {
		t.Fatalf("plain file after failed sync = %q, %v", plain, err)
	}
	if data, err := readTrialFile(path); err != nil || string(data) != contents {
		t.Fatalf("read after failed sync = %q, %v", data, err)
	}
	var states [][]string
	err = compressFile(path, func(synced string) error {
		states = append(states, entries(t, synced))
		return syncDir(synced)
	})
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([][]string{{Samples, Samples + ".gz"}, {Samples + ".gz"}}, states); diff != "" {
		t.Fatalf("directory at each sync (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff([]string{Samples + ".gz"}, entries(t, dir)); diff != "" {
		t.Fatalf("directory after retry (-want +got):\n%s", diff)
	}
	if compressed, err := gunzip(path + ".gz"); err != nil || compressed != contents {
		t.Fatalf("gzip file after retry = %q, %v", compressed, err)
	}
}

func gunzip(path string) (string, error) {
	f, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer f.Close()
	z, err := gzip.NewReader(f)
	if err != nil {
		return "", err
	}
	data, err := io.ReadAll(z)
	if err == nil {
		err = z.Close()
	}
	return string(data), err
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
