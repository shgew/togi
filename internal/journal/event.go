// Package journal is the append-only record of a session: events, replay, the state.json projection and log lines.
package journal

import (
	"bytes"
	"encoding/json"
	"encoding/json/jsontext"
	jsonv2 "encoding/json/v2"
	"errors"
	"fmt"
	"strconv"
	"time"
)

const Schema = 4

const timeLayout = "2006-01-02T15:04:05.000000000Z"

type Kind string

const (
	KindSessionStart    Kind = "session.start"
	KindSessionContext  Kind = "session.context"
	KindSessionBaseline Kind = "session.baseline"
	KindSessionNotice   Kind = "session.notice"
	KindSessionWarning  Kind = "session.warning"
	KindSessionArchived Kind = "session.archived"
	KindSessionCarried  Kind = "session.carried"
	KindConfigLoaded    Kind = "config.loaded"
	KindPreflightCheck  Kind = "preflight.check"
	KindSMUIntent       Kind = "smu.intent"
	KindSMUWrite        Kind = "smu.write"
	KindSMUReadback     Kind = "smu.readback"
	KindSMUError        Kind = "smu.error"
	KindProfileApplied  Kind = "profile.applied"
	KindProfileChange   Kind = "profile.change"
	KindProfileRestored Kind = "profile.restored"
	KindTrialIntent     Kind = "trial.intent"
	KindTrialStart      Kind = "trial.start"
	KindTrialProgress   Kind = "trial.progress"
	KindTrialSignal     Kind = "trial.signal"
	KindTrialSample     Kind = "trial.sample"
	KindTrialEnd        Kind = "trial.end"
	KindTrialCarried    Kind = "trial.carried"
	KindFailure         Kind = "failure"
	KindFailureCarried  Kind = "failure.carried"
	KindMCE             Kind = "mce"
	KindCrashDetected   Kind = "crash.detected"
	KindTunerDecision   Kind = "tuner.decision"
	KindCorePhase       Kind = "core.phase"
	KindCheckingCycle     Kind = "checking.cycle"
	KindCheckingStep    Kind = "checking.step"
	KindHostRanking     Kind = "host.ranking"
	KindHuntStart       Kind = "hunt.start"
	KindHuntGroup       Kind = "hunt.group"
	KindHuntEnd         Kind = "hunt.end"
	KindHuntSkipped     Kind = "hunt.skipped"
	KindCombination     Kind = "combination"
	KindDeepeningRound  Kind = "deepening.round"
	KindTunerWarning    Kind = "tuner.warning"
	KindBackendRetry    Kind = "backend.retry"
	KindCommandReset    Kind = "command.reset"
	KindDefectFound     Kind = "defect.found"
	KindDefectAnswered  Kind = "defect.answered"
	KindDeadEnd         Kind = "deadend"
	KindBootSavedEntry  Kind = "boot.saved_entry"
	KindBootLeaveReason Kind = "boot.leave_reason"
	KindShutdown        Kind = "shutdown"
	KindJournalTorn     Kind = "journal.torn"
	KindStateRebuilt    Kind = "state.rebuilt"
)

type Payload interface {
	Kind() Kind
	Message() string
}

type Event struct {
	Seq   int
	Time  time.Time
	Mono  int64
	Boot  string
	Kind  Kind
	Msg   string
	Cause []int
	Data  Payload
	Raw   []byte
}

func encode(e Event, stamp ...bool) ([]byte, error) {
	payload, err := marshal(e.Data)
	if err != nil {
		return nil, fmt.Errorf("encode %s payload: %w", e.Kind, err)
	}
	header, err := marshal(struct {
		Time string `json:"time"`
		Boot string `json:"boot"`
		Kind Kind   `json:"kind"`
		Msg  string `json:"msg"`
	}{e.Time.UTC().Format(timeLayout), e.Boot, e.Kind, e.Msg})
	if err != nil {
		return nil, fmt.Errorf("encode %s envelope: %w", e.Kind, err)
	}
	b := make([]byte, 0, len(header)+len(payload)+64)
	b = append(b, `{"seq":`...)
	b = strconv.AppendInt(b, int64(e.Seq), 10)
	if e.Mono != 0 || len(stamp) > 0 && stamp[0] {
		b = append(b, `,"mono_ms":`...)
		b = strconv.AppendInt(b, e.Mono, 10)
	}
	b = append(b, ',')
	b = append(b, header[1:len(header)-1]...)
	if len(payload) > 2 {
		b = append(b, ',')
		b = append(b, payload[1:len(payload)-1]...)
	}
	if len(e.Cause) > 0 {
		c, err := marshal(e.Cause)
		if err != nil {
			return nil, fmt.Errorf("encode %s cause: %w", e.Kind, err)
		}
		b = append(b, `,"cause":`...)
		b = append(b, c...)
	}
	b = append(b, '}')
	return b, nil
}

func marshal(v any) ([]byte, error) {
	return jsonv2.Marshal(v, json.DefaultOptionsV1(), jsontext.EscapeForHTML(false))
}

type envelope struct {
	Seq   int       `json:"seq"`
	Time  time.Time `json:"time"`
	Boot  string    `json:"boot"`
	Mono  int64     `json:"mono_ms,omitempty"`
	Kind  Kind      `json:"kind"`
	Msg   string    `json:"msg"`
	Cause []int     `json:"cause"`
}

func decode(line []byte) (Event, error) {
	return decodeEvent(line, false)
}

