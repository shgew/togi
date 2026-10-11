// Command gocompat-vectors writes the test vectors of the Rust togi-gocompat
// crate with the Go toolchain's own output. Run it from the repository root:
//
//	go run ./tools/gocompat-vectors MODE OUTPUT-DIR
//
// Each mode lives in its own file and registers itself in modes.
package main

import (
	"fmt"
	"io"
	"os"
	"slices"
	"sort"
	"strings"
)

type mode func(outDir string) error

var modes = map[string]mode{}

func main() {
	os.Exit(run(os.Args[1:], os.Stderr))
}

func run(args []string, stderr io.Writer) int {
	names := make([]string, 0, len(modes))
	for name := range modes {
		names = append(names, name)
	}
	sort.Strings(names)
	if len(args) != 2 || !slices.Contains(names, args[0]) {
		fmt.Fprintf(stderr, "usage: gocompat-vectors MODE OUTPUT-DIR\nmodes: %s\n", strings.Join(names, ", "))
		return 2
	}
	if err := modes[args[0]](args[1]); err != nil {
		fmt.Fprintf(stderr, "gocompat-vectors: %s: %v\n", args[0], err)
		return 1
	}
	return 0
}
