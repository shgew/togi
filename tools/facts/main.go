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
	if err := generate(os.Args[1], os.Args[2]); err != nil {
		fmt.Fprintf(os.Stderr, "facts: %v\n", err)
		os.Exit(1)
	}
}

func generate(dir, path string) error {
	f, err := os.CreateTemp(filepath.Dir(path), ".facts-*.jsonl.gz")
	if err != nil {
		return fmt.Errorf("create extract: %w", err)
	}
	defer os.Remove(f.Name())
	n, err := trialfacts.Extract(dir, f)
	closeErr := f.Close()
	if err != nil {
		return err
	}
	if closeErr != nil {
		return fmt.Errorf("close extract file: %w", closeErr)
	}
	if err := os.Rename(f.Name(), path); err != nil {
		return fmt.Errorf("replace extract: %w", err)
	}
	fmt.Printf("extracted %d facts\n", n)
	return nil
}
