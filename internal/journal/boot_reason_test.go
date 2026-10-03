package journal

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestBootReasonRecordedAcrossArchive(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	j := openTest(t, dir)
	appendAll(t, j, []Payload{sessionStart(), &BootLeaveReason{ReasonID: "pending-id", RestartLimitCount: 3, Reason: "restart limit exhausted"}})
	if found, err := j.BootReasonRecorded("pending-id"); err != nil || !found {
		t.Fatalf("current acknowledgement: %v, %v", found, err)
	}
	if _, err := j.Archive("20261002T011407Z"); err != nil {
		t.Fatal(err)
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	resumed := openTest(t, dir)
	if found, err := resumed.BootReasonRecorded("pending-id"); err != nil || !found {
		t.Fatalf("archived acknowledgement lost: %v, %v", found, err)
	}
	if found, err := resumed.BootReasonRecorded("different-id"); err != nil || found {
		t.Fatalf("different record acknowledged: %v, %v", found, err)
	}
}

func TestArchivedBootReasonReadsOnlyCompleteAcknowledgements(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name  string
		data  string
		found bool
		fail  bool
	}{
		{name: "complete", data: `{"kind":"boot.leave_reason","reason_id":"id"}` + "\n", found: true},
		{name: "torn last line", data: `{"kind":"boot.leave_reason","reason_id":"id"}`},
		{name: "message is not acknowledgement", data: `{"kind":"session.warning","msg":"boot.leave_reason","reason_id":"id"}` + "\n"},
		{name: "older and unknown payloads", data: `{"kind":"old.kind","removed_field":"x"}` + "\n" + `{"kind":"boot.leave_reason","reason_id":"id"}` + "\n", found: true},
		{name: "large event", data: `{"kind":"boot.leave_reason","reason_id":"id","reason":"` + strings.Repeat("x", 10000) + `"}` + "\n", found: true},
		{name: "invalid acknowledgement", data: `{"kind":"boot.leave_reason","reason_id":` + "\n", fail: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			path := filepath.Join(t.TempDir(), "archive.jsonl")
			if err := os.WriteFile(path, []byte(tc.data), 0600); err != nil {
				t.Fatal(err)
			}
			found, err := archivedBootReason(path, "id")
			if found != tc.found || (err != nil) != tc.fail {
				t.Fatalf("acknowledgement: found=%v err=%v", found, err)
			}
		})
	}
}

func TestBootReasonLookupReportsUnreadableArchive(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	j := openTest(t, dir)
	if err := os.WriteFile(filepath.Join(dir, archiveDir), []byte("not a directory"), 0600); err != nil {
		t.Fatal(err)
	}
	if found, err := j.BootReasonRecorded("id"); found || err == nil {
		t.Fatalf("unreadable archive silently discarded acknowledgement: found=%v err=%v", found, err)
	}
}
