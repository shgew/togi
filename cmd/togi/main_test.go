package main

import (
	"bytes"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestTopLevelRefusals(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name      string
		args      []string
		firstLine string
	}{
		{"no-command", nil, "   __              _"},
		{"unknown-command", []string{"unknown"}, `togi: unknown command "unknown"`},
		{"unknown-flag", []string{"--unknown"}, "togi: flag provided but not defined: -unknown"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var out, diagnostics bytes.Buffer
			if code := testCLI(t, tc.args, &out, &diagnostics); code != exitUsage || out.Len() != 0 {
				t.Fatalf("exit %d, stdout %q, stderr %q", code, out.String(), diagnostics.String())
			}
			firstLine, _, _ := strings.Cut(diagnostics.String(), "\n")
			if diff := cmp.Diff(tc.firstLine, firstLine); diff != "" {
				t.Fatalf("first diagnostic line (-want +got): %s", diff)
			}
		})
	}
}
