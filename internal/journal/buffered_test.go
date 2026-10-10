package journal

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestBufferedJournalHoldsAppendsUntilReadOrClose(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	j, err := Open(dir, Options{Boot: "boot", Now: fixedClock(), Buffered: true})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { j.Close() })
	appendAll(t, j, []Payload{sessionStart(), &SessionNotice{}})
	onDisk := func() []byte {
		t.Helper()
		data, err := os.ReadFile(filepath.Join(dir, eventsFile))
		if err != nil {
			t.Fatal(err)
		}
		return data
	}
	if got := onDisk(); len(got) != 0 {
		t.Fatalf("a buffered journal wrote %d bytes before it was closed or read", len(got))
	}
	events, _, err := j.Read()
	if err != nil {
		t.Fatal(err)
	}
	if len(events) != 2 || len(onDisk()) == 0 {
		t.Fatalf("reading the journal back returned %d events and left %d bytes on disk, want 2 and every line", len(events), len(onDisk()))
	}
	appendAll(t, j, []Payload{&SessionNotice{}})
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	unbuffered := t.TempDir()
	u, err := Open(unbuffered, Options{Boot: "boot", Now: fixedClock()})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { u.Close() })
	appendAll(t, u, []Payload{sessionStart(), &SessionNotice{}, &SessionNotice{}})
	if err := u.Close(); err != nil {
		t.Fatal(err)
	}
	want, err := os.ReadFile(filepath.Join(unbuffered, eventsFile))
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(string(want), string(onDisk())); diff != "" {
		t.Fatalf("buffered journal file (-unbuffered +buffered):\n%s", diff)
	}
}

func TestBufferedJournalCannotSync(t *testing.T) {
	t.Parallel()
	if _, err := Open(t.TempDir(), Options{Boot: "boot", Buffered: true, Sync: true}); err == nil {
		t.Fatal("opened a journal that is both buffered and synced")
	}
}
