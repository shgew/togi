package main

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/shgew/togi/tools/trialfacts"
)

func main() {
	if len(os.Args) != 3 {
		fmt.Fprintln(os.Stderr, "usage: facts STATE-DIR OUTPUT.jsonl.gz")
		os.Exit(2)
	}
	n, err := generate(os.Args[1], os.Args[2])
	if err != nil {
		fmt.Fprintf(os.Stderr, "facts: %v\n", err)
		os.Exit(1)
	}
	fmt.Printf("extracted %d facts\n", n)
}

func generate(dir, path string) (int, error) {
	f, err := os.CreateTemp(filepath.Dir(path), ".facts-*.jsonl.gz")
	if err != nil {
		return 0, fmt.Errorf("create extract: %w", err)
	}
	defer os.Remove(f.Name())
	n, err := trialfacts.Extract(dir, f)
	closeErr := f.Close()
	if err != nil {
		return 0, err
	}
	if closeErr != nil {
		return 0, fmt.Errorf("close extract file: %w", closeErr)
	}
	if err := os.Rename(f.Name(), path); err != nil {
		return 0, fmt.Errorf("replace extract: %w", err)
	}
	return n, nil
}
