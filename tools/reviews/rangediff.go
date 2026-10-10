package main

import (
	"errors"
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
			// A commit the new base contains, itself or rewritten, is inherited whatever lower commits did to its lines since.
			ancestor, err := isAncestor(git, e.Old, newBase)
			if err != nil {
				return deltaResult{}, err
			}
			if ancestor {
				break
			}
			_, files, err := patch(e.Old, e.Old+"^")
			if err != nil {
				return deltaResult{}, err
			}
			rewritten, err := rewrittenInto(git, e.Old, newBase, files)
			if err != nil {
				return deltaResult{}, err
			}
			if rewritten {
				break
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
		name := sourceName(f)
		specs := []string{":(literal)" + name}
		if f.Path != name {
			specs = append(specs, ":(literal)"+f.Path) // a rename or copy: the file's other name tells whether it is back
		}
		var base, head []fileDiff
		for _, side := range []struct {
			target string
			since  *[]fileDiff
		}{{newBase, &base}, {newHead, &head}} {
			args := append([]string{"diff", "--no-color", "--no-ext-diff", "--no-renames", "-U0", oldPatch, side.target, "--"}, specs...)
			out, err := git(args...)
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

// lostEffects narrows f, the reversal of a removed patch's changes to one file, to the changes that neither the new base nor the new head keeps (base and head are the changes from the patch to each). Each changed line is traced on its own, so adjacent lines of one hunk that a lower layer carries stay out of the reversal, which is rendered again as coherent hunks. The file operation (creation, deletion, rename, copy) and the mode are traced on their own too: one a lower layer carries leaves the header, and the text then applies to the file as it stands now. It reports false when nothing is lost. Anything it cannot tell apart counts as lost, so an effect is never dropped from the review wrongly.
func lostEffects(f fileDiff, base, head []fileDiff) (fileDiff, bool) {
	f.Changed = nil
	name := sourceName(f)
	opLost := f.Status != "modified" && operationLost(f, base) && operationLost(f, head)
	modeLost := hasModeChange(f) && changesMode(base) && changesMode(head)
	lost := func(sign byte, old int, text string) bool {
		return touches(sign, old, text, name, base) && touches(sign, old, text, name, head)
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
	binaryLost := f.Binary && len(base) > 0 && len(head) > 0
	if len(hunks) == 0 && !modeLost && !opLost && !binaryLost {
		return f, false
	}
	survives := f.Status != "modified" && !opLost
	cur := f.Path
	if f.Status == "renamed" || f.Status == "copied" {
		cur = f.OldPath // the file stays where the patch left it
	}
	header := narrowHeader(f, cur, survives, modeLost, retained, len(hunks) > 0)
	text := header + body.String()
	if survives {
		if f.Binary {
			text += fmt.Sprintf("Binary files a/%s and b/%s differ\n", cur, cur)
		}
		f.Path, f.OldPath, f.Status = cur, "", "modified"
	}
	f.Text, f.Header, f.Hunks, f.Added, f.Removed, f.Removals = text, header, hunks, adds, dels, nil
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
	return f, true
}

// sourceName is the name of the file as the removed patch left it: the source of a reversal that renames or copies, else its path.
func sourceName(f fileDiff) string {
	if f.OldPath != "" {
		return f.OldPath
	}
	return f.Path
}

// operationLost reports whether the changes since the patch undo the file operation of f, the reversal of what the patch did: the patch created the file (f deletes it) and it is gone, the patch deleted it (f creates it) and it is back, or the patch renamed or copied it (f undoes that) and the source is gone or the target is back.
func operationLost(f fileDiff, since []fileDiff) bool {
	has := func(path, status string) bool {
		return slices.ContainsFunc(since, func(s fileDiff) bool { return s.Path == path && s.Status == status })
	}
	switch f.Status {
	case "deleted":
		return has(f.Path, "deleted")
	case "added", "copied":
		return has(f.Path, "added")
	case "renamed":
		return has(f.OldPath, "deleted") || has(f.Path, "added")
	}
	return false
}

func hasModeChange(f fileDiff) bool { return strings.Contains(f.Mode, "old mode") }

func changesMode(since []fileDiff) bool {
	return slices.ContainsFunc(since, hasModeChange)
}

// touches reports whether the changes since the patch undo one line of the reversal: the line the reversal removes (old-side line number old of the file name), which they removed too, or the line the reversal adds (text), which they brought back.
func touches(sign byte, old int, text, name string, since []fileDiff) bool {
	for _, s := range since {
		if s.Binary {
			return true
		}
		if sign == '-' {
			if s.Path == name && slices.ContainsFunc(s.Removals, func(r span) bool { return r.First <= old && old <= r.Last }) {
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

// narrowHeader builds the header of a narrowed reversal. Mode lines stay only when the mode change is lost. When the file operation survives (survives), the old header, which names a creation, deletion, rename or copy, is replaced by a content-only header on cur, the file's current name. Otherwise the old header stays, minus the index line when some change of the text is left out, and minus the file name lines when no hunk is left.
func narrowHeader(f fileDiff, cur string, survives, modeLost, partial, hunks bool) string {
	var b strings.Builder
	if survives {
		fmt.Fprintf(&b, "diff --git a/%s b/%s\n", cur, cur)
	}
	for line := range strings.Lines(f.Header) {
		t := strings.TrimSuffix(line, "\n")
		switch {
		case strings.HasPrefix(t, "old mode "), strings.HasPrefix(t, "new mode "):
			if !modeLost {
				continue
			}
		case survives:
			continue
		case !hunks && (strings.HasPrefix(t, "--- ") || strings.HasPrefix(t, "+++ ")):
			continue
		case strings.HasPrefix(t, "index ") && partial:
			continue
		}
		b.WriteString(line)
	}
	if survives && hunks {
		fmt.Fprintf(&b, "--- a/%s\n+++ b/%s\n", cur, cur)
	}
	return b.String()
}

// isAncestor reports whether the new base contains commit itself. Git's exit status 1 means it does not; any other failure is an error.
func isAncestor(git gitFunc, commit, base string) (bool, error) {
	_, err := git("merge-base", "--is-ancestor", commit, base)
	if err == nil {
		return true, nil
	}
	var exit interface{ ExitCode() int }
	if !errors.As(err, &exit) || exit.ExitCode() != 1 {
		return false, fmt.Errorf("merge-base --is-ancestor %s %s: %w", commit, base, err)
	}
	return false, nil
}

// rewrittenInto reports whether the new base, which does not contain commit itself, contains it rewritten: a commit of the base that changes the files commit touches (those of files, its reversal) exactly as commit does. sameChanges is the proof: the same added and removed lines byte for byte, modes and binary contents, wherever the hunks sit. Patch IDs, as git cherry compares them, are no proof: they ignore whitespace, so "a b" and "ab" would match. When no proof exists the removal is traced by its lines instead.
func rewrittenInto(git gitFunc, commit, base string, files []fileDiff) (bool, error) {
	var specs []string
	for _, f := range files {
		for _, p := range []string{f.Path, f.OldPath} {
			if spec := ":(literal)" + p; p != "" && !slices.Contains(specs, spec) {
				specs = append(specs, spec)
			}
		}
	}
	if len(specs) == 0 {
		return false, nil
	}
	exact := []string{"--no-color", "--no-ext-diff", "--no-renames", "--full-index"}
	// Each commit of the base that touches those files, as its patch limited to them, after a NUL.
	args := append(append([]string{"log", "--no-merges", "--no-show-signature", "-p", "--format=%x00"}, exact...), commit+".."+base, "--")
	out, err := git(append(args, specs...)...)
	if err != nil {
		return false, fmt.Errorf("log %s..%s: %w", commit, base, err)
	}
	candidates := strings.Split(string(out), "\x00")[1:]
	if len(candidates) == 0 {
		return false, nil
	}
	out, err = git(append(append([]string{"diff"}, exact...), commit+"^", commit)...)
	if err != nil {
		return false, fmt.Errorf("diff %s^ %s: %w", commit, commit, err)
	}
	own, err := parseDiff(string(out))
	if err != nil {
		return false, fmt.Errorf("diff %s^ %s: %w", commit, commit, err)
	}
	for _, c := range candidates {
		theirs, err := parseDiff(strings.TrimLeft(c, "\n"))
		if err != nil {
			return false, fmt.Errorf("log %s..%s: %w", commit, base, err)
		}
		if sameChanges(own, theirs) {
			return true, nil
		}
	}
	return false, nil
}
