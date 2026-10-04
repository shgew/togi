package main

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"github.com/shgew/togi/internal/journal"
)

func installJournalFixture(t *testing.T, dir string) []byte {
	t.Helper()
	fixture := currentJournalFixture(t, "testdata/events.jsonl")
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), fixture, 0o644); err != nil {
		t.Fatalf("install journal fixture: %v", err)
	}
	return fixture
}

// currentJournalFixture translates a private copy; committed journals stay historical.
func currentJournalFixture(t *testing.T, path string) []byte {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read journal fixture: %v", err)
	}
	dir := t.TempDir()
	file := filepath.Join(dir, "events.jsonl")
	if err := os.WriteFile(file, data, 0o644); err != nil {
		t.Fatal(err)
	}
	return currentJournalCopy(t, dir)
}

func currentJournalCopy(t *testing.T, dir string) []byte {
	t.Helper()
	build, _, err := journal.Scan(dir)
	if err != nil {
		t.Fatal(err)
	}
	events, torn, err := journal.ReadReplay(dir, build.Ruleset)
	if err != nil {
		t.Fatal(err)
	}
	if len(torn) != 0 {
		t.Fatalf("fixture has %d torn bytes", len(torn))
	}
	var data []byte
	for _, event := range events {
		if event.Data == nil {
			data = append(data, event.Raw...)
			data = append(data, '\n')
			continue
		}
		payload, err := json.Marshal(event.Data)
		if err != nil {
			t.Fatal(err)
		}
		var body, raw map[string]json.RawMessage
		if err := json.Unmarshal(payload, &body); err != nil {
			t.Fatal(err)
		}
		if err := json.Unmarshal(event.Raw, &raw); err != nil {
			t.Fatal(err)
		}
		for _, key := range []string{"seq", "time", "mono_ms", "boot", "msg", "cause"} {
			if value, ok := raw[key]; ok {
				body[key] = value
			}
		}
		body["kind"], err = json.Marshal(event.Kind)
		if err != nil {
			t.Fatal(err)
		}
		if _, ok := body["schema"]; ok {
			body["schema"], err = json.Marshal(journal.Schema)
			if err != nil {
				t.Fatal(err)
			}
		}
		line, err := json.Marshal(body)
		if err != nil {
			t.Fatal(err)
		}
		data = append(data, line...)
		data = append(data, '\n')
	}
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), data, 0o644); err != nil {
		t.Fatal(err)
	}
	return data
}
