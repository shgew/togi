package trialfacts

import (
	"bytes"
	"compress/gzip"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

type rejectingWriter struct{ err error }

func (w rejectingWriter) Write([]byte) (int, error) { return 0, w.err }

type headerOnlyWriter struct {
	err         error
	wroteHeader bool
}

func (w *headerOnlyWriter) Write(p []byte) (int, error) {
	if !w.wroteHeader {
		w.wroteHeader = true
		return len(p), nil
	}
	return 0, w.err
}

func TestExtractReportsSourceAndOutputFailures(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "events.jsonl")
	if err := os.WriteFile(path, []byte("{invalid}\n"), 0600); err != nil {
		t.Fatal(err)
	}
	var out bytes.Buffer
	if n, err := Extract(dir, &out); err == nil || n != 0 || out.Len() != 0 {
		t.Fatalf("invalid source: count=%d error=%v output=%q", n, err, out.String())
	}
	text := `{"seq":1,"kind":"session.start","session":"20260101T000000Z","schema":1,"ruleset":6,"cores":[{"core":0}]}
{"seq":2,"kind":"trial.intent","trial":"1","core":0,"offset":-10,"regime":"R1","workload":"fixture","duration_s":90,"condition":"isolated","phase":"search"}
{"seq":3,"kind":"trial.end","trial":"1","outcome":"pass","duration_s":90}
`
	if err := os.WriteFile(path, []byte(text), 0600); err != nil {
		t.Fatal(err)
	}
	failure := errors.New("destination rejected write")
	if _, err := Extract(dir, rejectingWriter{failure}); !errors.Is(err, failure) {
		t.Fatalf("output error=%v, want %v", err, failure)
	}
	if n, err := Extract(dir, &headerOnlyWriter{err: failure}); n != 1 || !errors.Is(err, failure) || !strings.Contains(err.Error(), "close extract") {
		t.Fatalf("close error: count=%d error=%v", n, err)
	}
}

func TestReadRejectsInvalidExtracts(t *testing.T) {
	var malformed bytes.Buffer
	gz := gzip.NewWriter(&malformed)
	if _, err := io.WriteString(gz, "{}\n{invalid}\n"); err != nil {
		t.Fatal(err)
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}
	var valid bytes.Buffer
	gz = gzip.NewWriter(&valid)
	if _, err := io.WriteString(gz, "{}\n"); err != nil {
		t.Fatal(err)
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		name      string
		data      []byte
		operation string
	}{
		{"missing", nil, "open extract"},
		{"not-gzip", []byte("not compressed"), "open compressed extract"},
		{"malformed-json", malformed.Bytes(), "decode extract"},
		{"truncated-gzip", valid.Bytes()[:len(valid.Bytes())-8], "decode extract"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "facts.gz")
			if tc.data != nil {
				if err := os.WriteFile(path, tc.data, 0600); err != nil {
					t.Fatal(err)
				}
			}
			records, err := Read(path)
			if err == nil || !strings.Contains(err.Error(), tc.operation) || records != nil {
				t.Fatalf("Read=%+v, %v; want no partial evidence and %s error", records, err, tc.operation)
			}
		})
	}
}
