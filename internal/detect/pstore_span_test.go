package detect

import (
	"errors"
	"io/fs"
	"testing"
	"testing/fstest"

	"github.com/google/go-cmp/cmp"
)

func TestPstoreRequiresKnownBootSpan(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct{ name, boots string }{
		{name: "missing timestamps", boots: `[{"boot_id":"old"},{"boot_id":"later"}]`},
		{name: "clock moved backwards", boots: `[{"boot_id":"old","first_entry":300000000},{"boot_id":"later","first_entry":100000000}]`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			k := NewKernel(nil)
			k.pstore = fstest.MapFS{"150/001/dmesg.txt": {Data: []byte("unrelated dump")}}
			k.journalctl = func([]string) ([]byte, []byte, int, error) { return []byte(tc.boots), nil, 0, nil }
			got, err := k.SavedPstore("old")
			if err != nil {
				t.Fatal(err)
			}
			if got != nil {
				t.Fatalf("unknown span recorded dump: %+v", got)
			}
		})
	}
}

type pstoreOpenError struct{ fstest.MapFS }

func (pstoreOpenError) Open(string) (fs.File, error) { return nil, fs.ErrPermission }

func TestMatchingPstoreReadError(t *testing.T) {
	t.Parallel()
	k := NewKernel(nil)
	k.pstore = pstoreOpenError{fstest.MapFS{"150/001/dmesg.txt": {Data: []byte("last")}}}
	k.journalctl = func([]string) ([]byte, []byte, int, error) {
		return []byte(`[{"boot_id":"old","first_entry":100000000},{"boot_id":"next","first_entry":200000000}]`), nil, 0, nil
	}
	if _, err := k.SavedPstore("old"); !errors.Is(err, fs.ErrPermission) {
		t.Fatalf("matching record read: %v", err)
	}
}

func TestPstoreSurvivesLostJournalTail(t *testing.T) {
	t.Parallel()
	k := NewKernel(nil)
	k.pstore = fstest.MapFS{"150/001/dmesg.txt": {Data: []byte("kernel page dump\n")}}
	k.journalctl = func(args []string) ([]byte, []byte, int, error) {
		if args[0] != "--list-boots" {
			return nil, nil, 0, errors.New("journal tail is corrupt")
		}
		return []byte(`[{"boot_id":"old","first_entry":100000000},{"boot_id":"next","first_entry":200000000}]`), nil, 0, nil
	}
	got, err := k.SavedPstore("old")
	if err != nil {
		t.Fatal(err)
	}
	if got == nil {
		t.Fatal("lost journal tail discarded pstore diagnostic")
	}
	if diff := cmp.Diff([]string{"kernel page dump"}, got.Lines); diff != "" {
		t.Fatal(diff)
	}
}
