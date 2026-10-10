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
			// The patch left the range, but its effects may live on: a commit moved into the new base is inherited, not removed. The delta holds the reversal of only what neither the new base nor the new head keeps.
			s.Status = "removed"
			_, files, err := patch(e.Old, e.Old+"^")
			if err != nil {
				return deltaResult{}, err
			}
			kept, err := goneEffects(git, e.Old, newBase, newHead, files)
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

// goneEffects returns the sections of a removed patch's reversal that undo effects the new range really lost, with no changed new-side lines. oldPatch is the removed commit, files the sections of its reversal: their removed lines are what the patch added, their added lines what it removed. Each hunk, and each file's mode or binary change, is traced separately against newBase, which may have inherited the effect, and against newHead, which may keep it through other commits; only what neither has is gone, so one lost hunk never reverses another a lower layer already carries.
func goneEffects(git gitFunc, oldPatch, newBase, newHead string, files []fileDiff) ([]fileDiff, error) {
	var kept []fileDiff
	for _, f := range files {
		name := f.Path
		if f.OldPath != "" {
			name = f.OldPath // the reversal's source is the file as the patch left it
		}
		var base, head []fileDiff
		for _, side := range []struct {
			target string
			since  *[]fileDiff
		}{{newBase, &base}, {newHead, &head}} {
			out, err := git("diff", "--no-color", "--no-ext-diff", "-U0", oldPatch, side.target, "--", ":(literal)"+name)
			if err != nil {
				return nil, fmt.Errorf("diff %s %s -- %s: %w", oldPatch, side.target, name, err)
			}
			if *side.since, err = parseDiff(string(out)); err != nil {
				return nil, fmt.Errorf("diff %s %s -- %s: %w", oldPatch, side.target, name, err)
			}
		}
		if lost, ok := lostEffects(f, base, head); ok {
			kept = append(kept, lost)
		}
	}
	return kept, nil
}

// lostEffects narrows f, the reversal of a removed patch's changes to one file, to the parts that neither the new base (since: the changes from the patch to it) nor the new head keep. It reports false when nothing is lost. Anything it cannot tell apart counts as lost, so an effect is never dropped from the review wrongly.
func lostEffects(f fileDiff, base, head []fileDiff) (fileDiff, bool) {
	f.Changed = nil
	if f.Binary || len(f.Hunks) == 0 {
		return f, effectLost(f, base) && effectLost(f, head)
	}
	var lost []hunk
	for _, h := range f.Hunks {
		if touches(h, base) && touches(h, head) {
			lost = append(lost, h)
		}
	}
	modeLost := f.Mode != "" && changesMode(base) && changesMode(head)
	if len(lost) == 0 && !modeLost {
		return f, false
	}
	f.Text, f.Hunks, f.Added, f.Removed, f.Removals = f.Header, lost, 0, 0, nil
	for _, h := range lost {
		f.Text += h.Text
		f.Removals = append(f.Removals, h.Removals...)
		for line := range strings.Lines(h.Text) {
			switch {
			case strings.HasPrefix(line, "+"):
				f.Added++
			case strings.HasPrefix(line, "-"):
				f.Removed++
			}
		}
	}
	return f, true
}

// effectLost reports whether the changes since the patch (its file to a later commit) undo a binary or header-only change of f.
func effectLost(f fileDiff, since []fileDiff) bool {
	for _, s := range since {
		if f.Binary || s.Binary || f.Mode == "" || s.Mode != "" {
			return true
		}
		// A header-only change of mode, and the file's mode is the same.
	}
	return false
}

func changesMode(since []fileDiff) bool {
	return slices.ContainsFunc(since, func(s fileDiff) bool { return s.Mode != "" })
}

// touches reports whether the changes since the patch undo the hunk's reversal: they remove lines the patch added, or bring back lines it removed.
func touches(h hunk, since []fileDiff) bool {
	for _, s := range since {
		if s.Binary {
			return true
		}
		for _, r := range s.Removals {
			if slices.ContainsFunc(h.Removals, func(a span) bool { return r.First <= a.Last && a.First <= r.Last }) {
				return true
			}
		}
		for _, l := range s.changedLines() {
			if t, ok := strings.CutPrefix(l, "+"); ok && slices.Contains(h.Added, t) {
				return true
			}
		}
	}
	return false
}
