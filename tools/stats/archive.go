package main

import (
	"bufio"
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"time"

	"github.com/shgew/togi/internal/journal"
)

func readOlder(path string, original error) ([]journal.Event, error) {
	var incompatible *journal.IncompatibleError
	if !errors.As(original, &incompatible) || incompatible.Journal.Schema > journal.Schema {
		return nil, original
	}
	events, err := journal.ReadForCarry(path)
	if err != nil {
		return nil, err
	}
	carried := make(map[int]journal.Event, len(events))
	for _, e := range events {
		carried[e.Seq] = e
	}
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 4096), 4<<20)
	scanner.Split(func(data []byte, atEOF bool) (int, []byte, error) {
		if n := bytes.IndexByte(data, '\n'); n >= 0 {
			return n + 1, data[:n], nil
		}
		if atEOF {
			return len(data), nil, nil
		}
		return 0, nil, nil
	})
	events = nil
	for scanner.Scan() {
		var env struct {
			Seq   int          `json:"seq"`
			Time  time.Time    `json:"time"`
			Mono  int64        `json:"mono_ms"`
			Boot  string       `json:"boot"`
			Kind  journal.Kind `json:"kind"`
			Cause []int        `json:"cause"`
		}
		if err := json.Unmarshal(scanner.Bytes(), &env); err != nil {
			return nil, fmt.Errorf("read archive envelope: %w", err)
		}
		if e, ok := carried[env.Seq]; ok {
			events = append(events, e)
			continue
		}
		var p journal.Payload
		if constructor := archiveExtras[env.Kind]; constructor != nil {
			p = constructor()
		}
		if p != nil {
			if err := json.Unmarshal(scanner.Bytes(), p); err != nil {
				return nil, fmt.Errorf("read archive %s: %w", env.Kind, err)
			}
		}
		events = append(events, journal.Event{Seq: env.Seq, Time: env.Time, Mono: env.Mono, Boot: env.Boot, Kind: env.Kind, Cause: env.Cause, Data: p})
	}
	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("read archive: %w", err)
	}
	return events, nil
}

var archiveExtras = map[journal.Kind]func() journal.Payload{
	journal.KindTrialStart:     func() journal.Payload { return &journal.TrialStart{} },
	journal.KindTrialProgress:  func() journal.Payload { return &journal.TrialProgress{} },
	journal.KindTrialSignal:    func() journal.Payload { return &journal.TrialSignal{} },
	journal.KindTrialSample:    func() journal.Payload { return &journal.TrialSample{} },
	journal.KindCrashDetected:  func() journal.Payload { return &journal.CrashDetected{} },
	journal.KindGuardRotation:  func() journal.Payload { return &journal.GuardRotation{} },
	journal.KindProfileApplied: func() journal.Payload { return &journal.ProfileApplied{} },
	journal.KindSMUReadback:    func() journal.Payload { return &journal.SMUReadback{} },
	journal.KindHuntMask:       func() journal.Payload { return &journal.HuntMask{} },
	journal.KindTunerWarning:   func() journal.Payload { return &journal.TunerWarning{} },
}
