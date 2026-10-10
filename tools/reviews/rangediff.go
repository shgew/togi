package main

import (
	"fmt"
	"regexp"
	"slices"
	"strconv"
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

// lostEffects narrows f, the reversal of a removed patch's changes to one file, to the changes that neither the new base nor the new head keeps (base and head are the changes from the patch to each). Each changed line is traced on its own, so adjacent lines of one hunk that a lower layer carries stay out of the reversal, which is rendered again as coherent hunks; a mode change that is kept leaves the header too. It reports false when nothing is lost. Anything it cannot tell apart counts as lost, so an effect is never dropped from the review wrongly.
func lostEffects(f fileDiff, base, head []fileDiff) (fileDiff, bool) {
	f.Changed = nil
	if f.Binary || len(f.Hunks) == 0 {
		return f, effectLost(f, base) && effectLost(f, head)
	}
	lost := func(sign byte, old int, text string) bool {
		return touches(sign, old, text, base) && touches(sign, old, text, head)
	}
	var hunks []hunk
	var body strings.Builder
	offset, adds, dels, retained := 0, 0, 0, false
	for _, h := range f.Hunks {
		n, a, d, r := narrowHunk(h, lost, offset)
		retained = retained || r
		if n.Text == "" {
			continue
		}
		hunks = append(hunks, n)
		body.WriteString(n.Text)
		adds, dels = adds+a, dels+d
		offset += a - d
	}
	modeLost := f.Mode != "" && changesMode(base) && changesMode(head)
	if len(hunks) == 0 && !modeLost {
		return f, false
	}
	header := narrowHeader(f, modeLost, retained, len(hunks) > 0)
	f.Text, f.Header, f.Hunks, f.Added, f.Removed, f.Removals = header+body.String(), header, hunks, adds, dels, nil
	for _, h := range hunks {
		f.Removals = append(f.Removals, h.Removals...)
	}
	var modes []string
	for line := range strings.Lines(header) {
		line = strings.TrimSuffix(line, "\n")
		if strings.HasPrefix(line, "old mode ") || strings.HasPrefix(line, "new mode ") || strings.HasPrefix(line, "new file mode") || strings.HasPrefix(line, "deleted file mode") {
			modes = append(modes, line)
		}
	}
	f.Mode = strings.Join(modes, "\n")
	if retained && (f.Status == "added" || f.Status == "deleted") {
		f.Status = "modified"
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

// touches reports whether the changes since the patch undo one line of the reversal: the line the reversal removes (old-side line number old), which they removed too, or the line the reversal adds (text), which they brought back.
func touches(sign byte, old int, text string, since []fileDiff) bool {
	for _, s := range since {
		if s.Binary {
			return true
		}
		if sign == '-' {
			if slices.ContainsFunc(s.Removals, func(r span) bool { return r.First <= old && old <= r.Last }) {
				return true
			}
			continue
		}
		for _, l := range s.changedLines() {
			if t, ok := strings.CutPrefix(l, "+"); ok && t == text {
				return true
			}
		}
	}
	return false
}

var fullHunkHeader = regexp.MustCompile(`^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(.*)$`)

// narrowHunk keeps the changes of one reversal hunk that lost reports lost, as one coherent hunk: a reversed removal that is kept stays as context, a reversed addition that is kept is dropped, and the line counts and the new-side start, which offset shifts by the hunks before it, are rewritten. A hunk with no lost change comes back with empty text. retained says some change was left out; a hunk kept whole comes back verbatim when its numbering is unchanged.
func narrowHunk(h hunk, lost func(sign byte, old int, text string) bool, offset int) (n hunk, adds, dels int, retained bool) {
	lines := slices.Collect(strings.Lines(h.Text))
	m := fullHunkHeader.FindStringSubmatch(strings.TrimSuffix(lines[0], "\n"))
	if m == nil {
		return h, 0, 0, false
	}
	oldStart, _ := strconv.Atoi(m[1])
	newStart, _ := strconv.Atoi(m[3])
	oldNo := oldStart
	var body strings.Builder
	var removed []int
	var added []string
	oldCount, newCount := 0, 0
	lastKept := false
	for _, line := range lines[1:] {
		text := strings.TrimSuffix(line, "\n")
		switch {
		case strings.HasPrefix(line, " "):
			body.WriteString(line)
			oldCount, newCount, oldNo, lastKept = oldCount+1, newCount+1, oldNo+1, true
		case strings.HasPrefix(line, "-"):
			if lost('-', oldNo, text[1:]) {
				body.WriteString(line)
				removed = append(removed, oldNo)
				dels++
				oldCount++
			} else {
				body.WriteString(" " + line[1:])
				oldCount, newCount, retained = oldCount+1, newCount+1, true
			}
			oldNo++
			lastKept = true
		case strings.HasPrefix(line, "+"):
			if lost('+', 0, text[1:]) {
				body.WriteString(line)
				added = append(added, text[1:])
				adds++
				newCount++
				lastKept = true
			} else {
				retained, lastKept = true, false
			}
		case strings.HasPrefix(line, `\`):
			if lastKept {
				body.WriteString(line)
			}
		}
	}
	if adds+dels == 0 {
		return hunk{}, 0, 0, retained
	}
	first := oldStart
	if oldCount == 0 {
		first++
	}
	first += offset
	if newCount == 0 {
		first--
	}
	if !retained && first == newStart {
		return h, adds, dels, false
	}
	head := fmt.Sprintf("@@ -%d,%d +%d,%d @@%s\n", oldStart, oldCount, first, newCount, m[5])
	return hunk{Text: head + body.String(), Removals: spans(removed), Added: added}, adds, dels, retained
}

// narrowHeader keeps the parts of a reversal's header that its narrowed text still carries: mode lines only when the mode change is lost; and, when some change of the text is left out, no file-creation or deletion lines, since the file stays. Without hunks it drops the file name lines.
func narrowHeader(f fileDiff, modeLost, partial, hunks bool) string {
	var b strings.Builder
	for line := range strings.Lines(f.Header) {
		t := strings.TrimSuffix(line, "\n")
		switch {
		case !hunks && (strings.HasPrefix(t, "--- ") || strings.HasPrefix(t, "+++ ")):
			continue
		case strings.HasPrefix(t, "old mode "), strings.HasPrefix(t, "new mode "):
			if !modeLost {
				continue
			}
		case strings.HasPrefix(t, "new file mode"), strings.HasPrefix(t, "deleted file mode"), strings.HasPrefix(t, "index "):
			if partial {
				continue
			}
		case t == "--- /dev/null" && partial:
			line = "--- a/" + f.Path + "\n"
		case t == "+++ /dev/null" && partial:
			line = "+++ b/" + f.Path + "\n"
		}
		b.WriteString(line)
	}
	return b.String()
}
