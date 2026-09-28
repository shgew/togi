package session

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
)

func (r *runner) checkDefects() (*Stop, error) {
	entries := r.in.Defects
	if entries == nil {
		entries = defect.Entries()
	}
	for _, finding := range defect.FindWith(r.in.Journal.Events(), entries) {
		if _, err := r.append(&journal.DefectFound{
			ID: finding.Entry.ID, Title: finding.Entry.Title, Detail: finding.Entry.Detail, PR: finding.Entry.PR,
			Direction: string(finding.Entry.Direction), Cores: finding.Cores, Decisions: finding.Decisions,
		}, finding.Decisions...); err != nil {
			return nil, fmt.Errorf("record defect %d: %w", finding.Entry.ID, err)
		}
	}
	for _, found := range defect.Unanswered(r.in.Journal.Events()) {
		var entry defect.Entry
		for _, candidate := range entries {
			if candidate.ID == found.ID {
				entry = candidate
				break
			}
		}
		if entry.ID == 0 {
			continue
		}
		foundSeq := 0
		for _, event := range r.in.Journal.Events() {
			if p, ok := event.Data.(*journal.DefectFound); ok && p.ID == found.ID {
				foundSeq = event.Seq
				break
			}
		}
		if r.in.Prompt != nil {
			yes, err := r.in.Prompt(defect.Finding{Entry: entry, Cores: found.Cores, Decisions: found.Decisions})
			if err != nil {
				return nil, fmt.Errorf("ask whether to reset defect %d cores: %w", found.ID, err)
			}
			if yes {
				for _, core := range found.Cores {
					if _, err := r.append(&journal.CommandReset{Core: new(core)}, foundSeq); err != nil {
						return nil, fmt.Errorf("queue reset of core %d: %w", core, err)
					}
				}
			}
			answer := "no"
			if yes {
				answer = "yes"
			}
			if _, err := r.append(&journal.DefectAnswered{ID: found.ID, Cores: slices.Clone(found.Cores), Answer: answer}, foundSeq); err != nil {
				return nil, fmt.Errorf("record answer for defect %d: %w", found.ID, err)
			}
		} else if entry.Direction == defect.TooAggressive {
			return r.deadEnd(&journal.DeadEnd{
				Condition: journal.DeadEndDefect,
				Detail:    fmt.Sprintf("defect %d (%s) affected cores %v; run in a terminal to decide whether to reset them", found.ID, found.Title, found.Cores),
			}, foundSeq)
		}
	}
	return nil, nil
}
