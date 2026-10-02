package detect

import (
	"encoding/json"
	"errors"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func cursorEntry(cursor, boot, message string, mono int64) []byte {
	data, _ := json.Marshal(map[string]any{"__CURSOR": cursor, "_BOOT_ID": boot, "MESSAGE": message, "__REALTIME_TIMESTAMP": "2000000", "__MONOTONIC_TIMESTAMP": strconv.FormatInt(mono, 10)})
	return append(data, '\n')
}

func TestCursorReadsRetainEmptyIntervals(t *testing.T) {
	k := NewKernel([]machine.CoreInfo{{Core: 1, CPUs: []int{2}}})
	initial := cursorEntry("first", "abcd", "Linux version 6.16", 0)
	status := cursorEntry("mce", "abcd", "[Hardware Error]: CPU:2 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135", 1000000)
	last := cursorEntry("last", "abcd", "ordinary kernel message", 2000000)
	k.journalctl = func(args []string) ([]byte, []byte, int, error) {
		if strings.HasPrefix(args[len(args)-1], "--cursor=") {
			if args[len(args)-1] == "--cursor=last" {
				return last, nil, 0, nil
			}
			return append(append(initial, status...), last...), nil, 0, nil
		}
		return initial, nil, 0, nil
	}
	first, err := k.ReadMCEs("ab-cd", "")
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(machine.KernelRead{Cursor: "first"}, first); diff != "" {
		t.Fatal(diff)
	}
	next, err := k.ReadMCEs("ab-cd", first.Cursor)
	if err != nil {
		t.Fatal(err)
	}
	if next.Cursor != "last" || len(next.MCEs) != 1 || next.MCEs[0].Core != 1 || next.MCEs[0].Monotonic != time.Second {
		t.Fatalf("new MCE after empty boundary: %+v", next)
	}
	empty, err := k.ReadMCEs("ab-cd", next.Cursor)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(machine.KernelRead{Cursor: "last"}, empty); diff != "" {
		t.Fatal(diff)
	}
}

func TestCursorRejectsNearestEntryAndOtherBoot(t *testing.T) {
	for _, tc := range []struct {
		name, boot, cursor string
		out                []byte
	}{
		{"vacuumed cursor", "abcd", "gone", cursorEntry("nearest", "abcd", "ordinary", 1)},
		{"wrong boot", "abcd", "saved", cursorEntry("saved", "ef01", "ordinary", 1)},
		{"past tail", "abcd", "saved", nil},
		{"missing metadata", "abcd", "", cursorEntry("", "abcd", "ordinary", 1)},
	} {
		t.Run(tc.name, func(t *testing.T) {
			k := NewKernel(nil)
			k.journalctl = func([]string) ([]byte, []byte, int, error) { return tc.out, nil, 0, nil }
			read, err := k.ReadMCEs(tc.boot, tc.cursor)
			if !errors.Is(err, machine.ErrCursorMissing) {
				t.Fatalf("accepted unavailable interval: %+v %v", read, err)
			}
			if diff := cmp.Diff(machine.KernelRead{}, read); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestCursorReadErrorRetainsMCEEvidencedBeforeMalformedEntry(t *testing.T) {
	k := NewKernel([]machine.CoreInfo{{Core: 1, CPUs: []int{2}}})
	out := cursorEntry("mce", "abcd", "[Hardware Error]: CPU:2 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135", 1000000)
	out = append(out, []byte("{broken\n")...)
	k.journalctl = func([]string) ([]byte, []byte, int, error) { return out, nil, 0, nil }
	read, err := k.ReadMCEs("abcd", "")
	if err == nil {
		t.Fatal("malformed journal entry was accepted")
	}
	if len(read.MCEs) != 1 || !read.MCEs[0].Corrected || read.MCEs[0].Core != 1 {
		t.Fatalf("lost stronger evidence before read error: %+v", read)
	}
}

func TestCursorMetadataLossRetainsOnlyValidatedMCEs(t *testing.T) {
	for _, boot := range []string{"abcd", "other"} {
		t.Run(boot, func(t *testing.T) {
			k := NewKernel([]machine.CoreInfo{{Core: 1, CPUs: []int{2}}})
			message := "[Hardware Error]: CPU:2 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135"
			out := cursorEntry("valid", "abcd", message, 1000000)
			cursor := ""
			if boot != "abcd" {
				cursor = "wrong-boot"
			}
			out = append(out, cursorEntry(cursor, boot, message, 2000000)...)
			k.journalctl = func([]string) ([]byte, []byte, int, error) { return out, nil, 0, nil }
			read, err := k.ReadMCEs("abcd", "")
			if !errors.Is(err, machine.ErrCursorMissing) {
				t.Fatalf("metadata loss: %v", err)
			}
			if read.Cursor != "valid" || len(read.MCEs) != 1 || read.MCEs[0].Monotonic != time.Second || read.MCEs[0].Core != 1 {
				t.Fatalf("lost validated evidence or accepted invalid row: %+v", read)
			}
		})
	}
}

func TestKernelObservationFailuresDoNotBecomeCleanIntervals(t *testing.T) {
	unavailable := errors.New("journal unavailable")
	for _, tc := range []struct {
		name, stderr, detail string
		code                 int
		err, want            error
		cursor               string
	}{
		{name: "command", stderr: "read failed", detail: "read failed", err: unavailable, want: unavailable},
		{name: "missing boot", stderr: "No journal boot entry found", detail: machine.ErrBootMissing.Error(), code: 1, want: machine.ErrBootMissing},
		{name: "vacuumed cursor", stderr: "Failed to seek to cursor", detail: "Failed to seek to cursor", code: 1, want: machine.ErrCursorMissing, cursor: "saved"},
		{name: "other exit", stderr: "permission denied", detail: "permission denied", code: 2},
	} {
		t.Run(tc.name, func(t *testing.T) {
			k := NewKernel(nil)
			k.journalctl = func([]string) ([]byte, []byte, int, error) { return nil, []byte(tc.stderr), tc.code, tc.err }
			read, err := k.ReadMCEs("boot", tc.cursor)
			if err == nil || (tc.want != nil && !errors.Is(err, tc.want)) || !strings.Contains(err.Error(), tc.detail) {
				t.Fatalf("observation failure lost: %v", err)
			}
			if diff := cmp.Diff(machine.KernelRead{}, read); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestMalformedKernelMessagesRetainOnlyEarlierEvidence(t *testing.T) {
	for _, tc := range []struct {
		name       string
		message    json.RawMessage
		wall, mono string
	}{
		{"invalid message", json.RawMessage(`{}`), "2000000", "1000000"},
		{"invalid wall clock", json.RawMessage(`"ordinary"`), "invalid", "1000000"},
		{"invalid monotonic clock", json.RawMessage(`"ordinary"`), "2000000", "invalid"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			k := NewKernel([]machine.CoreInfo{{Core: 1, CPUs: []int{2}}})
			first := cursorEntry("valid", "boot", "[Hardware Error]: CPU:2 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135", 1000000)
			bad, err := json.Marshal(map[string]any{"MESSAGE": tc.message, "__REALTIME_TIMESTAMP": tc.wall, "__MONOTONIC_TIMESTAMP": tc.mono, "__CURSOR": "bad", "_BOOT_ID": "boot"})
			if err != nil {
				t.Fatal(err)
			}
			k.journalctl = func([]string) ([]byte, []byte, int, error) { return append(first, bad...), nil, 0, nil }
			read, err := k.ReadMCEs("boot", "")
			if err == nil || !strings.Contains(err.Error(), "decode kernel message") {
				t.Fatalf("malformed message accepted: %v", err)
			}
			if len(read.MCEs) != 1 || read.MCEs[0].Core != 1 || !read.MCEs[0].Corrected {
				t.Fatalf("earlier evidence lost: %+v", read)
			}
			if got, err := k.MCEs("boot", 0); err == nil || got != nil {
				t.Fatalf("legacy read silently clean: %+v, %v", got, err)
			}
		})
	}
	k := NewKernel(nil)
	k.journalctl = func([]string) ([]byte, []byte, int, error) { return []byte("{broken"), nil, 0, nil }
	if got, err := k.MCEs("boot", 0); err == nil || got != nil {
		t.Fatalf("invalid JSON accepted: %+v, %v", got, err)
	}
}
