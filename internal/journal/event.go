// Package journal is the append-only record of a session: events, replay, the state.json projection and log lines.
package journal

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"strconv"
	"time"
)

const Schema = 2

const timeLayout = "2006-01-02T15:04:05.000000000Z"

type Kind string

const (
	KindSessionStart    Kind = "session.start"
	KindSessionContext  Kind = "session.context"
	KindSessionBaseline Kind = "session.baseline"
	KindSessionNotice   Kind = "session.notice"
	KindSessionArchived Kind = "session.archived"
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
	KindFailure         Kind = "failure"
	KindMCE             Kind = "mce"
	KindCrashDetected   Kind = "crash.detected"
	KindTunerDecision   Kind = "tuner.decision"
	KindCorePhase       Kind = "core.phase"
	KindGuardRotation   Kind = "guard.rotation"
	KindTierChange      Kind = "tier.change"
	KindCommandReset    Kind = "command.reset"
	KindDefectFound     Kind = "defect.found"
	KindDefectAnswered  Kind = "defect.answered"
	KindDeadEnd         Kind = "deadend"
	KindBootSavedEntry  Kind = "boot.saved_entry"
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
	Boot  string
	Kind  Kind
	Msg   string
	Cause []int
	Data  Payload
	Raw   []byte
}

func encode(e Event) ([]byte, error) {
	payload, err := marshal(e.Data)
	if err != nil {
		return nil, fmt.Errorf("encode %s payload: %w", e.Kind, err)
	}
	var b bytes.Buffer
	b.WriteString(`{"seq":`)
	b.WriteString(strconv.Itoa(e.Seq))
	for _, f := range []struct{ key, value string }{
		{"time", e.Time.UTC().Format(timeLayout)},
		{"boot", e.Boot},
		{"kind", string(e.Kind)},
		{"msg", e.Msg},
	} {
		v, err := marshal(f.value)
		if err != nil {
			return nil, fmt.Errorf("encode %s %s: %w", e.Kind, f.key, err)
		}
		fmt.Fprintf(&b, `,%q:`, f.key)
		b.Write(v)
	}
	if len(payload) > 2 {
		b.WriteByte(',')
		b.Write(payload[1 : len(payload)-1])
	}
	if len(e.Cause) > 0 {
		c, err := marshal(e.Cause)
		if err != nil {
			return nil, fmt.Errorf("encode %s cause: %w", e.Kind, err)
		}
		b.WriteString(`,"cause":`)
		b.Write(c)
	}
	b.WriteByte('}')
	return b.Bytes(), nil
}

func marshal(v any) ([]byte, error) {
	var b bytes.Buffer
	enc := json.NewEncoder(&b)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(v); err != nil {
		return nil, err
	}
	return bytes.TrimSuffix(b.Bytes(), []byte("\n")), nil
}

type envelope struct {
	Seq   int       `json:"seq"`
	Time  time.Time `json:"time"`
	Boot  string    `json:"boot"`
	Kind  Kind      `json:"kind"`
	Msg   string    `json:"msg"`
	Cause []int     `json:"cause"`
}

func decode(line []byte) (Event, error) {
	var env envelope
	if err := json.Unmarshal(line, &env); err != nil {
		return Event{}, err
	}
	if env.Kind == "" {
		return Event{}, errors.New("event has no kind")
	}
	p, err := decodePayload(env.Kind, line)
	if err != nil {
		return Event{}, err
	}
	return Event{
		Seq:   env.Seq,
		Time:  env.Time,
		Boot:  env.Boot,
		Kind:  env.Kind,
		Msg:   env.Msg,
		Cause: env.Cause,
		Data:  p,
		Raw:   line,
	}, nil
}

func decodePayload(kind Kind, raw []byte) (Payload, error) {
	var p Payload
	switch kind {
	case KindSessionStart:
		p = &SessionStart{}
	case KindSessionContext:
		p = &SessionContext{}
	case KindSessionBaseline:
		p = &SessionBaseline{}
	case KindSessionNotice:
		p = &SessionNotice{}
	case KindSessionArchived:
		p = &SessionArchived{}
	case KindConfigLoaded:
		p = &ConfigLoaded{}
	case KindPreflightCheck:
		p = &PreflightCheck{}
	case KindSMUIntent:
		p = &SMUIntent{}
	case KindSMUWrite:
		p = &SMUWrite{}
	case KindSMUReadback:
		p = &SMUReadback{}
	case KindSMUError:
		p = &SMUError{}
	case KindProfileApplied:
		p = &ProfileApplied{}
	case KindProfileChange:
		p = &ProfileChange{}
	case KindProfileRestored:
		p = &ProfileRestored{}
	case KindTrialIntent:
		p = &TrialIntent{}
	case KindTrialStart:
		p = &TrialStart{}
	case KindTrialProgress:
		p = &TrialProgress{}
	case KindTrialSignal:
		p = &TrialSignal{}
	case KindTrialSample:
		p = &TrialSample{}
	case KindTrialEnd:
		p = &TrialEnd{}
	case KindFailure:
		p = &Failure{}
	case KindMCE:
		p = &MCE{}
	case KindCrashDetected:
		p = &CrashDetected{}
	case KindTunerDecision:
		p = &TunerDecision{}
	case KindCorePhase:
		p = &CorePhase{}
	case KindGuardRotation:
		p = &GuardRotation{}
	case KindTierChange:
		p = &TierChange{}
	case KindCommandReset:
		p = &CommandReset{}
	case KindDefectFound:
		p = &DefectFound{}
	case KindDefectAnswered:
		p = &DefectAnswered{}
	case KindDeadEnd:
		p = &DeadEnd{}
	case KindBootSavedEntry:
		p = &BootSavedEntry{}
	case KindShutdown:
		p = &Shutdown{}
	case KindJournalTorn:
		p = &JournalTorn{}
	case KindStateRebuilt:
		p = &StateRebuilt{}
	}
	if p == nil {
		return nil, fmt.Errorf("unknown kind %q", kind)
	}
	if err := json.Unmarshal(raw, p); err != nil {
		return nil, fmt.Errorf("decode %s: %w", kind, err)
	}
	return p, nil
}
