package journal

import (
	"encoding/json"
	"fmt"
	"slices"
	"strings"
	"time"
)

func FormatLine(e Event, loc *time.Location) string {
	return fmt.Sprintf("%s %-14s %s", e.Time.In(loc).Format("15:04:05"), e.Kind, e.Msg)
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
	}
	if err := json.Unmarshal(e.Raw, &fields); err != nil {
		return false
	}
	if f.Trial != "" && fields.Trial != f.Trial {
		return false
	}
	if f.Core != nil {
		var cores []int
		_ = json.Unmarshal(fields.Cores, &cores)
		if (fields.Core == nil || *fields.Core != *f.Core) && !slices.Contains(cores, *f.Core) && !slices.Contains(fields.Candidates, *f.Core) && !slices.ContainsFunc(fields.Members, func(m JointMember) bool { return m.Core == *f.Core }) {
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
