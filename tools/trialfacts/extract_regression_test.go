package trialfacts

import (
	"bytes"
	"os"
	"path/filepath"
	"testing"
)

func TestExtractRejectsEmptyEvidence(t *testing.T) {
	for _, tc := range []struct {
		name, text string
		journal    bool
	}{
		{"no-journals", "", false},
		{"empty-journal", "", true},
		{"no-facts", `{"seq":1,"kind":"session.start","session":"20260101T000000Z","schema":1,"ruleset":6,"cores":[{"core":0},{"core":1}]}
`, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			if tc.journal {
				if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte(tc.text), 0600); err != nil {
					t.Fatal(err)
				}
			}
			var output bytes.Buffer
			output.WriteString("existing output")
			n, err := Extract(dir, &output)
			if err == nil || n != 0 {
				t.Fatalf("Extract = %d, %v; want no facts and an error", n, err)
			}
			if output.String() != "existing output" {
				t.Fatalf("empty source changed output to %q", output.String())
			}
		})
	}
}
