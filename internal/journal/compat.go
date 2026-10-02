package journal

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	togi "github.com/shgew/togi"
)

// Build identifies the rules, evidence comparability and journal format used by a togi binary.
type Build struct {
	Version string `json:"version"`
	Rev     string `json:"rev"`
	Ruleset int    `json:"ruleset"`
	Schema  int    `json:"schema"`
	Fixes   int    `json:"fixes"`
	// EvidenceEpoch is compatibility metadata; only session.start.evidence persists it.
	EvidenceEpoch int `json:"-"`
}

// Epoch defaults unstamped ruleset-six and later builds to evidence epoch one.
func (b Build) Epoch() int {
	return evidenceEpoch(b.Ruleset, b.EvidenceEpoch)
}

func evidenceEpoch(ruleset, evidence int) int {
	if evidence != 0 {
		return evidence
	}
	if ruleset >= 6 {
		return 1
	}
	return 0
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

type UnknownKindError struct {
	Kind    Kind
	Journal Build
	Binary  Build
}

func (e *UnknownKindError) Error() string {
	return fmt.Sprintf("unknown kind %q in journal written by %s (schema %d, ruleset %d); this build, %s (schema %d, ruleset %d), cannot safely modify it; install the build that wrote it", e.Kind, e.Journal.name(), e.Journal.Schema, e.Journal.Ruleset, e.Binary.name(), e.Binary.Schema, e.Binary.Ruleset)
}

func KnownKinds(events []Event, binary Build) error {
	for _, e := range events {
		if _, ok := payloadTypes[e.Kind]; !ok {
			if binary.Schema == 0 {
				binary = binarySchemaBuild()
			}
			return &UnknownKindError{Kind: e.Kind, Journal: BuildOf(events), Binary: binary}
		}
	}
	return nil
}

func (e *IncompatibleError) Error() string {
	written := fmt.Sprintf("this journal was written by %s (schema %d, ruleset %d)", e.Journal.name(), e.Journal.Schema, e.Journal.Ruleset)
	if e.Field == "evidence epoch" {
		written = fmt.Sprintf("this journal was written by %s (schema %d, ruleset %d, evidence epoch %d)", e.Journal.name(), e.Journal.Schema, e.Journal.Ruleset, e.Journal.Epoch())
	}
	advice := "Install the togi build that wrote it to continue this session"
	if e.Journal.Version != "" {
		advice = "Install togi " + e.Journal.Version + " to continue this session"
	}
	var value int
	switch e.Field {
	case "schema":
		value = e.Binary.Schema
	case "evidence epoch":
		value = e.Binary.Epoch()
	default:
		value = e.Binary.Ruleset
	}
	if Older(e.Journal, e.Binary) {
		return fmt.Sprintf("%s; this build, %s, uses %s %d. togi run archives it and starts a new session that carries its edges and failed marks; togi reset --all archives it and starts over.", written, e.Binary.name(), e.Field, value)
	}
	return fmt.Sprintf("%s; this build, %s, uses %s %d. %s, or run togi reset --all to archive it and start over.", written, e.Binary.name(), e.Field, value, advice)
}

// Compatible checks schema, ruleset, then evidence epoch. A missing ruleset stamp means ruleset 1.
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
	if recorded.Epoch() != binary.Epoch() {
		return &IncompatibleError{Field: "evidence epoch", Journal: recorded, Binary: binary}
	}
	return nil
}

// Older reports whether recorded has an earlier schema, ruleset or evidence epoch and no later dimension.
func Older(recorded, binary Build) bool {
	if recorded.Ruleset == 0 {
		recorded.Ruleset = 1
	}
	return recorded.Schema <= binary.Schema && recorded.Ruleset <= binary.Ruleset && recorded.Epoch() <= binary.Epoch() &&
		(recorded.Schema < binary.Schema || recorded.Ruleset < binary.Ruleset || recorded.Epoch() < binary.Epoch())
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
		Session  string `json:"session"`
		Evidence int    `json:"evidence"`
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
	build.EvidenceEpoch = evidenceEpoch(build.Ruleset, start.Evidence)
	for len(rest) > 0 {
		next := bytes.IndexByte(rest, '\n')
		if next < 0 {
			break
		}
		line := rest[:next]
		rest = rest[next+1:]
		if !bytes.Contains(line, []byte(KindConfigLoaded)) && !bytes.ContainsRune(line, '\\') {
			continue
		}
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
	build.EvidenceEpoch = start.Epoch()
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
