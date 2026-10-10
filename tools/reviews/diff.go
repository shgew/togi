package main

import (
	"errors"
	"fmt"
	"path"
	"regexp"
	"slices"
	"strconv"
	"strings"
)

// span is an inclusive range of line numbers.
type span struct{ First, Last int }

// hunk is one hunk of a file's section.
type hunk struct {
	// Text is the hunk verbatim, its @@ line included.
	Text string
	// Removals holds the old-side lines the hunk removes, merged into spans.
	Removals []span
	// Added holds the text of the lines the hunk adds.
	Added []string
}

// fileDiff is one file's section of a unified diff.
type fileDiff struct {
	Path           string
	OldPath        string // the source path of a rename or copy
	Status         string // added, modified, deleted, renamed or copied
	Binary         bool
	Added, Removed int
	// Text is the section verbatim, header and hunks.
	Text string
	// Changed holds the new-side lines the section adds, merged into spans.
	Changed []span
	// Removals holds the old-side lines the section removes, merged into spans.
	Removals []span
	// Header is the section's lines before its first hunk; Hunks are the hunks, so Header plus every hunk's Text is Text.
	Header string
	Hunks  []hunk
	// Mode is the section's mode lines (old mode, new mode, new file mode, deleted file mode), one per line.
	Mode string
	// Blob is the object a binary section's index line names as the file's new content; Payload is the body of its GIT binary patch.
	Blob, Payload string
}

const diffHeader = "diff --git "

var hunkHeader = regexp.MustCompile(`^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@`)

// parseDiff splits a unified diff, as git or gh print it, into its file sections. Empty input has no sections; text without a diff header is an error, so a failure message is never taken for a diff.
func parseDiff(raw string) ([]fileDiff, error) {
	if raw == "" {
		return nil, nil
	}
	if !strings.HasPrefix(raw, diffHeader) {
		return nil, errors.New("not a unified diff: it does not start with a diff --git header")
	}
	var files []fileDiff
	rest := raw
	for rest != "" {
		end := strings.Index(rest[len(diffHeader):], "\n"+diffHeader)
		section := rest
		if end >= 0 {
			section = rest[:len(diffHeader)+end+1]
		}
		rest = rest[len(section):]
		f, err := parseSection(section)
		if err != nil {
			return nil, err
		}
		files = append(files, f)
	}
	return files, nil
}

func parseSection(section string) (fileDiff, error) {
	f := fileDiff{Status: "modified", Text: section}
	lines := strings.SplitAfter(section, "\n")
	headerLine := strings.TrimSuffix(lines[0], "\n")
	var oldName, newName string
	var modes, payload []string
	inPayload := false
	i := 1
	for ; i < len(lines); i++ {
		line := strings.TrimSuffix(lines[i], "\n")
		if strings.HasPrefix(line, "@@ ") {
			break
		}
		if inPayload {
			payload = append(payload, line)
			continue
		}
		switch {
		case strings.HasPrefix(line, "old mode "), strings.HasPrefix(line, "new mode "):
			modes = append(modes, line)
		case strings.HasPrefix(line, "new file mode"):
			f.Status = "added"
			modes = append(modes, line)
		case strings.HasPrefix(line, "deleted file mode"):
			f.Status = "deleted"
			modes = append(modes, line)
		case strings.HasPrefix(line, "index "):
			ids, _, _ := strings.Cut(strings.TrimPrefix(line, "index "), " ")
			_, f.Blob, _ = strings.Cut(ids, "..")
		case strings.HasPrefix(line, "rename from "):
			f.Status, f.OldPath = "renamed", unquote(strings.TrimPrefix(line, "rename from "))
		case strings.HasPrefix(line, "rename to "):
			f.Status, f.Path = "renamed", unquote(strings.TrimPrefix(line, "rename to "))
		case strings.HasPrefix(line, "copy from "):
			f.Status, f.OldPath = "copied", unquote(strings.TrimPrefix(line, "copy from "))
		case strings.HasPrefix(line, "copy to "):
			f.Status, f.Path = "copied", unquote(strings.TrimPrefix(line, "copy to "))
		case strings.HasPrefix(line, "--- "):
			oldName = diffName(strings.TrimPrefix(line, "--- "))
		case strings.HasPrefix(line, "+++ "):
			newName = diffName(strings.TrimPrefix(line, "+++ "))
		case line == "GIT binary patch":
			f.Binary, inPayload = true, true
		case strings.HasPrefix(line, "Binary files "):
			f.Binary = true
		}
	}
	f.Mode, f.Payload = strings.Join(modes, "\n"), strings.Join(payload, "\n")
	if f.Path == "" {
		switch {
		case newName != "":
			f.Path = newName
		case oldName != "":
			f.Path = oldName
		default:
			p, ok := headerPath(strings.TrimPrefix(headerLine, diffHeader))
			if !ok {
				return fileDiff{}, fmt.Errorf("cannot read the path in %q", headerLine)
			}
			f.Path = p
		}
	}
	if f.Status == "deleted" && oldName != "" {
		f.Path = oldName
	}
	f.Header = strings.Join(lines[:i], "")
	old, next := 0, 0
	var changed, removed, hunkRemoved []int
	var cur *hunk
	flush := func() {
		if cur != nil {
			cur.Removals = spans(hunkRemoved)
			f.Hunks = append(f.Hunks, *cur)
		}
	}
	for ; i < len(lines); i++ {
		line := lines[i]
		if m := hunkHeader.FindStringSubmatch(line); m != nil {
			flush()
			cur, hunkRemoved = &hunk{Text: line}, nil
			old, _ = strconv.Atoi(m[1])
			next, _ = strconv.Atoi(m[2])
			continue
		}
		if cur != nil {
			cur.Text += line
		}
		switch {
		case strings.HasPrefix(line, "+"):
			f.Added++
			changed = append(changed, next)
			next++
			if cur != nil {
				cur.Added = append(cur.Added, strings.TrimSuffix(line[1:], "\n"))
			}
		case strings.HasPrefix(line, "-"):
			f.Removed++
			removed = append(removed, old)
			hunkRemoved = append(hunkRemoved, old)
			old++
		case strings.HasPrefix(line, " "):
			old++
			next++
		}
	}
	flush()
	f.Changed, f.Removals = spans(changed), spans(removed)
	return f, nil
}

