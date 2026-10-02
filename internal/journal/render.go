package journal

import (
	"encoding/json"
	"fmt"
	"slices"
	"strings"
	"time"
	"unicode/utf8"
)

func FormatLine(e Event, loc *time.Location) string {
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

type Filter struct {
	Core  *int
	Kinds []string
	Trial string
	Since time.Time
	Until time.Time
}

func (f Filter) Match(e Event) bool {
	if len(f.Kinds) > 0 && !slices.ContainsFunc(f.Kinds, func(k string) bool { return kindMatches(k, e.Kind) }) {
		return false
	}
	if !f.Since.IsZero() && e.Time.Before(f.Since) {
		return false
	}
	if !f.Until.IsZero() && !e.Time.Before(f.Until) {
		return false
	}
	if f.Core == nil && f.Trial == "" {
		return true
	}
	var fields struct {
		Core       *int            `json:"core"`
		Cores      json.RawMessage `json:"cores"`
		Candidates []int           `json:"candidates"`
		Members    []JointMember   `json:"members"`
		Trial      string          `json:"trial"`
		Source     FactSource      `json:"source"`
		Class      TrialClass      `json:"class"`
	}
	if err := json.Unmarshal(e.Raw, &fields); err != nil {
		return false
	}
	if e.Kind == KindTrialCarried || e.Kind == KindFailureCarried {
		fields.Trial = fields.Source.Trial
	}
	if f.Trial != "" && fields.Trial != f.Trial {
		return false
	}
	if f.Core != nil {
		var cores []int
		_ = json.Unmarshal(fields.Cores, &cores)
		if (fields.Core == nil || *fields.Core != *f.Core) && !slices.Contains(cores, *f.Core) && !slices.Contains(fields.Class.Cores, *f.Core) && !slices.Contains(fields.Candidates, *f.Core) && !slices.ContainsFunc(fields.Members, func(m JointMember) bool { return m.Core == *f.Core }) {
			return false
		}
	}
	return true
}

func kindMatches(entry string, kind Kind) bool {
	if entry == string(kind) {
		return true
	}
	if strings.Contains(entry, ".") {
		return false
	}
	group, _, _ := strings.Cut(string(kind), ".")
	return group == entry
}

func ValidateKindSelector(name string) error {
	if _, ok := payloadConstructors[Kind(name)]; ok {
		return nil
	}
	if name != "" && !strings.Contains(name, ".") {
		for kind := range payloadConstructors {
			if kindMatches(name, kind) {
				return nil
			}
		}
	}
	names := make([]string, 0, 2*len(payloadConstructors))
	for kind := range payloadConstructors {
		names = append(names, string(kind))
		group, _, _ := strings.Cut(string(kind), ".")
		names = append(names, group)
	}
	slices.Sort(names)
	names = slices.Compact(names)
	if name == "" {
		return fmt.Errorf("empty kind list; valid names: %s", strings.Join(names, ", "))
	}
	return fmt.Errorf("unknown kind or group %q; valid names: %s", name, strings.Join(names, ", "))
}
