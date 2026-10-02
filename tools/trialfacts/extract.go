// Package trialfacts owns the privacy-safe evidence shared by evaluation tools.
package trialfacts

import (
	"compress/gzip"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"slices"

	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type Record struct {
	Session   string               `json:"session"`
	Build     journal.Build        `json:"build"`
	Ruleset   int                  `json:"ruleset"`
	Context   *machine.BIOSContext `json:"context"`
	Seq       int                  `json:"seq"`
	Trial     string               `json:"trial,omitempty"`
	Kind      facts.Kind           `json:"kind"`
	Class     facts.Class          `json:"class"`
	Condition machine.Condition    `json:"condition"`
	Phase     journal.Phase        `json:"phase"`
	Profile   []int                `json:"profile"`
	Outcome   journal.Outcome      `json:"outcome"`
	Signal    machine.Signal       `json:"signal,omitempty"`
	DurationS int                  `json:"duration_s"`
}

func Extract(dir string, dst io.Writer) (int, error) {
	sessions, err := facts.ReadDir(dir)
	if err != nil {
		return 0, err
	}
	gz := gzip.NewWriter(dst)
	encoder := json.NewEncoder(gz)
	count := 0
	for _, session := range sessions {
		for _, f := range session.Facts {
			class := f.Class
			class.Cores = slices.Clone(class.Cores)
			slices.Sort(class.Cores)
			r := Record{Session: f.Session, Build: f.Build, Ruleset: f.Ruleset, Context: session.Context, Seq: f.Seq, Trial: f.Trial, Kind: f.Kind, Class: class, Condition: f.Condition, Phase: f.Phase, Profile: f.Profile, Outcome: f.Outcome, Signal: f.Signal, DurationS: f.DurationS}
			if err := encoder.Encode(r); err != nil {
				return count, fmt.Errorf("encode fact: %w", err)
			}
			count++
		}
	}
	if err := gz.Close(); err != nil {
		return count, fmt.Errorf("close extract: %w", err)
	}
	return count, nil
}

func Read(path string) ([]Record, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, fmt.Errorf("open extract: %w", err)
	}
	defer f.Close()
	gz, err := gzip.NewReader(f)
	if err != nil {
		return nil, fmt.Errorf("open compressed extract: %w", err)
	}
	defer gz.Close()
	decoder := json.NewDecoder(gz)
	var records []Record
	for {
		var r Record
		if err := decoder.Decode(&r); errors.Is(err, io.EOF) {
			break
		} else if err != nil {
			return nil, fmt.Errorf("decode extract: %w", err)
		}
		records = append(records, r)
	}
	return records, nil
}