// spans merges ascending line numbers into inclusive runs.
func spans(lines []int) []span {
	var out []span
	for _, n := range lines {
		if k := len(out); k > 0 && out[k-1].Last+1 == n {
			out[k-1].Last = n
			continue
		}
		out = append(out, span{n, n})
	}
	return out
}

func unquote(s string) string {
	if strings.HasPrefix(s, `"`) {
		if u, err := strconv.Unquote(s); err == nil {
			return u
		}
	}
	return s
}

// diffName reads the path of a --- or +++ line: its a/ or b/ prefix and any tab suffix are dropped, and /dev/null is no name.
func diffName(s string) string {
	s, _, _ = strings.Cut(s, "\t")
	if s == "/dev/null" {
		return ""
	}
	s = unquote(s)
	if len(s) > 2 && (s[:2] == "a/" || s[:2] == "b/") {
		return s[2:]
	}
	return s
}

// headerPath reads the path from "a/P b/P" when both sides name the same file. Git C-quotes a name with control characters, quotes, backslashes or, by default, non-ASCII bytes, and quotes both sides then.
func headerPath(s string) (string, bool) {
	if strings.HasPrefix(s, `"`) {
		return quotedHeaderPath(s)
	}
	if len(s) < 5 || (len(s)-5)%2 != 0 {
		return "", false
	}
	n := (len(s) - 5) / 2
	if s[:2] != "a/" || s[2+n:5+n] != " b/" || s[2:2+n] != s[5+n:] {
		return "", false
	}
	return s[2 : 2+n], true
}

func quotedHeaderPath(s string) (string, bool) {
	first, err := strconv.QuotedPrefix(s)
	if err != nil {
		return "", false
	}
	rest, ok := strings.CutPrefix(s[len(first):], " ")
	if !ok {
		return "", false
	}
	a, err := strconv.Unquote(first)
	if err != nil {
		return "", false
	}
	b, err := strconv.Unquote(rest)
	if err != nil {
		return "", false
	}
	if len(a) < 3 || len(b) < 3 || a[:2] != "a/" || b[:2] != "b/" || a[2:] != b[2:] {
		return "", false
	}
	return a[2:], true
}

