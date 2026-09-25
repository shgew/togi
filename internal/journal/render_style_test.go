package journal

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"
)

func TestStyleOf(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name string
		data Payload
		want Style
	}{
		{"failed trial", &TrialEnd{Outcome: OutcomeFailure}, Red},
		{"failure", &Failure{}, Red},
		{"crash", &CrashDetected{}, Red},
		{"dead end", &DeadEnd{}, RedBold},
		{"passed search step", &TunerDecision{Decision: StepDeeper}, Green},
		{"candidate edge", &CorePhase{From: PhaseSearch, To: PhaseConfirmation}, GreenBold},
		{"confirmed edge", &CorePhase{From: PhaseConfirmation, To: PhaseConfirmed}, GreenBold},
		{"regained edge", &CorePhase{From: PhaseRegain, To: PhaseConfirmed}, GreenBold},
		{"clean rotation", &GuardRotation{Event: RotationEnd, Clean: true}, GreenBold},
		{"bronze earned", &TierChange{From: TierNone, To: TierBronze}, GreenBold},
		{"silver earned", &TierChange{From: TierBronze, To: TierSilver}, GreenBold},
		{"gold earned", &TierChange{From: TierSilver, To: TierGold}, GreenBold},
		{"platinum earned", &TierChange{From: TierGold, To: TierPlatinum}, GreenBold},
		{"proven backoff", &TunerDecision{Decision: Backoff}, Yellow},
		{"suspect backoff", &TunerDecision{Decision: SuspectBackoff}, Yellow},
		{"inconclusive trial", &TrialEnd{Outcome: OutcomeInconclusive}, Dim},
		{"single passed trial", &TrialEnd{Outcome: OutcomePass}, Plain},
		{"tier drop", &TierChange{From: TierGold, To: TierNone}, Plain},
		{"unchanged tier", &TierChange{From: TierGold, To: TierGold}, Plain},
		{"rotation start", &GuardRotation{Event: RotationStart}, Plain},
		{"unclean rotation", &GuardRotation{Event: RotationEnd}, Plain},
		{"initial core phase", &CorePhase{To: PhaseSearch}, Plain},
		{"other event", &SessionBaseline{}, Plain},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := StyleOf(Event{Kind: tt.data.Kind(), Data: tt.data}); got != tt.want {
				t.Fatalf("style = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestRendererColors(t *testing.T) {
	t.Parallel()
	renderer := Renderer{color: true}
	tests := []struct {
		name string
		data Payload
		sgr  string
	}{
		{"failure", &Failure{}, "\x1b[31m"},
		{"dead end", &DeadEnd{}, "\x1b[1;31m"},
		{"passed step", &TunerDecision{Decision: StepDeeper}, "\x1b[32m"},
		{"confirmed", &CorePhase{From: PhaseConfirmation, To: PhaseConfirmed}, "\x1b[1;32m"},
		{"backoff", &TunerDecision{Decision: Backoff}, "\x1b[33m"},
		{"inconclusive", &TrialEnd{Outcome: OutcomeInconclusive}, "\x1b[2m"},
		{"trial pass", &TrialEnd{Outcome: OutcomePass}, ""},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			event := Event{Kind: tt.data.Kind(), Data: tt.data, Time: time.Date(2026, 10, 2, 1, 14, 7, 0, time.UTC), Msg: tt.name}
			want := FormatLine(event, time.UTC)
			if tt.sgr != "" {
				want = tt.sgr + want + "\x1b[0m"
			}
			if got := renderer.Line(event, time.UTC); got != want {
				t.Fatalf("line = %q, want %q", got, want)
			}
		})
	}
}

