package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"slices"
	"strings"
)

const (
	recordOpen  = "<!-- togi-review "
	recordClose = " -->"
)

var (
	priorities = [...]string{"P0", "P1", "P2", "P3"}
	outcomes   = [...]string{"fixed", "rejected", "deferred"}
)

// record is the part of a review record's hidden JSON block this report reads; versions 1, 2 and 3 share it (docs/review-record.md).
type record struct {
	Version  int       `json:"version"`
	Findings []finding `json:"findings"`
}

type finding struct {
	Source   string `json:"source"`
	Priority string `json:"priority"`
	Finding  string `json:"finding"`
	// Location is kept raw because version 1 records it as an object and later versions as a string; null when absent.
	Location json.RawMessage `json:"location"`
	Outcome  outcome         `json:"outcome"`
}

type outcome struct {
	Status string `json:"status"`
	SHA    string `json:"sha,omitempty"`
	Reason string `json:"reason,omitempty"`
	Issue  int    `json:"issue,omitempty"`
}

var errNoRecord = errors.New("no togi-review block")

// parseRecord reads the hidden togi-review block that ends a review record comment.
func parseRecord(body string) (record, error) {
	_, rest, found := strings.CutLast(body, recordOpen)
	if !found {
		return record{}, errNoRecord
	}
	block, _, found := strings.Cut(rest, recordClose)
	if !found {
		return record{}, errors.New("unterminated togi-review block")
	}
	var r record
	if err := json.Unmarshal([]byte(block), &r); err != nil {
		return record{}, fmt.Errorf("decode togi-review block: %w", err)
	}
	if r.Version < 1 || r.Version > 3 {
		return record{}, fmt.Errorf("unsupported record version %d", r.Version)
	}
	for _, f := range r.Findings {
		if !slices.Contains(priorities[:], f.Priority) {
			return record{}, fmt.Errorf("finding from %s: unknown priority %q", f.Source, f.Priority)
		}
		if !slices.Contains(outcomes[:], f.Outcome.Status) {
			return record{}, fmt.Errorf("finding from %s: unknown outcome %q", f.Source, f.Outcome.Status)
		}
	}
	return r, nil
}
