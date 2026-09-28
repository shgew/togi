package main

import (
	"bytes"
	"errors"
	"io"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/session"
)

func TestRunDeadEndEvidencePriority(t *testing.T) {
	t.Parallel()
	stderr, err := os.CreateTemp(t.TempDir(), "run-output")
	if err != nil {
		t.Fatal(err)
	}
	defer stderr.Close()
	failure := journal.Event{Kind: journal.KindFailure, Data: &journal.Failure{}, Time: time.Date(2026, 10, 2, 1, 14, 7, 0, time.UTC), Msg: "failure"}
	journalStream := journalStreamFor(t, stderr)
	renderer := journal.NewRenderer(stderr, func(k string) string {
		if k == "JOURNAL_STREAM" {
			return journalStream
		}
		return ""
	})
	stop := session.Stop{Reason: session.StopDeadEnd, DeadEnd: &journal.DeadEnd{Condition: journal.DeadEndSMU, Detail: "failed"}, Evidence: []journal.Event{failure}}
	if code := runResult(stop, nil, stderr, renderer); code != deadEndExit(stop.DeadEnd.Condition) {
		t.Fatalf("exit %d", code)
	}
	data, err := os.ReadFile(stderr.Name())
	if err != nil {
		t.Fatal(err)
	}
	if got := string(data); !strings.HasPrefix(got, "<3>\x1b[1;31mtogi: dead end smu: failed\x1b[0m\n") {
		t.Fatalf("summary not decorated as a red-bold priority-err line: %q", got)
	}
	if got := string(data); !strings.Contains(got, "\n<3>\x1b[31m  evidence: ") || !strings.Contains(got, "\x1b[0m\n") {
		t.Fatalf("evidence not decorated as a whole line: %q", got)
	}
}

func TestDefectDeadEndExit(t *testing.T) {
	var stderr bytes.Buffer
	stop := session.Stop{Reason: session.StopDeadEnd, DeadEnd: &journal.DeadEnd{Condition: journal.DeadEndDefect, Detail: "operator decision required"}}
	if code := runResult(stop, nil, &stderr, journal.Renderer{}); code != 17 || !strings.Contains(stderr.String(), "dead end defect") {
		t.Fatalf("exit %d, stderr %q", code, stderr.String())
	}
}

func TestParseDefectAnswer(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		line string
		err  error
		want bool
	}{
		{name: "yes", line: "Yes \n", want: true},
		{name: "short yes", line: "Y\n", want: true},
		{name: "no", line: "no\n"},
		{name: "other", line: "sure\n"},
		{name: "empty", line: "\n"},
		{name: "yes without newline at EOF", line: "yes", err: io.EOF},
		{name: "yes with read error", line: "yes\n", err: errors.New("read failed")},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := parseDefectAnswer(tc.line, tc.err); got != tc.want {
				t.Fatalf("parseDefectAnswer(%q, %v) = %t, want %t", tc.line, tc.err, got, tc.want)
			}
		})
	}
}

func TestDefectPromptRejectsCharacterDevicesThatAreNotTerminals(t *testing.T) {
	null, err := os.Open(os.DevNull)
	if err != nil {
		t.Fatal(err)
	}
	defer null.Close()
	stdin := os.Stdin
	os.Stdin = null
	defer func() { os.Stdin = stdin }()
	if prompt := defectPrompt(null); prompt != nil {
		t.Fatal("/dev/null must not be treated as a terminal")
	}
}

type clearingBootloader struct{ calls int }

func (b *clearingBootloader) ClearSavedEntry() (string, string, error) {
	b.calls++
	return "togi", "", nil
}

func TestCompatibilityRefusalClearsGRUBAndUsesErrPriority(t *testing.T) {
	stderr, err := os.CreateTemp(t.TempDir(), "refusal")
	if err != nil {
		t.Fatal(err)
	}
	defer stderr.Close()
	stream := journalStreamFor(t, stderr)
	renderer := journal.NewRenderer(stderr, func(key string) string {
		if key == "JOURNAL_STREAM" {
			return stream
		}
		return ""
	})
	bl := &clearingBootloader{}
	mismatch := &journal.IncompatibleError{Field: "ruleset", Journal: journal.Build{Version: "0.2.1", Rev: "def5678", Schema: 1, Ruleset: 99}, Binary: session.Build()}
	if code := runResult(session.Stop{}, mismatch, stderr, renderer, bl); code != exitIncompatible || bl.calls != 1 {
		t.Fatalf("exit %d; clear calls %d", code, bl.calls)
	}
	data, err := os.ReadFile(stderr.Name())
	if err != nil {
		t.Fatal(err)
	}
	if got := string(data); !strings.Contains(got, "<3>\x1b[1;31mtogi run: this journal was written by shycler 0.2.1+def5678") || !strings.Contains(got, "cleared GRUB saved entry") {
		t.Fatalf("refusal line and clear report: %q", got)
	}
}