func TestRendererStreams(t *testing.T) {
	t.Parallel()
	device, err := os.OpenFile(os.DevNull, os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	defer device.Close()
	file, err := os.CreateTemp(t.TempDir(), "log")
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	fileStat, err := file.Stat()
	if err != nil {
		t.Fatal(err)
	}
	fileIdentity := fileStat.Sys().(*syscall.Stat_t)
	matched := fmt.Sprintf("%d:%d", fileIdentity.Dev, fileIdentity.Ino)
	mismatched := fmt.Sprintf("%d:%d", fileIdentity.Dev, fileIdentity.Ino+1)
	failure := Event{Kind: KindFailure, Data: &Failure{}, Time: time.Date(2026, 10, 2, 1, 14, 7, 0, time.UTC), Msg: "failure"}
	plain := FormatLine(failure, time.UTC)
	tests := []struct {
		name   string
		stream *os.File
		env    map[string]string
		want   string
	}{
		{"terminal", device, nil, "\x1b[31m" + plain + "\x1b[0m"},
		{"terminal with NO_COLOR", device, map[string]string{"NO_COLOR": "1"}, plain},
		{"terminal with inherited journal stream", device, map[string]string{"JOURNAL_STREAM": matched}, "\x1b[31m" + plain + "\x1b[0m"},
		{"file", file, nil, plain},
		{"file with NO_COLOR", file, map[string]string{"NO_COLOR": "1"}, plain},
		{"matching journal stream", file, map[string]string{"JOURNAL_STREAM": matched}, "<3>\x1b[31m" + plain + "\x1b[0m"},
		{"matching journal stream with NO_COLOR", file, map[string]string{"JOURNAL_STREAM": matched, "NO_COLOR": "1"}, "<3>" + plain},
		{"mismatched journal stream", file, map[string]string{"JOURNAL_STREAM": mismatched}, plain},
		{"mismatched journal stream with NO_COLOR", file, map[string]string{"JOURNAL_STREAM": mismatched, "NO_COLOR": "1"}, plain},
		{"malformed journal stream", file, map[string]string{"JOURNAL_STREAM": "1"}, plain},
		{"malformed journal stream with NO_COLOR", file, map[string]string{"JOURNAL_STREAM": "1", "NO_COLOR": "1"}, plain},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			renderer := NewRenderer(tt.stream, func(k string) string { return tt.env[k] })
			if got := renderer.Line(failure, time.UTC); got != tt.want {
				t.Fatalf("line = %q, want %q", got, tt.want)
			}
		})
	}
	journald := NewRenderer(file, func(k string) string { return map[string]string{"JOURNAL_STREAM": matched}[k] })
	if got := journald.Line(Event{Kind: KindTrialEnd, Data: &TrialEnd{Outcome: OutcomePass}, Msg: "pass", Time: failure.Time}, time.UTC); strings.HasPrefix(got, "<3>") || strings.ContainsRune(got, '\x1b') {
		t.Fatalf("plain line = %q", got)
	}
	if got := journald.Text(failure, "  evidence: "+plain); got != "<3>\x1b[31m  evidence: "+plain+"\x1b[0m" {
		t.Fatalf("evidence line = %q", got)
	}
	var buffer bytes.Buffer
	if got := NewRenderer(&buffer, func(k string) string { return map[string]string{"JOURNAL_STREAM": matched}[k] }).Line(failure, time.UTC); got != plain {
		t.Fatalf("non-file stream = %q, want %q", got, plain)
	}
}

func TestColoredLogDoesNotColorJournal(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	var log bytes.Buffer
	renderer := Renderer{color: true, journald: true}
	j, err := Open(dir, Options{Boot: "boot", Now: fixedClock(), Log: &log, Renderer: renderer})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := j.Append(sessionStart()); err != nil {
		t.Fatal(err)
	}
	if _, err := j.Append(&Failure{}); err != nil {
		t.Fatal(err)
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(log.String(), "<3>\x1b[31m") {
		t.Fatalf("failed event not styled in log: %q", log.String())
	}
	data, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	if bytes.ContainsRune(data, '\x1b') || bytes.Contains(data, []byte("<3>")) {
		t.Fatalf("journal contains log decoration: %q", data)
	}
}
