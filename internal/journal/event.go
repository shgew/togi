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
	KindGuardRotation   Kind = "guard.rotation"
	KindHostRanking     Kind = "host.ranking"
	KindHuntStart       Kind = "hunt.start"
	KindHuntMask        Kind = "hunt.mask"
	KindHuntEnd         Kind = "hunt.end"
	KindHuntSkipped     Kind = "hunt.skipped"
	KindMarkJoint       Kind = "mark.joint"
	KindRefineRound     Kind = "refine.round"
	KindTunerWarning    Kind = "tuner.warning"
	KindBackendRetry    Kind = "backend.retry"
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
	var b bytes.Buffer
	b.WriteString(`{"seq":`)
	b.WriteString(strconv.Itoa(e.Seq))
	if e.Mono != 0 || len(stamp) > 0 && stamp[0] {
		fmt.Fprintf(&b, `,"mono_ms":%d`, e.Mono)
	}
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
	Mono  int64     `json:"mono_ms,omitempty"`
	Kind  Kind      `json:"kind"`
	Msg   string    `json:"msg"`
	Cause []int     `json:"cause"`
}

func decode(line []byte) (Event, error) {
	return decodeEvent(line, false)
}

func decodeEvent(line []byte, history bool) (Event, error) {
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
	}, nil
}

var payloadConstructors = map[Kind]func() Payload{
	KindSessionStart:    func() Payload { return &SessionStart{} },
	KindSessionContext:  func() Payload { return &SessionContext{} },
	KindSessionBaseline: func() Payload { return &SessionBaseline{} },
	KindSessionNotice:   func() Payload { return &SessionNotice{} },
	KindSessionWarning:  func() Payload { return &SessionWarning{} },
	KindSessionArchived: func() Payload { return &SessionArchived{} },
	KindSessionCarried:  func() Payload { return &SessionCarried{} },
	KindConfigLoaded:    func() Payload { return &ConfigLoaded{} },
	KindPreflightCheck:  func() Payload { return &PreflightCheck{} },
	KindSMUIntent:       func() Payload { return &SMUIntent{} },
	KindSMUWrite:        func() Payload { return &SMUWrite{} },
	KindSMUReadback:     func() Payload { return &SMUReadback{} },
	KindSMUError:        func() Payload { return &SMUError{} },
	KindProfileApplied:  func() Payload { return &ProfileApplied{} },
	KindProfileChange:   func() Payload { return &ProfileChange{} },
	KindProfileRestored: func() Payload { return &ProfileRestored{} },
	KindTrialIntent:     func() Payload { return &TrialIntent{} },
	KindTrialStart:      func() Payload { return &TrialStart{} },
	KindTrialProgress:   func() Payload { return &TrialProgress{} },
	KindTrialSignal:     func() Payload { return &TrialSignal{} },
	KindTrialSample:     func() Payload { return &TrialSample{} },
	KindTrialEnd:        func() Payload { return &TrialEnd{} },
	KindTrialCarried:    func() Payload { return &TrialCarried{} },
	KindFailure:         func() Payload { return &Failure{} },
	KindFailureCarried:  func() Payload { return &FailureCarried{} },
	KindMCE:             func() Payload { return &MCE{} },
	KindCrashDetected:   func() Payload { return &CrashDetected{} },
	KindTunerDecision:   func() Payload { return &TunerDecision{} },
	KindCorePhase:       func() Payload { return &CorePhase{} },
	KindGuardRotation:   func() Payload { return &GuardRotation{} },
	KindHostRanking:     func() Payload { return &HostRanking{} },
	KindHuntStart:       func() Payload { return &HuntStart{} },
	KindHuntMask:        func() Payload { return &HuntMask{} },
	KindHuntEnd:         func() Payload { return &HuntEnd{} },
	KindHuntSkipped:     func() Payload { return &HuntSkipped{} },
	KindMarkJoint:       func() Payload { return &MarkJoint{} },
	KindRefineRound:     func() Payload { return &RefineRound{} },
	KindTunerWarning:    func() Payload { return &TunerWarning{} },
	KindBackendRetry:    func() Payload { return &BackendRetry{} },
	KindCommandReset:    func() Payload { return &CommandReset{} },
	KindDefectFound:     func() Payload { return &DefectFound{} },
	KindDefectAnswered:  func() Payload { return &DefectAnswered{} },
	KindDeadEnd:         func() Payload { return &DeadEnd{} },
	KindBootSavedEntry:  func() Payload { return &BootSavedEntry{} },
	KindShutdown:        func() Payload { return &Shutdown{} },
	KindJournalTorn:     func() Payload { return &JournalTorn{} },
	KindStateRebuilt:    func() Payload { return &StateRebuilt{} },
}

func decodePayload(kind Kind, raw []byte) (Payload, error) {
	constructor, ok := payloadConstructors[kind]
	if !ok {
		return nil, nil
	}
	p := constructor()
	if err := json.Unmarshal(raw, p); err != nil {
		return nil, fmt.Errorf("decode %s: %w", kind, err)
	}
	return p, nil
}
