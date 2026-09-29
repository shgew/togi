package main

import (
	"flag"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

var update = flag.Bool("update", false, "rewrite testdata/*.golden from the current output")

var renderedSequence = regexp.MustCompile(`(#|seq )\d+`)

func golden(t *testing.T, name, got string) {
	t.Helper()
	path := filepath.Join("testdata", name+".golden")
	if *update {
		if err := os.WriteFile(path, []byte(got), 0o644); err != nil {
			t.Fatal(err)
		}
		return
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	expected := string(want)
	if name == "status" || name == "cert" || strings.HasPrefix(name, "status-") || strings.HasPrefix(name, "cert-") || strings.HasPrefix(name, "watch-") {
		expected = renderedSequence.ReplaceAllString(expected, "${1}<seq>")
		got = renderedSequence.ReplaceAllString(got, "${1}<seq>")
	}
	if diff := cmp.Diff(expected, got); diff != "" {
		t.Errorf("%s mismatch (-want +got):\n%s\nrun `go test ./cmd/togi -update` to accept the new output", path, diff)
	}
}
