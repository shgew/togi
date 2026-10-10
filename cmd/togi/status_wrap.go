package main

import (
	"bytes"
	"fmt"
	"io"
	"strings"
	"unicode/utf8"
)

// statusWidth is the terminal width status fits. Words are never split, so a
// single word longer than the room overflows instead.
const statusWidth = 80

// wrapLines writes text word-wrapped to statusWidth columns: the first line
// starts with first, every following line with rest. Leading newlines of first
// print as blank lines before the text.
func wrapLines(w io.Writer, first, rest, text string) {
	body := strings.TrimLeft(first, "\n")
	fmt.Fprint(w, first[:len(first)-len(body)])
	line, width := body, utf8.RuneCountInString(body)
	empty := true
	for word := range strings.FieldsSeq(text) {
		n := utf8.RuneCountInString(word)
		if !empty && width+1+n > statusWidth {
			fmt.Fprintln(w, line)
			line, width, empty = rest, utf8.RuneCountInString(rest), true
		}
		if !empty {
			line += " "
			width++
		}
		line += word
		width += n
		empty = false
	}
	fmt.Fprintln(w, line)
}

// rowNote is text printed wrapped under a table row, led by label.
type rowNote struct{ label, text string }

// notedRow is one table row: tab-separated cells, and notes printed wrapped
// on the lines below the row so that their length cannot widen the table.
type notedRow struct {
	cells string
	notes []rowNote
}

// writeNotedTable writes rows as an aligned table, each row followed by its
// notes wrapped and indented under the row.
func writeNotedTable(w io.Writer, rows []notedRow) {
	var buf bytes.Buffer
	tw := newTable(&buf)
	for _, r := range rows {
		fmt.Fprintln(tw, r.cells)
	}
	_ = tw.Flush()
	lines := strings.Split(strings.TrimSuffix(buf.String(), "\n"), "\n")
	for i, line := range lines {
		if utf8.RuneCountInString(line) > statusWidth {
			wrapLines(w, "", "  ", line)
		} else {
			fmt.Fprintln(w, line)
		}
		for _, n := range rows[i].notes {
			prefix := "  " + n.label + " "
			wrapLines(w, prefix, strings.Repeat(" ", utf8.RuneCountInString(prefix)), n.text)
		}
	}
}