func decodeEvent(line []byte, history bool) (Event, error) {
	if kind, ok := leadingKind(line); ok && (!history || kind != KindConfigLoaded) {
		if t, ok := payloadTypes[kind]; ok {
			if env, p, err := t.decode(line); err == nil && env.Kind == kind {
				return event(env, p, line), nil
			}
		}
	}
	var env envelope
	if err := json.Unmarshal(line, &env); err != nil {
		return Event{}, err
	}
	if env.Kind == "" {
		return Event{}, errors.New("event has no kind")
	}
	var p Payload
	var err error
	if history && env.Kind == KindConfigLoaded {
		// Historical configurations changed shape; fact readers need only their build stamp.
		var build Build
		err = json.Unmarshal(line, &build)
		p = &ConfigLoaded{Build: build}
	} else {
		p, err = decodePayload(env.Kind, line)
	}
	if err != nil {
		return Event{}, err
	}
	return event(env, p, line), nil
}

func event(env envelope, p Payload, line []byte) Event {
	return Event{
		Seq:   env.Seq,
		Time:  env.Time,
		Mono:  env.Mono,
		Boot:  env.Boot,
		Kind:  env.Kind,
		Msg:   env.Msg,
		Cause: env.Cause,
		Data:  p,
		Raw:   line,
	}
}

type payloadType struct {
	new    func() Payload
	decode func(line []byte) (envelope, Payload, error)
}

// framed decodes an event's envelope and its payload in one pass, matching json.Unmarshal of each.
type framed[P any] struct {
	envelope
	Payload *P `json:",embed"`
}

// strictDecode accepts only exact-case, unique, known names and valid UTF-8, where v2 and v1 decode alike;
// any other line errors and decodeEvent falls back to v1.
var strictDecode = jsonv2.RejectUnknownMembers(true)

func typeOf[P any, PP interface {
	*P
	Payload
}]() payloadType {
	return payloadType{
		new: func() Payload { return PP(new(P)) },
		decode: func(line []byte) (envelope, Payload, error) {
			f := framed[P]{Payload: new(P)}
			err := jsonv2.Unmarshal(line, &f, strictDecode)
			return f.envelope, PP(f.Payload), err
		},
	}
}

var payloadTypes = map[Kind]payloadType{
	KindSessionStart:    typeOf[SessionStart](),
	KindSessionContext:  typeOf[SessionContext](),
	KindSessionBaseline: typeOf[SessionBaseline](),
	KindSessionNotice:   typeOf[SessionNotice](),
	KindSessionWarning:  typeOf[SessionWarning](),
	KindSessionArchived: typeOf[SessionArchived](),
	KindSessionCarried:  typeOf[SessionCarried](),
	KindConfigLoaded:    typeOf[ConfigLoaded](),
	KindPreflightCheck:  typeOf[PreflightCheck](),
	KindSMUIntent:       typeOf[SMUIntent](),
	KindSMUWrite:        typeOf[SMUWrite](),
	KindSMUReadback:     typeOf[SMUReadback](),
	KindSMUError:        typeOf[SMUError](),
	KindProfileApplied:  typeOf[ProfileApplied](),
	KindProfileChange:   typeOf[ProfileChange](),
	KindProfileRestored: typeOf[ProfileRestored](),
	KindTrialIntent:     typeOf[TrialIntent](),
	KindTrialStart:      typeOf[TrialStart](),
	KindTrialProgress:   typeOf[TrialProgress](),
	KindTrialSignal:     typeOf[TrialSignal](),
	KindTrialSample:     typeOf[TrialSample](),
	KindTrialEnd:        typeOf[TrialEnd](),
	KindTrialCarried:    typeOf[TrialCarried](),
	KindFailure:         typeOf[Failure](),
	KindFailureCarried:  typeOf[FailureCarried](),
	KindMCE:             typeOf[MCE](),
	KindCrashDetected:   typeOf[CrashDetected](),
	KindTunerDecision:   typeOf[TunerDecision](),
	KindCorePhase:       typeOf[CorePhase](),
	KindCheckingCycle:     typeOf[CheckingCycle](),
	KindCheckingStep:    typeOf[CheckingStep](),
	KindHostRanking:     typeOf[HostRanking](),
	KindHuntStart:       typeOf[HuntStart](),
	KindHuntGroup:       typeOf[HuntGroup](),
	KindHuntEnd:         typeOf[HuntEnd](),
	KindHuntSkipped:     typeOf[HuntSkipped](),
	KindCombination:     typeOf[Combination](),
	KindDeepeningRound:  typeOf[DeepeningRound](),
	KindTunerWarning:    typeOf[TunerWarning](),
	KindBackendRetry:    typeOf[BackendRetry](),
	KindCommandReset:    typeOf[CommandReset](),
	KindDefectFound:     typeOf[DefectFound](),
	KindDefectAnswered:  typeOf[DefectAnswered](),
	KindDeadEnd:         typeOf[DeadEnd](),
	KindBootSavedEntry:  typeOf[BootSavedEntry](),
	KindBootLeaveReason: typeOf[BootLeaveReason](),
	KindShutdown:        typeOf[Shutdown](),
	KindJournalTorn:     typeOf[JournalTorn](),
	KindStateRebuilt:    typeOf[StateRebuilt](),
}

func decodePayload(kind Kind, raw []byte) (Payload, error) {
	t, ok := payloadTypes[kind]
	if !ok {
		return nil, nil
	}
	p := t.new()
	if err := json.Unmarshal(raw, p); err != nil {
		return nil, fmt.Errorf("decode %s: %w", kind, err)
	}
	return p, nil
}

// leadingKind finds the kind encode writes before any payload field; decodeEvent confirms it against the decoded envelope.
func leadingKind(line []byte) (Kind, bool) {
	_, rest, ok := bytes.Cut(line, []byte(`"kind":"`))
	if !ok {
		return "", false
	}
	kind, _, ok := bytes.Cut(rest, []byte{'"'})
	if !ok || bytes.IndexByte(kind, '\\') >= 0 {
		return "", false
	}
	return Kind(kind), true
}
