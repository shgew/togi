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

// span is an inclusive range of new-side line numbers.
type span struct{ First, Last int }

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
}

const diffHeader = "diff --git "

var hunkHeader = regexp.MustCompile(`^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@`)

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
	i := 1
	for ; i < len(lines); i++ {
		line := strings.TrimSuffix(lines[i], "\n")
		if strings.HasPrefix(line, "@@ ") {
			break
		}
		switch {
		case strings.HasPrefix(line, "new file mode"):
			f.Status = "added"
		case strings.HasPrefix(line, "deleted file mode"):
			f.Status = "deleted"
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
		case strings.HasPrefix(line, "Binary files "), line == "GIT binary patch":
			f.Binary = true
		}
	}
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
	next := 0
	var changed []int
	for ; i < len(lines); i++ {
		line := lines[i]
		if m := hunkHeader.FindStringSubmatch(line); m != nil {
			next, _ = strconv.Atoi(m[1])
			continue
		}
		switch {
		case strings.HasPrefix(line, "+"):
			f.Added++
			changed = append(changed, next)
			next++
		case strings.HasPrefix(line, "-"):
			f.Removed++
		case strings.HasPrefix(line, " "):
			next++
		}
	}
	f.Changed = spans(changed)
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

// headerPath reads the path from "a/P b/P" when both sides name the same file.
func headerPath(s string) (string, bool) {
	if len(s) < 5 || (len(s)-5)%2 != 0 {
		return "", false
	}
	n := (len(s) - 5) / 2
	if s[:2] != "a/" || s[2+n:5+n] != " b/" || s[2:2+n] != s[5+n:] {
		return "", false
	}
	return s[2 : 2+n], true
}

// changedLines returns the added and removed lines of the section, ignoring context and hunk positions.
func (f fileDiff) changedLines() []string {
	var out []string
	inHunk := false
	for line := range strings.Lines(f.Text) {
		line = strings.TrimSuffix(line, "\n")
		switch {
		case strings.HasPrefix(line, "@@ "):
			inHunk = true
		case inHunk && (strings.HasPrefix(line, "+") || strings.HasPrefix(line, "-")):
			out = append(out, line)
		}
	}
	return out
}

// sameChanges reports whether two patches add and remove the same lines in the same files, whatever their context lines, hunk positions and messages.
func sameChanges(a, b []fileDiff) bool {
	return slices.EqualFunc(a, b, func(x, y fileDiff) bool {
		return x.Path == y.Path && x.OldPath == y.OldPath && x.Status == y.Status && x.Binary == y.Binary &&
			slices.Equal(x.changedLines(), y.changedLines())
	})
}

// excludedReason names why the review skips the file, or "" when it is included.
func excludedReason(f fileDiff) string {
	switch base := path.Base(f.Path); {
	case base == "go.sum", base == "flake.lock":
		return "generated"
	case slices.Contains(strings.Split(f.Path, "/"), "testdata"):
		return "test data (`**/testdata/**`)"
	case f.Binary:
		return "binary"
	case isGenerated(f):
		return "generated"
	}
	return ""
}

func isGenerated(f fileDiff) bool {
	for line := range strings.Lines(f.Text) {
		line = strings.TrimSuffix(line, "\n")
		if strings.HasPrefix(line, "+// Code generated ") && strings.HasSuffix(line, " DO NOT EDIT.") {
			return true
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
