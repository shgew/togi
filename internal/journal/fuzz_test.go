package journal

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func FuzzParse(f *testing.F) {
	dir := f.TempDir()
	j := openTest(f, dir)
	appendAll(f, j, samplePayloads())
	j.Close()
	data, err := os.ReadFile(filepath.Join(dir, eventsFile))
	if err != nil {
		f.Fatal(err)
	}
	f.Add(data)
	f.Add(data[:len(data)-5])
	f.Add([]byte{})

	f.Fuzz(func(t *testing.T, data []byte) {
		events, end, err := parse(data, Build{})
		if err != nil {
			return
		}
		if end < 0 || end > len(data) {
			t.Fatalf("end %d outside [0, %d]", end, len(data))
		}
		again, againEnd, err := parse(data[:end], Build{})
		if err != nil {
			t.Fatalf("reparse of the first %d bytes: %v", end, err)
		}
		if againEnd != end {
			t.Fatalf("reparse end %d, want %d", againEnd, end)
		}
		if diff := cmp.Diff(events, again); diff != "" {
			t.Fatalf("reparse mismatch (-first +again):\n%s", diff)
		}
	})
}
