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
func (f Filter) Match(e Event) bool { return f.match(e, nil, nil) }

// Select returns the events that match f, oldest first. With Core set it also returns the
// trial.end and the failures naming no core of every trial whose trial.intent loads that core,
// so events must start at the journal's start: a trial that began before Since still counts.
// A known-failure skip (a failure with known_failure) never takes its trial, which can be a
// carried source's trial ID that collides with a local one; it matches when the event it names
// matched the core, wherever that event lies in the journal.
func (f Filter) Select(events []Event) []Event {
	var loading map[string]bool
	var matched map[int]bool
	if f.Core != nil {
		loading, matched = map[string]bool{}, map[int]bool{}
	}
	var selected []Event
	for _, e := range events {
		if intent, ok := e.Data.(*TrialIntent); ok && loading != nil {
			loading[intent.Trial] = intent.Core != nil && *intent.Core == *f.Core || slices.Contains(intent.Cores, *f.Core)
		}
		if f.match(e, loading, matched) {
			selected = append(selected, e)
		}
	}
	return selected
}

// match reports whether e passes f; loading holds the trials whose intent loads f.Core and
// matched, which match fills, the seqs of the events that meet the core criterion, whatever
// Kinds, Since, Until and Trial say.
func (f Filter) match(e Event, loading map[string]bool, matched map[int]bool) bool {
	inWindow := (len(f.Kinds) == 0 || slices.ContainsFunc(f.Kinds, func(k string) bool { return kindMatches(k, e.Kind) })) &&
		(f.Since.IsZero() || !e.Time.Before(f.Since)) &&
		(f.Until.IsZero() || e.Time.Before(f.Until))
	if f.Core == nil && f.Trial == "" {
		return inWindow
	}
	if !inWindow && matched == nil {
		return false
	}
	var fields struct {
		Core         *int                `json:"core"`
		Cores        json.RawMessage     `json:"cores"`
		Candidates   []int               `json:"candidates"`
		Members      []CombinationMember `json:"members"`
		Trial        string              `json:"trial"`
		KnownFailure int                 `json:"known_failure"`
		Source       FactSource          `json:"source"`
		Class        TrialClass          `json:"class"`
	}
	if err := json.Unmarshal(e.Raw, &fields); err != nil {
		return false
	}
	if e.Kind == KindTrialCarried || e.Kind == KindFailureCarried {
		fields.Trial = fields.Source.Trial
	}
	coreOK := true
	if f.Core != nil {
		var cores []int
		_ = json.Unmarshal(fields.Cores, &cores)
		named := fields.Core != nil && *fields.Core == *f.Core || slices.Contains(cores, *f.Core) || slices.Contains(fields.Class.Cores, *f.Core) || slices.Contains(fields.Candidates, *f.Core) || slices.ContainsFunc(fields.Members, func(m CombinationMember) bool { return m.Core == *f.Core })
		var ofTrial bool
		if e.Kind == KindFailure && fields.KnownFailure != 0 {
			ofTrial = matched[fields.KnownFailure]
		} else {
			ofTrial = (e.Kind == KindTrialEnd || e.Kind == KindFailure && fields.Core == nil) && loading[fields.Trial]
		}
		coreOK = named || ofTrial
		if coreOK && matched != nil {
			matched[e.Seq] = true
		}
	}
	return inWindow && coreOK && (f.Trial == "" || fields.Trial == f.Trial)
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
