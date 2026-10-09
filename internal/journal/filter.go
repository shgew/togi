package journal

import (
	"encoding/json"
	"fmt"
	"slices"
	"strings"
	"time"
)

type Filter struct {
	Core  *int
	Kinds []string
	Trial string
	Since time.Time
	Until time.Time
}

// Match reports whether e passes f on its own fields; Select adds the trial outcomes of f.Core.
func (f Filter) Match(e Event) bool { return f.match(e, nil) }

// Select returns the events that match f, oldest first. With Core set it also returns the
// trial.end and the failures naming no core of every trial whose trial.intent loads that core,
// so events must start at the journal's start: a trial that began before Since still counts.
func (f Filter) Select(events []Event) []Event {
	var loading map[string]bool
	if f.Core != nil {
		loading = map[string]bool{}
	}
	var selected []Event
	for _, e := range events {
		if intent, ok := e.Data.(*TrialIntent); ok && loading != nil {
			loading[intent.Trial] = intent.Core != nil && *intent.Core == *f.Core || slices.Contains(intent.Cores, *f.Core)
		}
		if f.match(e, loading) {
			selected = append(selected, e)
		}
	}
	return selected
}

// match reports whether e passes f; loading holds the trials whose intent loads f.Core.
func (f Filter) match(e Event, loading map[string]bool) bool {
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
		Core       *int                `json:"core"`
		Cores      json.RawMessage     `json:"cores"`
		Candidates []int               `json:"candidates"`
		Members    []CombinationMember `json:"members"`
		Trial      string              `json:"trial"`
		Source     FactSource          `json:"source"`
		Class      TrialClass          `json:"class"`
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
		named := fields.Core != nil && *fields.Core == *f.Core || slices.Contains(cores, *f.Core) || slices.Contains(fields.Class.Cores, *f.Core) || slices.Contains(fields.Candidates, *f.Core) || slices.ContainsFunc(fields.Members, func(m CombinationMember) bool { return m.Core == *f.Core })
		ofTrial := (e.Kind == KindTrialEnd || e.Kind == KindFailure && fields.Core == nil) && loading[fields.Trial]
		if !named && !ofTrial {
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

// KindSelectors returns every exact kind and group that --kind accepts, sorted.
func KindSelectors() []string {
	names := make([]string, 0, 2*len(payloadTypes))
	for kind := range payloadTypes {
		names = append(names, string(kind))
		group, _, _ := strings.Cut(string(kind), ".")
		names = append(names, group)
	}
	slices.Sort(names)
	return slices.Compact(names)
}

func ValidateKindSelector(name string) error {
	names := KindSelectors()
	if name != "" && slices.Contains(names, name) {
		return nil
	}
	if name == "" {
		return fmt.Errorf("empty kind list; valid names: %s", strings.Join(names, ", "))
	}
	return fmt.Errorf("unknown kind or group %q; valid names: %s", name, strings.Join(names, ", "))
}
