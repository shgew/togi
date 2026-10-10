package main

import (
	"fmt"
	"regexp"
	"strings"
)

// gitFunc runs git with the arguments in the repository being reviewed and returns its standard output.
type gitFunc func(args ...string) ([]byte, error)

// rangeDiffLine is `git range-diff --no-patch` output: old index, old commit, mark, new index, new commit, subject.
var rangeDiffLine = regexp.MustCompile(`^\s*(?:\d+|-+):\s+([0-9a-f]+|-+)\s+([=!<>])\s+(?:\d+|-+):\s+([0-9a-f]+|-+)\s+(.*)$`)

type rangeDiffEntry struct {
	Old, New, Subject string
	Mark              byte
}

func parseRangeDiff(out string) ([]rangeDiffEntry, error) {
	var entries []rangeDiffEntry
	for line := range strings.Lines(out) {
		line = strings.TrimRight(line, "\n")
		if line == "" {
			continue
		}
		m := rangeDiffLine.FindStringSubmatch(line)
		if m == nil {
			return nil, fmt.Errorf("unrecognized range-diff line %q", line)
		}
		e := rangeDiffEntry{Old: m[1], Mark: m[2][0], New: m[3], Subject: m[4]}
		if strings.HasPrefix(e.Old, "-") {
			e.Old = ""
		}
		if strings.HasPrefix(e.New, "-") {
			e.New = ""
		}
		entries = append(entries, e)
	}
	return entries, nil
}

// deltaResult is a re-review's patch classification and the patches that make up its delta.
type deltaResult struct {
	Patches []patchStatus
	// Texts are the delta's patches in range-diff order: the new version of each changed patch, each added patch, and the reverse of each removed one.
	Texts [][]byte
	// Files are the sections of those patches; the reversed ones carry no changed new-side lines.
	Files [][]fileDiff
}

func (d deltaResult) carryForward() bool {
	for _, p := range d.Patches {
		if p.Status == "added" || p.Status == "removed" || p.Status == "changed" {
			return false
		}
	}
	return true
}

// classifyRange compares the layer's patches between the previous record's range and the current one. A patch range-diff marks changed whose added and removed lines are the same, so only its context moved, counts as unchanged.
func classifyRange(git gitFunc, oldBase, oldHead, newBase, newHead string) (deltaResult, error) {
	out, err := git("range-diff", "--no-patch", "--no-color", oldBase+".."+oldHead, newBase+".."+newHead)
	if err != nil {
		return deltaResult{}, fmt.Errorf("range-diff of %s..%s and %s..%s: %w", oldBase, oldHead, newBase, newHead, err)
	}
	entries, err := parseRangeDiff(string(out))
	if err != nil {
		return deltaResult{}, err
	}
	patch := func(from, to string) ([]byte, []fileDiff, error) {
		text, err := git("diff", "--no-color", "--no-ext-diff", from, to)
		if err != nil {
			return nil, nil, fmt.Errorf("diff %s %s: %w", from, to, err)
		}
		files, err := parseDiff(string(text))
		if err != nil {
			return nil, nil, fmt.Errorf("diff %s %s: %w", from, to, err)
		}
		return text, files, nil
	}
	var d deltaResult
	for _, e := range entries {
		s := patchStatus{Old: e.Old, New: e.New, Subject: e.Subject}
		switch e.Mark {
		case '=':
			s.Status = "unchanged"
		case '<':
			s.Status = "removed"
			text, files, err := patch(e.Old, e.Old+"^")
			if err != nil {
				return deltaResult{}, err
			}
			for i := range files {
				files[i].Changed = nil
			}
			d.Texts, d.Files = append(d.Texts, text), append(d.Files, files)
		case '>':
			s.Status = "added"
			text, files, err := patch(e.New+"^", e.New)
			if err != nil {
				return deltaResult{}, err
			}
			d.Texts, d.Files = append(d.Texts, text), append(d.Files, files)
		case '!':
			_, oldFiles, err := patch(e.Old+"^", e.Old)
			if err != nil {
				return deltaResult{}, err
			}
			text, newFiles, err := patch(e.New+"^", e.New)
			if err != nil {
				return deltaResult{}, err
			}
			if sameChanges(oldFiles, newFiles) {
				s.Status, s.ContextOnly = "unchanged", true
				break
			}
			s.Status = "changed"
			d.Texts, d.Files = append(d.Texts, text), append(d.Files, newFiles)
		}
		d.Patches = append(d.Patches, s)
	}
	return d, nil
}
