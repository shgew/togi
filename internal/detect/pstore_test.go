package detect

import (
	"errors"
	"io/fs"
	"strings"
	"testing"
	"testing/fstest"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestSavedPstore(t *testing.T) {
	t.Parallel()
	const boots = `[{"boot_id":"old","first_entry":100000000},{"boot_id":"skipped","first_entry":200000000},{"boot_id":"current","first_entry":300000000}]`
	for _, tc := range []struct {
		name    string
		archive fstest.MapFS
		boot    string
		want    *machine.PstoreRecord
	}{
		{"matching EFI record", fstest.MapFS{"150/001/dmesg.txt": {Data: []byte("Panic#1 Part1\nfirst\nlast\x1b[2J\n")}}, "old", &machine.PstoreRecord{Path: pstoreDir + "/150/001/dmesg.txt", Lines: []string{"Panic#1 Part1", "first", "last\x1b[2J"}}},
		{"absent archive", nil, "old", nil},
		{"older record", fstest.MapFS{"99/001/dmesg.txt": {Data: []byte("old")}}, "old", nil},
		{"following boot record", fstest.MapFS{"250/001/dmesg.txt": {Data: []byte("later")}}, "old", nil},
		{"successor boundary excluded", fstest.MapFS{"200/001/dmesg.txt": {Data: []byte("later")}}, "old", nil},
		{"first boundary included", fstest.MapFS{"100/001/dmesg.txt": {Data: []byte("Panic#1 Part1\nfirst")}}, "old", &machine.PstoreRecord{Path: pstoreDir + "/100/001/dmesg.txt", Lines: []string{"Panic#1 Part1", "first"}}},
		{"unknown boot", fstest.MapFS{"150/001/dmesg.txt": {Data: []byte("data")}}, "missing", nil},
		{"no successor", fstest.MapFS{"350/001/dmesg.txt": {Data: []byte("data")}}, "current", nil},
		{"latest dump and count", fstest.MapFS{"150/001/dmesg.txt": {Data: []byte("Oops#1 Part1\nolder")}, "160/001/dmesg.txt": {Data: []byte("Oops#1 Part2\nolder count")}, "160/002/dmesg.txt": {Data: []byte("Panic#2 Part1\nlatest")}}, "old", &machine.PstoreRecord{Path: pstoreDir + "/160/002/dmesg.txt", Lines: []string{"Panic#2 Part1", "latest"}}},
		{"dump split across seconds retains newest messages", fstest.MapFS{"150/001/dmesg.txt": {Data: []byte("Oops#1 Part1\nold dump")}, "160/002/dmesg.txt": {Data: []byte("Panic#2 Part19\nnewest messages")}, "161/002/dmesg.txt": {Data: []byte("Panic#2 Part20\nolder messages from later chunk")}}, "old", &machine.PstoreRecord{Path: pstoreDir + "/160/002/dmesg.txt", Lines: []string{"Panic#2 Part19", "newest messages"}}},
		{"count suffix wraps", fstest.MapFS{"150/999/dmesg.txt": {Data: []byte("Oops#999 Part1\nold dump")}, "160/000/dmesg.txt": {Data: []byte("Panic#1000 Part1\nlatest")}}, "old", &machine.PstoreRecord{Path: pstoreDir + "/160/000/dmesg.txt", Lines: []string{"Panic#1000 Part1", "latest"}}},
		{"repeated suffix retains full count", fstest.MapFS{"150/001/dmesg.txt": {Data: []byte("Oops#1 Part1\nold dump")}, "160/001/dmesg.txt": {Data: []byte("Panic#1001 Part1\nlatest")}}, "old", &machine.PstoreRecord{Path: pstoreDir + "/160/001/dmesg.txt", Lines: []string{"Panic#1001 Part1", "latest"}}},
		{"unknown backend", fstest.MapFS{"150/dmesg.txt": {Data: []byte("ERST")}, "dmesg-ramoops-0": {Data: []byte("ramoops")}}, "old", nil},
	} {
		t.Run(tc.name, func(t *testing.T) {
			k := NewKernel(nil)
			k.pstore = tc.archive
			k.journalctl = func([]string) ([]byte, []byte, int, error) {
				return []byte(boots), nil, 0, nil
			}
			got, err := k.SavedPstore(tc.boot)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestPstoreTailBounds(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name, data string
		want       []string
	}{
		{"line count", strings.Repeat("older\n", 40) + "final\n", append(makeLines("older", 31), "final")},
		{"byte count discards partial first line", strings.Repeat("x", 5000) + "\nlast\n", []string{"last"}},
		{"long final line", strings.Repeat("x", 5000), []string{strings.Repeat("x", 4096)}},
		{"UTF8 boundary", strings.Repeat("界", 2000), []string{strings.Repeat("界", 1365)}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			file, err := (fstest.MapFS{"dmesg": {Data: []byte(tc.data)}}).Open("dmesg")
			if err != nil {
				t.Fatal(err)
			}
			got, err := pstoreTail(file, pstoreTailBytes)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func makeLines(line string, count int) []string {
	lines := make([]string, count)
	for i := range lines {
		lines[i] = line
	}
	return lines
}

type unreadablePstore struct{ fs.FS }

func (unreadablePstore) Open(string) (fs.File, error) { return nil, fs.ErrPermission }

func TestSavedPstoreErrors(t *testing.T) {
	t.Parallel()
	k := NewKernel(nil)
	k.pstore = unreadablePstore{}
	if _, err := k.SavedPstore("old"); !errors.Is(err, fs.ErrPermission) {
		t.Fatalf("archive read: %v", err)
	}
	k.pstore = fstest.MapFS{"150/001/dmesg.txt": {Data: []byte("last")}}
	k.journalctl = func([]string) ([]byte, []byte, int, error) { return nil, nil, 0, fs.ErrPermission }
	if _, err := k.SavedPstore("old"); !errors.Is(err, fs.ErrPermission) {
		t.Fatalf("boot read: %v", err)
	}
	k.journalctl = func([]string) ([]byte, []byte, int, error) { return []byte("bad JSON"), nil, 0, nil }
	if _, err := k.SavedPstore("old"); err == nil {
		t.Fatal("malformed boot span accepted")
	}
	k.journalctl = func([]string) ([]byte, []byte, int, error) { return nil, []byte("unreadable"), 2, nil }
	if _, err := k.SavedPstore("old"); err == nil || !strings.Contains(err.Error(), "exit status 2") {
		t.Fatalf("boot exit: %v", err)
	}
}

func TestPstoreHeaderIdentityAndBudget(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		data string
		fail bool
	}{
		{"invalid header", "not a pstore dump\n", true},
		{"incomplete header", "Panic#1 Part1", true},
		{"invalid count", "Panic#x Part1\n", true},
		{"invalid part", "Panic#1 Part0\n", true},
		{"bounded long record", "Panic#1 Part1\n" + strings.Repeat("x", 5000), false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			k := NewKernel(nil)
			k.pstore = fstest.MapFS{"150/001/dmesg.txt": {Data: []byte(tc.data)}}
			k.journalctl = func([]string) ([]byte, []byte, int, error) {
				return []byte(`[{"boot_id":"old","first_entry":100000000},{"boot_id":"next","first_entry":200000000}]`), nil, 0, nil
			}
			got, err := k.SavedPstore("old")
			if tc.fail {
				if err == nil || !strings.Contains(err.Error(), "invalid dump identity") {
					t.Fatalf("invalid header: %v", err)
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff([]string{strings.Repeat("x", pstoreTailBytes-pstoreHeaderBytes)}, got.Lines); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}
