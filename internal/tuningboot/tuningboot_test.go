package tuningboot

import (
	"errors"
	"maps"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

type memoryBootloader struct {
	values     map[string]string
	sets       []map[string]string
	operations []string
	getErr     error
	setErr     error
	unsetErr   error
	clearErr   error
	cleared    int
}

func (b *memoryBootloader) Get(name string) (string, error) {
	b.operations = append(b.operations, "get "+name)
	return b.values[name], b.getErr
}

func (b *memoryBootloader) Set(values map[string]string) error {
	b.operations = append(b.operations, "set")
	b.sets = append(b.sets, values)
	if b.setErr != nil {
		return b.setErr
	}
	if b.values == nil {
		b.values = make(map[string]string)
	}
	maps.Copy(b.values, values)
	return nil
}

func (b *memoryBootloader) Unset(names ...string) error {
	b.operations = append(b.operations, "unset "+strings.Join(names, " "))
	if b.unsetErr != nil {
		return b.unsetErr
	}
	for _, name := range names {
		delete(b.values, name)
	}
	return nil
}

func (b *memoryBootloader) ClearSavedEntry() (string, string, error) {
	b.operations = append(b.operations, "clear saved_entry")
	b.cleared++
	if b.clearErr != nil {
		return "togi", "togi", b.clearErr
	}
	return "togi", "", nil
}

func TestRestartLimitTwoRetriesThenNormalBoot(t *testing.T) {
	bootloader := &memoryBootloader{}
	var previousID string
	for attempt := 1; attempt <= 4; attempt++ {
		bootloader.operations = nil
		count, retry, err := RestartLimit(bootloader)
		wantCount := min(attempt, 3)
		if err != nil || count != wantCount || retry != (attempt < 3) {
			t.Fatalf("attempt %d: count=%d retry=%v err=%v", attempt, count, retry, err)
		}
		if len(bootloader.sets) != attempt || len(bootloader.sets[attempt-1]) != 2 {
			t.Fatalf("count and reason were not written together: %+v", bootloader.sets)
		}
		wantOperations := []string{"get " + CountVariable, "set"}
		wantReason := retryReason
		if attempt >= 3 {
			wantOperations = append(wantOperations, "clear saved_entry")
			wantReason = exhaustedReason
		}
		if diff := cmp.Diff(wantOperations, bootloader.operations); diff != "" {
			t.Fatalf("attempt order (-want +got): %s", diff)
		}
		reason, err := ReadReason(bootloader)
		if err != nil || reason == nil || reason.ID == previousID || reason.Count != wantCount || reason.Reason != wantReason {
			t.Fatalf("attempt %d reason: %+v, %v", attempt, reason, err)
		}
		previousID = reason.ID
	}
	if bootloader.cleared != 2 {
		t.Fatalf("exhaustion clear calls %d", bootloader.cleared)
	}
}

func TestRestartLimitRejectsInvalidCountWithoutRetry(t *testing.T) {
	for _, value := range []string{"-1", "4", "01", "+1", "1\n", " 1", "garbage", strings.Repeat("9", 301)} {
		t.Run(value, func(t *testing.T) {
			bootloader := &memoryBootloader{values: map[string]string{CountVariable: value}}
			_, retry, err := RestartLimit(bootloader)
			if err == nil || retry || len(bootloader.sets) != 0 || bootloader.cleared != 0 {
				t.Fatalf("invalid count granted boot action: retry=%v err=%v %+v", retry, err, bootloader)
			}
		})
	}
}

func TestRestartLimitFailuresDoNotGrantRetry(t *testing.T) {
	failure := errors.New("GRUB failure")
	for _, operation := range []string{"get", "set", "clear"} {
		t.Run(operation, func(t *testing.T) {
			bootloader := &memoryBootloader{values: map[string]string{CountVariable: "2"}}
			switch operation {
			case "get":
				bootloader.getErr = failure
			case "set":
				bootloader.setErr = failure
			case "clear":
				bootloader.clearErr = failure
			}
			_, retry, err := RestartLimit(bootloader)
			if !errors.Is(err, failure) || retry {
				t.Fatalf("failure hidden: retry=%v error=%v", retry, err)
			}
			if operation != "clear" && bootloader.cleared != 0 {
				t.Fatal("saved entry cleared before state persisted")
			}
		})
	}
}

func TestReasonRoundTripAndIndependentCleanup(t *testing.T) {
	bootloader := &memoryBootloader{values: map[string]string{CountVariable: "2", "saved_entry": "togi"}}
	if reason, err := ReadReason(bootloader); err != nil || reason != nil {
		t.Fatalf("absent reason: %+v, %v", reason, err)
	}
	want, err := NewReason("journal incompatible", 0)
	if err != nil {
		t.Fatal(err)
	}
	if err := WriteReason(bootloader, want); err != nil {
		t.Fatal(err)
	}
	got, err := ReadReason(bootloader)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(&want, got); diff != "" {
		t.Fatalf("reason round trip (-want +got): %s", diff)
	}
	if err := ResetCount(bootloader); err != nil {
		t.Fatal(err)
	}
	if bootloader.values[CountVariable] != "" || bootloader.values[ReasonVariable] == "" {
		t.Fatalf("count reset cleared reason: %+v", bootloader.values)
	}
	if err := ClearReason(bootloader); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(map[string]string{"saved_entry": "togi"}, bootloader.values); diff != "" {
		t.Fatalf("cleanup changed saved entry (-want +got): %s", diff)
	}
}

func TestReasonSanitizesAndBoundsWrites(t *testing.T) {
	for _, text := range []string{"first\nsecond\rthird\ttab\x00 ☃", strings.Repeat("x", 200), strings.Repeat(`"\<&>`, 100)} {
		bootloader := &memoryBootloader{}
		record := Reason{ID: strings.Repeat("a", 64), Count: 3, Reason: text}
		if err := WriteReason(bootloader, record); err != nil {
			t.Fatal(err)
		}
		encoded := bootloader.values[ReasonVariable]
		if len(encoded) > maxRecordBytes || !printableASCII(encoded) {
			t.Fatalf("unsafe environment record (%d bytes): %q", len(encoded), encoded)
		}
		got, err := ReadReason(bootloader)
		if err != nil || got == nil || len(got.Reason) > 160 || !printableASCII(got.Reason) {
			t.Fatalf("unsafe reason: %+v, %v", got, err)
		}
		if got.ID != record.ID || got.Count != record.Count {
			t.Fatalf("metadata changed: %+v", got)
		}
	}
}

func TestReadReasonRejectsInvalidRecords(t *testing.T) {
	for _, value := range []string{
		"not JSON", "null", "{}", `{"id":"test","reason":"retry"}`, `{"id":"test","count":0}`,
		`{"id":"","count":0,"reason":"retry"}`, `{"id":"bad id","count":0,"reason":"retry"}`,
		`{"id":"test","count":-1,"reason":"retry"}`, `{"id":"test","count":4,"reason":"retry"}`,
		`{"id":"test","count":1.5,"reason":"retry"}`, `{"id":"test","count":0,"reason":""}`,
		`{"id":"test","count":0,"reason":"retry\nline"}`, `{"id":"test","count":0,"reason":"\u2603"}`,
		`{"id":"test","count":0,"reason":"retry","extra":true}`, `{"id":"test","count":0,"reason":"retry"} {}`,
		`{"id":"` + strings.Repeat("a", 65) + `","count":0,"reason":"retry"}`,
		`{"id":"test","count":0,"reason":"` + strings.Repeat("a", 161) + `"}`,
		strings.Repeat(" ", maxRecordBytes+1), "{\n}",
	} {
		t.Run(value, func(t *testing.T) {
			bootloader := &memoryBootloader{values: map[string]string{ReasonVariable: value}}
			if reason, err := ReadReason(bootloader); err == nil || reason != nil {
				t.Fatalf("accepted invalid record: %+v, %v", reason, err)
			}
		})
	}
}

func TestWriteReasonRejectsInvalidMetadata(t *testing.T) {
	for _, reason := range []Reason{
		{ID: "", Reason: "retry"}, {ID: "bad\nID", Reason: "retry"}, {ID: strings.Repeat("a", 65), Reason: "retry"},
		{ID: "test", Count: -1, Reason: "retry"}, {ID: "test", Count: 4, Reason: "retry"}, {ID: "test", Reason: " \n\t "},
	} {
		bootloader := &memoryBootloader{}
		if err := WriteReason(bootloader, reason); err == nil || len(bootloader.sets) != 0 {
			t.Fatalf("invalid reason written: %+v, %v", reason, err)
		}
	}
	for _, count := range []int{-1, 4} {
		if _, err := NewReason("retry", count); err == nil {
			t.Fatalf("NewReason accepted count %d", count)
		}
	}
}

func TestReasonOperationFailuresPreserveState(t *testing.T) {
	failure := errors.New("GRUB failure")
	bootloader := &memoryBootloader{values: map[string]string{CountVariable: "2", ReasonVariable: "pending"}, getErr: failure, setErr: failure, unsetErr: failure}
	if _, err := ReadReason(bootloader); !errors.Is(err, failure) {
		t.Fatalf("read error: %v", err)
	}
	if err := WriteReason(bootloader, Reason{ID: "test", Count: 2, Reason: "retry"}); !errors.Is(err, failure) {
		t.Fatalf("write error: %v", err)
	}
	if err := ClearReason(bootloader); !errors.Is(err, failure) {
		t.Fatalf("clear error: %v", err)
	}
	if err := ResetCount(bootloader); !errors.Is(err, failure) {
		t.Fatalf("reset error: %v", err)
	}
	if diff := cmp.Diff(map[string]string{CountVariable: "2", ReasonVariable: "pending"}, bootloader.values); diff != "" {
		t.Fatalf("failed operations lost pending state (-want +got): %s", diff)
	}
}
