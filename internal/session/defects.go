package session

import (
	"context"
	"errors"
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
	for _, event := range r.in.Journal.Events() {
		if answer, ok := event.Data.(*journal.DefectAnswered); ok && answer.Answer == "yes" {
			if err := r.queueDefectResets(event); err != nil {
				return nil, err
			}
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
			if errors.Is(err, context.Canceled) {
				stop, shutdownErr := r.shutdown(&journal.Shutdown{Reason: journal.ShutdownSignal}, StopSignal)
				return &stop, shutdownErr
			}
			if err != nil {
				return nil, fmt.Errorf("ask whether to reset defect %d cores: %w", found.ID, err)
			}
			answer := "no"
			if yes {
				answer = "yes"
			}
			event, err := r.append(&journal.DefectAnswered{ID: found.ID, Cores: slices.Clone(found.Cores), Answer: answer}, foundSeq)
			if err != nil {
				return nil, fmt.Errorf("record answer for defect %d: %w", found.ID, err)
			}
			if yes {
				if err := r.queueDefectResets(event); err != nil {
					return nil, err
				}
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

func (r *runner) queueDefectResets(answer journal.Event) error {
	events := r.in.Journal.Events()
	if legacyDefectResetsComplete(answer, events) {
		return nil
	}
	for _, core := range answer.Data.(*journal.DefectAnswered).Cores {
		recorded := false
		for _, event := range events {
			if reset, ok := event.Data.(*journal.CommandReset); ok && reset.Core != nil && *reset.Core == core && slices.Contains(event.Cause, answer.Seq) {
				recorded = true
				break
			}
		}
		if recorded {
			continue
		}
		if _, err := r.append(&journal.CommandReset{Core: new(core)}, answer.Seq); err != nil {
			return fmt.Errorf("queue reset of core %d for defect answer #%d: %w", core, answer.Seq, err)
		}
	}
	return nil
}

func legacyDefectResetsComplete(answer journal.Event, events []journal.Event) bool {
	cores := answer.Data.(*journal.DefectAnswered).Cores
	if answer.Seq < 1 || answer.Seq > len(events) || len(answer.Cause) != 1 {
		return false
	}
	at := answer.Seq - 2
	for _, core := range slices.Backward(cores) {
		if at < 0 {
			return false
		}
		event := events[at]
		if warning, ok := event.Data.(*journal.SessionWarning); ok {
			if at == 0 || warning.Operation != "write state projection" || !slices.Equal(event.Cause, []int{events[at-1].Seq}) {
				return false
			}
			at--
			event = events[at]
		}
		reset, ok := event.Data.(*journal.CommandReset)
		if !ok || reset.Core == nil || *reset.Core != core || !slices.Equal(event.Cause, answer.Cause) {
			return false
		}
		at--
	}
	return true
}
