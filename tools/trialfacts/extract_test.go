package trialfacts

import (
	"bytes"
	"compress/gzip"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestExtractAllSessionsPrivacyAndDeterminism(t *testing.T) {
	dir := t.TempDir()
	if err := os.Mkdir(filepath.Join(dir, "archive"), 0700); err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		path, session string
		ruleset       int
	}{
		{"archive/old.jsonl", "20260101T000000Z", 1},
		{"events.jsonl", "20260102T000000Z", 6},
	} {
		text := `{"seq":1,"kind":"session.start","session":"SESSION","schema":1,"ruleset":RULESET,"version":"fixture","rev":"abc","boot":"private-boot","cores":[{"core":0},{"core":1}]}
{"seq":2,"kind":"trial.intent","trial":"1","core":0,"offset":-10,"regime":"R1","workload":"fixture","duration_s":90,"condition":"isolated","phase":"search","boot":"private-boot"}
{"seq":3,"kind":"trial.end","trial":"1","outcome":"pass","duration_s":90,"boot":"private-boot"}
`
		text = strings.ReplaceAll(text, "SESSION", tc.session)
		if tc.ruleset == 1 {
			text = strings.ReplaceAll(text, "RULESET", "1")
		} else {
			text = strings.ReplaceAll(text, "RULESET", "6")
		}
		if err := os.WriteFile(filepath.Join(dir, tc.path), []byte(text), 0600); err != nil {
			t.Fatal(err)
		}
	}
	var first, second bytes.Buffer
	for _, dst := range []*bytes.Buffer{&first, &second} {
		count, err := Extract(dir, dst)
		if err != nil {
			t.Fatal(err)
		}
		if count != 2 {
			t.Fatalf("facts=%d want 2", count)
		}
	}
	if !bytes.Equal(first.Bytes(), second.Bytes()) {
		t.Fatal("extract bytes differ")
	}
	gz, err := gzip.NewReader(bytes.NewReader(first.Bytes()))
	if err != nil {
		t.Fatal(err)
	}
	plain, err := io.ReadAll(gz)
	if err != nil {
		t.Fatal(err)
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}
	if bytes.Contains(plain, []byte("private-boot")) || bytes.Contains(plain, []byte(dir)) {
		t.Fatal("private journal metadata leaked")
	}
	path := filepath.Join(dir, "facts.jsonl.gz")
	if err := os.WriteFile(path, first.Bytes(), 0600); err != nil {
		t.Fatal(err)
	}
	records, err := Read(path)
	if err != nil {
		t.Fatal(err)
	}
	if len(records) != 2 || records[0].Ruleset != 1 || records[1].Ruleset != 6 || records[0].Profile[0] != -10 {
		t.Fatalf("got %+v", records)
	}
}
