package main

import (
	"fmt"
	"regexp"
	"slices"
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
			// The patch left the range, but its effect may live on: a commit moved into the new base is inherited, not removed. Only the files whose effect newHead no longer has are the delta's reversal.
			s.Status = "removed"
			_, files, err := patch(e.Old, e.Old+"^")
			if err != nil {
				return deltaResult{}, err
			}
			kept, err := goneEffects(git, e.Old, newHead, files)
			if err != nil {
				return deltaResult{}, err
			}
			if len(kept) > 0 {
				var text strings.Builder
				for _, f := range kept {
					text.WriteString(f.Text)
				}
				d.Texts, d.Files = append(d.Texts, []byte(text.String())), append(d.Files, kept)
			}
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

// goneEffects returns the sections of a removed patch's reversal whose file newHead no longer has the patch's effect on, with no changed new-side lines. oldPatch is the removed commit, files the sections of its reversal: their removed lines are what the patch added, their added lines what it removed. The patch's effect on a file survives, inherited from the new base or kept by the new range, when comparing the file at the patch with newHead leaves the lines it added untouched and does not bring back the lines it removed.
func goneEffects(git gitFunc, oldPatch, newHead string, files []fileDiff) ([]fileDiff, error) {
	var kept []fileDiff
	for _, f := range files {
		name := f.Path
		if f.OldPath != "" {
			name = f.OldPath // the reversal's source is the file as the patch left it
		}
		out, err := git("diff", "--no-color", "--no-ext-diff", "-U0", oldPatch, newHead, "--", ":(literal)"+name)
		if err != nil {
			return nil, fmt.Errorf("diff %s %s -- %s: %w", oldPatch, newHead, name, err)
		}
		since, err := parseDiff(string(out))
		if err != nil {
			return nil, fmt.Errorf("diff %s %s -- %s: %w", oldPatch, newHead, name, err)
		}
		if effectGone(f, since) {
			f.Changed = nil
			kept = append(kept, f)
		}
	}
	return kept, nil
}

// effectGone reports whether the changes since the removed patch (its file to newHead) undo what the patch did to the file, whose reversal is f. Anything it cannot tell apart counts as gone, so a file is never dropped from the review wrongly.
func effectGone(f fileDiff, since []fileDiff) bool {
	if len(since) == 0 {
		return false
	}
	hunks := f.Added+f.Removed > 0
	var restored []string
	if hunks {
		for _, l := range f.changedLines() {
			if t, ok := strings.CutPrefix(l, "+"); ok {
				restored = append(restored, t)
			}
		}
	}
	for _, s := range since {
		switch {
		case f.Binary || s.Binary || !hunks:
			if !f.Binary && !s.Binary && f.Mode != "" && s.Mode == "" {
				continue // a header-only patch of mode changes: the file's mode is the same
			}
			return true
		case f.Mode != "" && s.Mode != "":
			return true
		}
		for _, r := range s.Removals {
			if slices.ContainsFunc(f.Removals, func(a span) bool { return r.First <= a.Last && a.First <= r.Last }) {
				return true
			}
		}
		for _, l := range s.changedLines() {
			if t, ok := strings.CutPrefix(l, "+"); ok && slices.Contains(restored, t) {
				return true
			}
		}
	}
	return false
}
