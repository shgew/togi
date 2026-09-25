package main

import (
	"bytes"
	"regexp"
	"testing"
)

func TestVersion(t *testing.T) {
	t.Parallel()
	var stdout, stderr bytes.Buffer
	if code := cli([]string{"--version"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("exit %d; stderr %s", code, stderr.String())
	}
	if got := stdout.String(); !regexp.MustCompile(`^shycler \d+\.\d+\.\d+\+[0-9a-z-]+\n$`).MatchString(got) {
		t.Fatalf("stdout %q, want shycler <version>+<rev>", got)
	}
	if stderr.Len() != 0 {
		t.Fatalf("stderr %q", stderr.String())
	}
}
