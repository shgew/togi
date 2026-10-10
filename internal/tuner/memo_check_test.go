package tuner

import (
	"fmt"
	"reflect"
	"slices"
)

// CheckMemos recomputes every index and every memo State still holds as valid, and reports the first that differs
// from what the folded events give. Deep also checks the per-trial derived requests, which cost a pass over the
// ledger.
func CheckMemos(s *State, deep bool) error {
	g := &s.checking
	if s.reqByStep != nil && s.reqEpoch == s.checkingEpoch {
		for step, got := range s.reqByStep {
			if want := s.computeRequirements(step); !reflect.DeepEqual(got, want) {
				return fmt.Errorf("requirements(%d) = %+v, want %+v", step, got, want)
			}
		}
	}
	if s.cycleMemo != nil && s.cycleCheckingEpoch == s.checkingEpoch && s.cycleEvidenceEpoch == s.evidenceEpoch {
		for k, got := range s.cycleMemo {
			if want := s.passes(k, g.profile, g.startSeq, cycleEvidence); got != want {
				return fmt.Errorf("cyclePasses(%+v) = %d, want %d", k, got, want)
			}
		}
	}
	if s.roundMemo && s.roundGen == s.gen {
		checks, sources := s.computeRoundChecks()
		if !reflect.DeepEqual(checks, s.roundChecksMemo) || !reflect.DeepEqual(sources, s.roundSourcesMemo) {
			return fmt.Errorf("round checks = %+v %v, want %+v %v", s.roundChecksMemo, s.roundSourcesMemo, checks, sources)
		}
	}
	for seq, p := range s.failurePos {
		want := -1
		s.eachFailureEntry(p.class, seq, func(i int) {
			if want < 0 || i < want {
				want = i
			}
		})
		if p.pos != want {
			return fmt.Errorf("failureEntryPos(%d) = %d, want %d", seq, p.pos, want)
		}
	}
	if s.r7OpenBuilt && s.r7OpenEpoch == s.r7Epoch {
		open := make([]bool, len(s.pendingFailures))
		for _, i := range s.r7Open {
			open[i] = true
		}
		for i, f := range s.pendingFailures {
			if open[i] {
				continue
			}
			if _, ok, settled := s.r7FailureDecision(f); ok || !settled {
				return fmt.Errorf("failure %d left the open list but decides %t, settled %t", f.seq, ok, settled)
			}
		}
	}
	if s.phasesSettled && s.phasesEpoch == s.limitEpoch {
		if a, ok := s.computePhaseNext(); ok {
			return fmt.Errorf("phases settled but phaseNext decides %+v", a.Payload)
		}
	}
	if !deep {
		return nil
	}
	for k, entries := range s.ledger {
		var want []int
		for i, e := range entries {
			if i > 0 && entries[i-1].seq >= e.seq {
				return fmt.Errorf("ledger %+v out of seq order at %d", k, e.seq)
			}
			if !e.pass {
				want = append(want, i)
			}
		}
		if got := s.classFailures[k]; !slices.Equal(got, want) {
			return fmt.Errorf("classFailures[%+v] = %v, want %v", k, got, want)
		}
	}
	measurements := s.measurements
	s.indexMeasurements()
	if !reflect.DeepEqual(measurements, s.measurements) && (len(measurements) != 0 || len(s.measurements) != 0) {
		return fmt.Errorf("measurement index = %v, want %v", measurements, s.measurements)
	}
	bySeq := map[int]entry{}
	for _, entries := range s.ledger {
		for _, e := range entries {
			bySeq[e.seq] = e
		}
	}
	for _, e := range s.r7Measurements {
		if _, ok := bySeq[e.seq]; !ok {
			bySeq[e.seq] = e
		}
	}
	for seq, d := range s.derivedBySeq {
		e, ok := bySeq[seq]
		if !ok || e.class.workload != d.workload || !slices.Equal(e.cores, d.cores) || !slices.Equal(e.profile, d.profile) {
			continue
		}
		req, sources := e.requests, []int{seq}
		if len(e.requests) == 0 {
			req, sources = s.r7RequestsBefore(e.class.workload, e.cores, e.profile, e.seq)
		}
		if !reflect.DeepEqual(req, d.req) || !slices.Equal(sources, d.sources) {
			return fmt.Errorf("requests derived for trial %d = %v %v, want %v %v", seq, d.req, d.sources, req, sources)
		}
		if d.hasTop {
			if want := s.r7TopRequests(e.cores, req); !slices.Equal(d.top, want) {
				return fmt.Errorf("top derived for trial %d = %v, want %v", seq, d.top, want)
			}
		}
	}
	return nil
}
