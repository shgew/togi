package journal

import (
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
)

type futurePayload struct{}

func (futurePayload) Kind() Kind      { return "future.fact" }
func (futurePayload) Message() string { return "future fact" }

func TestAppendRefusesUnregisteredKind(t *testing.T) {
	dir := t.TempDir()
	j, err := Open(dir, Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	start := &SessionStart{}
	start.Schema = Schema
	if _, err := j.Append(start); err != nil {
		t.Fatal(err)
	}
	before, err := os.ReadFile(filepath.Join(dir, eventsFile))
	if err != nil {
		t.Fatal(err)
	}
	if _, err := j.Append(futurePayload{}); err == nil {
		t.Fatal("unregistered append accepted")
	}
	after, err := os.ReadFile(filepath.Join(dir, eventsFile))
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(before, after); diff != "" {
		t.Fatalf("journal changed (-want +got): %s", diff)
	}
	if diff := cmp.Diff(1, len(j.Events())); diff != "" {
		t.Fatal(diff)
	}
}

func TestOpaqueEventRefusesWriterBeforeTornTailRepair(t *testing.T) {
	dir := t.TempDir()
	data := []byte(`{"seq":1,"kind":"session.start","schema":2,"ruleset":4,"version":"0.6.0","rev":"future"}` + "\n" + `{"seq":2,"kind":"future.fact","msg":"future fact","nested":{"value":42}}` + "\n" + `{"seq":3`)
	path := filepath.Join(dir, eventsFile)
	if err := os.WriteFile(path, data, 0o644); err != nil {
		t.Fatal(err)
	}
	events, torn, err := Read(dir)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]byte(`{"seq":3`), torn); diff != "" {
		t.Fatal(diff)
	}
	if events[1].Data != nil {
		t.Fatalf("opaque event decoded as %T", events[1].Data)
	}
	if diff := cmp.Diff(`{"seq":2,"kind":"future.fact","msg":"future fact","nested":{"value":42}}`, string(events[1].Raw)); diff != "" {
		t.Fatal(diff)
	}
	_, err = Open(dir, Options{Build: Build{Schema: Schema, Ruleset: 4}})
	if _, ok := errors.AsType[*UnknownKindError](err); !ok {
		t.Fatalf("writer error: %v", err)
	}
	after, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(data, after); diff != "" {
		t.Fatalf("writer changed journal: %s", diff)
	}
}
