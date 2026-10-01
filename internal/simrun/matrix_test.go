package simrun

import (
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type matrixResult struct {
	Attribution []journal.Failure
	Decisions   []journal.TunerDecision
	Phases      []journal.CorePhase
	Marks       []journal.MarkJoint
	Profiles    []journal.ProfileChange
	Hunts       []matrixHunt
	Masks       []journal.HuntMask
	Ends        []journal.HuntEnd
	Rounds      []matrixRound
	Checks      []matrixTrial
	Reruns      []matrixTrial
}

type matrixHunt struct {
	Start           journal.HuntStart
	Failure         journal.Failure
	QualifiedAnchor bool
}

type matrixRound struct {
	Round           journal.RefineRound
	QualifiedAnchor bool
}

type matrixTrial struct {
	Intent    journal.TrialIntent
	Outcome   journal.Outcome
	Signal    machine.Signal
	Core      *int
	DurationS int
}

func matrixCommitments(events []journal.Event) matrixResult {
	var out matrixResult
	intents := map[string]journal.TrialIntent{}
	failures := map[int]journal.Failure{}
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.Failure:
			failure := *p
			failure.Trial = ""
			failures[e.Seq] = failure
			out.Attribution = append(out.Attribution, failure)
		case *journal.TunerDecision:
			out.Decisions = append(out.Decisions, *p)
		case *journal.CorePhase:
			out.Phases = append(out.Phases, *p)
		case *journal.MarkJoint:
			out.Marks = append(out.Marks, *p)
		case *journal.ProfileChange:
			out.Profiles = append(out.Profiles, *p)
		case *journal.HuntStart:
			start := *p
			start.Trial, start.Failure, start.AnchorSeq = "", 0, 0
			out.Hunts = append(out.Hunts, matrixHunt{Start: start, Failure: failures[p.Failure], QualifiedAnchor: p.AnchorSeq != 0})
		case *journal.HuntMask:
			out.Masks = append(out.Masks, *p)
		case *journal.HuntEnd:
			out.Ends = append(out.Ends, *p)
		case *journal.RefineRound:
			round := *p
			round.AnchorSeq = 0
			out.Rounds = append(out.Rounds, matrixRound{Round: round, QualifiedAnchor: p.AnchorSeq != 0})
		case *journal.TrialIntent:
			intent := *p
			intent.Trial, intent.KernelBoundary = "", journal.KernelBoundary{}
			intents[p.Trial] = intent
		case *journal.TrialEnd:
			if p.Outcome == journal.OutcomeInconclusive {
				continue
			}
			intent, ok := intents[p.Trial]
			if !ok {
				continue
			}
			trial := matrixTrial{Intent: intent, Outcome: p.Outcome, Signal: p.Signal, Core: p.Core, DurationS: p.DurationS}
			if intent.Round != 0 {
				out.Checks = append(out.Checks, trial)
			}
			if intent.Rerun {
				out.Reruns = append(out.Reruns, trial)
			}
		}
	}
	return out
}