// changedLines returns the added and removed lines of the section, and the no-newline markers that follow them, ignoring context and hunk positions.
func (f fileDiff) changedLines() []string {
	var out []string
	inHunk, afterChange := false, false
	for line := range strings.Lines(f.Text) {
		line = strings.TrimSuffix(line, "\n")
		switch {
		case strings.HasPrefix(line, "@@ "):
			inHunk, afterChange = true, false
		case inHunk && (strings.HasPrefix(line, "+") || strings.HasPrefix(line, "-")):
			out = append(out, line)
			afterChange = true
		case inHunk && strings.HasPrefix(line, `\`):
			if afterChange {
				out = append(out, line)
			}
		default:
			afterChange = false
		}
	}
	return out
}

// sameChanges reports whether two patches change the same files the same way: the same added and removed lines, modes and, for binary files, resulting content, whatever their context lines, hunk positions and messages.
func sameChanges(a, b []fileDiff) bool {
	return slices.EqualFunc(a, b, func(x, y fileDiff) bool {
		return x.Path == y.Path && x.OldPath == y.OldPath && x.Status == y.Status && x.Binary == y.Binary && x.Mode == y.Mode &&
			sameBinary(x, y) && slices.Equal(x.changedLines(), y.changedLines())
	})
}

// sameBinary compares what two binary sections leave in the file: the object their index lines name, or their payloads when an index line is missing. Text sections always match.
func sameBinary(x, y fileDiff) bool {
	if !x.Binary {
		return true
	}
	if x.Blob != "" && y.Blob != "" {
		return x.Blob == y.Blob
	}
	return x.Blob == y.Blob && x.Payload == y.Payload
}

// fixedExclusion names why the review skips the file by its path or kind alone, or "" when the file needs a look at its contents.
func fixedExclusion(f fileDiff) string {
	switch base := path.Base(f.Path); {
	case base == "go.sum", base == "flake.lock":
		return "generated"
	case slices.Contains(strings.Split(f.Path, "/"), "testdata"):
		return "test data (`**/testdata/**`)"
	case f.Binary:
		return "binary"
	}
	return ""
}

// excludedReason names why the review skips the file from its diff section alone, or "" when it is included. A generated marker the section does not show is found by exclusionOf, which reads the file.
func excludedReason(f fileDiff) string {
	if reason := fixedExclusion(f); reason != "" {
		return reason
	}
	if isGenerated(f) {
		return "generated"
	}
	return ""
}

func isGeneratedMarker(line string) bool {
	return strings.HasPrefix(line, "// Code generated ") && strings.HasSuffix(line, " DO NOT EDIT.")
}

// isGenerated reports whether the section shows the generated marker: on a line it adds, or on a line it removes when it deletes the file, which then shows the whole old file.
func isGenerated(f fileDiff) bool {
	for line := range strings.Lines(f.Text) {
		line = strings.TrimSuffix(line, "\n")
		switch {
		case strings.HasPrefix(line, "+") && isGeneratedMarker(line[1:]):
			return true
		case f.Status == "deleted" && strings.HasPrefix(line, "-") && isGeneratedMarker(line[1:]):
			return true
		}
	}
	return false
}

// generatedContent reports whether a file's contents carry the generated marker the way Go defines it: as a whole line comment before the first source token. Blank space and complete line and block comments, however many lines a block spans, may come first; the marker inside a block comment does not count.
func generatedContent(content string) bool {
	inBlock := false
	for line := range strings.Lines(content) {
		rest := strings.TrimRight(line, "\r\n")
		if !inBlock && isGeneratedMarker(rest) {
			return true
		}
		for rest != "" {
			if inBlock {
				_, after, closed := strings.Cut(rest, "*/")
				if !closed {
					break
				}
				inBlock, rest = false, after
				continue
			}
			rest = strings.TrimLeft(rest, " \t")
			switch {
			case rest == "":
			case strings.HasPrefix(rest, "//"):
				rest = ""
			case strings.HasPrefix(rest, "/*"):
				inBlock, rest = true, rest[2:]
			default:
				return false
			}
		}
	}
	return false
}

// mergeByPath joins the sections of several patches into one section list, one entry per path in order of first appearance, so every path gets one hunk file.
func mergeByPath(patches [][]fileDiff) []fileDiff {
	var out []fileDiff
	at := map[string]int{}
	for _, files := range patches {
		for _, f := range files {
			k, seen := at[f.Path]
			if !seen {
				at[f.Path] = len(out)
				out = append(out, f)
				continue
			}
			m := &out[k]
			m.Text += f.Text
			m.Added += f.Added
			m.Removed += f.Removed
			m.Binary = m.Binary || f.Binary
			m.Changed = append(m.Changed, f.Changed...)
		}
	}
	return out
}
