// Package render turns journal events and diagnostics into human-readable lines: escaping, the line format, colour
// and the journald priority prefix.
package render

import (
	"fmt"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/shgew/togi/internal/journal"
)

func FormatLine(e journal.Event, loc *time.Location) string {
	return fmt.Sprintf("%s %-14s %s", e.Time.In(loc).Format("15:04:05"), EscapeText(string(e.Kind)), EscapeText(e.Msg))
}

// EscapeText makes untrusted text inert on a terminal without changing readable Unicode.
func EscapeText(text string) string {
	const hex = "0123456789abcdef"
	var out strings.Builder
	start := 0
	for i := 0; i < len(text); {
		r, size := utf8.DecodeRuneInString(text[i:])
		invalid := r == utf8.RuneError && size == 1
		if invalid || r < 0x20 || r >= 0x7f && r <= 0x9f ||
			r == '\u061c' || r == '\u200e' || r == '\u200f' ||
			r >= '\u2028' && r <= '\u202e' || r >= '\u2066' && r <= '\u2069' {
			if out.Len() == 0 {
				out.Grow(len(text))
			}
			out.WriteString(text[start:i])
			switch r {
			case '\t':
				out.WriteString(`\t`)
			case '\n':
				out.WriteString(`\n`)
			case '\r':
				out.WriteString(`\r`)
			default:
				if invalid {
					r = rune(text[i])
				}
				if r <= 0x7f || invalid {
					out.WriteString(`\x`)
				} else {
					out.WriteString(`\u`)
					out.WriteByte(hex[r>>12&0xf])
					out.WriteByte(hex[r>>8&0xf])
				}
				out.WriteByte(hex[r>>4&0xf])
				out.WriteByte(hex[r&0xf])
			}
			start = i + size
		}
		i += size
	}
	if start == 0 {
		return text
	}
	out.WriteString(text[start:])
	return out.String()
}
