package journal

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	togi "github.com/shgew/togi"
)

// Build identifies the rules and journal format used by a togi binary.
type Build struct {
	Version string `json:"version"`
	Rev     string `json:"rev"`
	Ruleset int    `json:"ruleset"`
	Schema  int    `json:"schema"`
	Fixes   int    `json:"fixes"`
}

func binarySchemaBuild() Build {
	return Build{Version: togi.Version(), Rev: togi.Rev(), Schema: Schema}
}

func (b Build) name() string {
	if b.Version == "" {
		return "a togi build from before version stamps"
	}
	if b.Rev == "" {
		return "togi " + b.Version
	}
	return "togi " + b.Version + "+" + b.Rev
}

// IncompatibleError describes a journal that this binary cannot safely modify.
type IncompatibleError struct {
	Field   string
	Journal Build
	Binary  Build
}

func (e *IncompatibleError) Error() string {
	written := fmt.Sprintf("this journal was written by %s (schema %d, ruleset %d)", e.Journal.name(), e.Journal.Schema, e.Journal.Ruleset)
	advice := "Install the togi build that wrote it to continue this session"
	if e.Journal.Version != "" {
		advice = "Install togi " + e.Journal.Version + " to continue this session"
	}
	var value int
	if e.Field == "schema" {
		value = e.Binary.Schema
	} else {
		value = e.Binary.Ruleset
	}
	return fmt.Sprintf("%s; this build, %s, uses %s %d. %s, or run togi reset --all to archive it and start over; candidate_edges in the configuration can start the new session in confirmation at the edges this one found.", written, e.Binary.name(), e.Field, value, advice)
}

// Compatible checks schema first, then strategy. A missing ruleset stamp means ruleset 1.
func Compatible(recorded, binary Build) error {
	if recorded.Ruleset == 0 {
		recorded.Ruleset = 1
	}
	if recorded.Schema != binary.Schema {
		return &IncompatibleError{Field: "schema", Journal: recorded, Binary: binary}
	}
	if recorded.Ruleset != binary.Ruleset {
		return &IncompatibleError{Field: "ruleset", Journal: recorded, Binary: binary}
	}
	return nil
}

// Scan reads only the build stamps, ignoring all other payloads and unknown event kinds.
// It also returns the session ID needed to archive a journal with an unreadable schema.
func Scan(dir string) (Build, string, error) {
	data, err := os.ReadFile(filepath.Join(dir, eventsFile))
	if err != nil {
		return Build{}, "", fmt.Errorf("read journal stamps: %w", err)
	}
	return scanBuild(data)
}

func scanBuild(data []byte) (Build, string, error) {
	var start struct {
		Kind Kind `json:"kind"`
		Build
		Session string `json:"session"`
	}
	first, rest, ok := bytes.Cut(data, []byte{'\n'})
	if !ok {
		return Build{}, "", nil
	}
	if err := json.Unmarshal(first, &start); err != nil {
		return Build{}, "", fmt.Errorf("read session.start stamp: %w", err)
	}
	if start.Kind != KindSessionStart {
		return Build{}, "", fmt.Errorf("journal line 1: first event is %s, want %s", start.Kind, KindSessionStart)
	}
	build := start.Build
	if build.Ruleset == 0 {
		build.Ruleset = 1
	}
	for len(rest) > 0 {
		next := bytes.IndexByte(rest, '\n')
		if next < 0 {
			break
		}
		line := rest[:next]
		rest = rest[next+1:]
		var stamp struct {
			Kind Kind `json:"kind"`
			Build
		}
		if json.Unmarshal(line, &stamp) != nil || stamp.Kind != KindConfigLoaded || stamp.Version == "" {
			continue
		}
		build.Version, build.Rev, build.Fixes = stamp.Version, stamp.Rev, stamp.Fixes
	}
	return build, start.Session, nil
}

// BuildOf extracts the session's rules and the last stamped binary from decoded events.
func BuildOf(events []Event) Build {
	if len(events) == 0 {
		return Build{}
	}
	start, ok := events[0].Data.(*SessionStart)
	if !ok {
		return Build{}
	}
	build := start.Build
	if build.Ruleset == 0 {
		build.Ruleset = 1
	}
	for _, e := range events[1:] {
		if p, ok := e.Data.(*ConfigLoaded); ok && p.Version != "" {
			build.Version, build.Rev, build.Fixes = p.Version, p.Rev, p.Fixes
		}
	}
	return build
}

// RulesetWarning reports that read-only output uses this build's strategy.
func RulesetWarning(recorded, binary Build) string {
	return "warning: journal written by " + recorded.name() + fmt.Sprintf(" (ruleset %d); rendered with this build's rules (ruleset %d)", recorded.Ruleset, binary.Ruleset)
}
