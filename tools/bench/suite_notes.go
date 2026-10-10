package main

import (
	"fmt"
	"io"
)

// reportSuiteNotes prints the registered gate's notes, one `suite: ...` line each, so a
// run shows which scenarios the suite labels synthetic. It is nil-safe and never scores.
func reportSuiteNotes(w io.Writer, g *gate) {
	if g == nil {
		return
	}
	for _, note := range g.Notes {
		fmt.Fprintf(w, "suite: %s\n", note)
	}
}
