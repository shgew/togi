package main

import (
	"bufio"
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"maps"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/render"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/tuningboot"
)

func TestRunFlagsCycleLimit(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		args []string
		want int
	}{
		{name: "endless by default"},
		{name: "one cycle", args: []string{"--cycles", "1"}, want: 1},
		{name: "multiple cycles", args: []string{"--cycles=3"}, want: 3},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			var g globals
			var cycles int
			var grubenv string
			var noTUI bool
			flags := runFlags(&g, &cycles, &grubenv, &noTUI)
			var stdout, stderr bytes.Buffer
			if code, ok := parseFlags(flags, tc.args, runHelp, &stdout, &stderr); !ok || code != exitOK {
				t.Fatalf("parse: exit %d, ok %v, stderr %q", code, ok, stderr.String())
			}
			if diff := cmp.Diff(tc.want, cycles); diff != "" {
				t.Fatalf("cycle limit (-want +got): %s", diff)
			}
		})
	}
}

func TestRunDeadEndEvidencePriority(t *testing.T) {
	t.Parallel()
	stderr, err := os.CreateTemp(t.TempDir(), "run-output")
	if err != nil {
		t.Fatal(err)
	}
	defer stderr.Close()
	failure := journal.Event{Kind: journal.KindFailure, Data: &journal.Failure{}, Time: time.Date(2026, 10, 2, 1, 14, 7, 0, time.UTC), Msg: "failure"}
	journalStream := journalStreamFor(t, stderr)
	renderer := render.NewRenderer(stderr, func(k string) string {
		if k == "JOURNAL_STREAM" {
			return journalStream
		}
		return ""
	})
	stop := session.Stop{Reason: session.StopDeadEnd, DeadEnd: &journal.DeadEnd{Condition: journal.DeadEndSMU, Detail: "failed"}, Evidence: []journal.Event{failure}}
	if code := runResult(stop, nil, stderr, renderer, nil); code != deadEndExit(stop.DeadEnd.Condition) {
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
	if prompt := defectPrompt(context.Background(), null); prompt != nil {
		t.Fatal("/dev/null must not be treated as a terminal")
	}
}

func TestDefectPromptAnswersOnlyInputTypedAfterTheQuestion(t *testing.T) {
	t.Parallel()
	pr, pw := io.Pipe()
	var out bytes.Buffer
	ask := askDefect(context.Background(), &out, bufio.NewReader(pr), func() { out.WriteString("discarded\n") })
	go fmt.Fprint(pw, "y\n")
	yes, err := ask(defect.Finding{Entry: defect.Entry{ID: 2, Title: "t"}, Cores: []int{3}})
	if err != nil || !yes {
		t.Fatalf("answer %t, %v", yes, err)
	}
	if got := out.String(); !strings.HasPrefix(got, "discarded\ntogi: defect 2:") || !strings.HasSuffix(got, "Reset cores 3? [y/N] ") {
		t.Fatalf("prompt output %q: want pending input discarded before the question", got)
	}
}

func TestDefectPromptReturnsWhenRunIsStopped(t *testing.T) {
	t.Parallel()
	ctx, stop := context.WithCancel(context.Background())
	stop()
	pr, _ := io.Pipe()
	ask := askDefect(ctx, io.Discard, bufio.NewReader(pr), func() {})
	yes, err := ask(defect.Finding{Cores: []int{3}})
	if yes || !errors.Is(err, context.Canceled) {
		t.Fatalf("answer %t, %v; want no answer and context.Canceled with nothing typed", yes, err)
	}
}

type clearingBootloader struct {
	calls            int
	values           map[string]string
	environmentCalls []string
	setErr           error
}

func (b *clearingBootloader) Get(name string) (string, error) {
	b.environmentCalls = append(b.environmentCalls, "get "+name)
	return b.values[name], nil
}

func (b *clearingBootloader) Set(values map[string]string) error {
	b.environmentCalls = append(b.environmentCalls, "set")
	if b.setErr != nil {
		return b.setErr
	}
	if b.values == nil {
		b.values = make(map[string]string)
	}
	maps.Copy(b.values, values)
	return nil
}

func (b *clearingBootloader) Unset(names ...string) error {
	b.environmentCalls = append(b.environmentCalls, "unset "+strings.Join(names, " "))
	for _, name := range names {
		delete(b.values, name)
	}
	return nil
}

func (b *clearingBootloader) ClearSavedEntry() (string, string, error) {
	b.calls++
	b.environmentCalls = append(b.environmentCalls, "clear saved_entry")
	return "togi", "", nil
}

func TestCompatibilityRefusalClearsGRUBAndUsesErrPriority(t *testing.T) {
	stderr, err := os.CreateTemp(t.TempDir(), "refusal")
	if err != nil {
		t.Fatal(err)
	}
	defer stderr.Close()
	stream := journalStreamFor(t, stderr)
	renderer := render.NewRenderer(stderr, func(key string) string {
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
	if got := string(data); !strings.HasPrefix(got, "<3>\x1b[1;31m") || !strings.Contains(got, "cleared GRUB saved entry") {
		t.Fatalf("refusal line and clear report: %q", got)
	}
}

func TestPrintCleanStop(t *testing.T) {
	t.Parallel()
	const restoredLine = "01:14:07 profile.restored restored offsets [-10 -20]\n"
	const shutdownLine = "01:14:07 shutdown       clean shutdown\n"
	for _, tc := range []struct {
		name  string
		kinds []journal.Kind
		want  string
	}{
		{name: "ordinary restore", kinds: []journal.Kind{journal.KindSMUReadback, journal.KindProfileRestored, journal.KindShutdown}, want: restoredLine + shutdownLine},
		{name: "interleaved and trailing warnings", kinds: []journal.Kind{journal.KindSMUReadback, journal.KindProfileRestored, journal.KindSessionWarning, journal.KindShutdown, journal.KindSessionWarning, journal.KindSessionWarning}, want: restoredLine + shutdownLine},
		{name: "later work excludes earlier restore", kinds: []journal.Kind{journal.KindProfileRestored, journal.KindSessionWarning, journal.KindProfileChange, journal.KindShutdown, journal.KindSessionWarning}, want: shutdownLine},
		{name: "new run excludes previous closing lines", kinds: []journal.Kind{journal.KindProfileRestored, journal.KindShutdown, journal.KindConfigLoaded, journal.KindSessionWarning}},
		{name: "previous shutdown bounds the closing suffix", kinds: []journal.Kind{journal.KindProfileRestored, journal.KindShutdown, journal.KindSessionWarning, journal.KindShutdown, journal.KindSessionWarning}, want: shutdownLine},
		{name: "shutdown without restore", kinds: []journal.Kind{journal.KindShutdown, journal.KindSessionWarning}, want: shutdownLine},
		{name: "restore without shutdown", kinds: []journal.Kind{journal.KindProfileRestored, journal.KindSessionWarning}, want: restoredLine},
		{name: "warnings only", kinds: []journal.Kind{journal.KindSessionWarning, journal.KindSessionWarning}},
		{name: "empty"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var events []journal.Event
			for _, kind := range tc.kinds {
				msg := "not a closing summary"
				if kind == journal.KindProfileRestored {
					msg = "restored offsets [-10 -20]"
				}
				if kind == journal.KindShutdown {
					msg = "clean shutdown"
				}
				events = append(events, journal.Event{Kind: kind, Time: time.Date(2026, 10, 2, 1, 14, 7, 0, time.Local), Msg: msg})
			}
			var stderr bytes.Buffer
			printCleanStop(events, &stderr, render.Renderer{})
			if diff := cmp.Diff(tc.want, stderr.String()); diff != "" {
				t.Fatalf("closing summary (-want +got):\n%s", diff)
			}
		})
	}
}

func TestRunRejectsInvalidCycles(t *testing.T) {
	t.Parallel()
	for _, value := range []string{"0", "-1", "many"} {
		t.Run(value, func(t *testing.T) {
			g := testGlobals(t)
			var out, diagnostics bytes.Buffer
			code := runRun(&g, []string{"--cycles", value}, &out, &diagnostics)
			if code != exitUsage || out.Len() != 0 {
				t.Fatalf("exit %d, stdout %q", code, out.String())
			}
			firstLine, _, _ := strings.Cut(diagnostics.String(), "\n")
			want := fmt.Sprintf("togi run: invalid value %q for flag --cycles: must be a positive integer", value)
			if diff := cmp.Diff(want, firstLine); diff != "" {
				t.Fatalf("cycle diagnostic (-want +got): %s", diff)
			}
			if diff := cmp.Diff(map[string]string{}, directoryFiles(t, g.stateDir)); diff != "" {
				t.Fatalf("invalid cycles changed state: %s", diff)
			}
		})
	}
}

func TestRunRejectsOldCyclesFlag(t *testing.T) {
	t.Parallel()
	g := testGlobals(t)
	var out, diagnostics bytes.Buffer
	code := runRun(&g, []string{"--laps", "1"}, &out, &diagnostics)
	if code != exitUsage || out.Len() != 0 {
		t.Fatalf("exit %d, stdout %q", code, out.String())
	}
	if !strings.Contains(diagnostics.String(), "flag provided but not defined: --laps") {
		t.Fatalf("old flag diagnostic: %q", diagnostics.String())
	}
	if diff := cmp.Diff(map[string]string{}, directoryFiles(t, g.stateDir)); diff != "" {
		t.Fatalf("old flag changed state: %s", diff)
	}
}

func TestRunResultExitCodes(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		stop session.Stop
		err  error
		code int
		want string
	}{
		{"signal", session.Stop{Reason: session.StopSignal}, nil, 0, ""},
		{"cycles", session.Stop{Reason: session.StopCycles}, nil, 0, ""},
		{"missing-core", session.Stop{}, session.ErrNoSuchCore, 2, "togi run: no such core\n"},
		{"journal-locked", session.Stop{}, journal.ErrLocked, 3, "togi run: another togi process holds the journal lock\n"},
		{"ordinary-error", session.Stop{}, errors.New("read failed\x1b[2J\nforged"), 1, "togi run: read failed\\x1b[2J\\nforged\n"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var out bytes.Buffer
			if code := runResult(tc.stop, tc.err, &out, render.Renderer{}, nil); code != tc.code {
				t.Fatalf("exit %d, want %d", code, tc.code)
			}
			if diff := cmp.Diff(tc.want, out.String()); diff != "" {
				t.Fatalf("run outcome (-want +got): %s", diff)
			}
		})
	}
	for _, tc := range []struct {
		condition journal.DeadEndCondition
		code      int
	}{
		{journal.DeadEndFailureAtZero, 10},
		{journal.DeadEndSMU, 11},
		{journal.DeadEndNoEvidence, 12},
		{journal.DeadEndBootLoop, 13},
		{journal.DeadEndContainment, 14},
		{journal.DeadEndPreflight, 15},
		{journal.DeadEndDefect, 17},
		{journal.DeadEndThermalTrip, 18},
		{"unknown", 1},
	} {
		t.Run(string(tc.condition), func(t *testing.T) {
			var out bytes.Buffer
			stop := session.Stop{Reason: session.StopDeadEnd, DeadEnd: &journal.DeadEnd{Condition: tc.condition, Detail: "operator intervention required"}}
			if code := runResult(stop, nil, &out, render.Renderer{}, nil); code != tc.code {
				t.Fatalf("exit %d, want %d", code, tc.code)
			}
			want := fmt.Sprintf("togi: dead end %s: operator intervention required\n", tc.condition)
			if diff := cmp.Diff(want, out.String()); diff != "" {
				t.Fatalf("dead-end diagnostic (-want +got): %s", diff)
			}
		})
	}
}

func TestRunMalformedJournalRefusal(t *testing.T) {
	t.Parallel()
	g := testGlobals(t)
	before := "not json\n"
	if err := os.WriteFile(filepath.Join(g.stateDir, "events.jsonl"), []byte(before), 0o600); err != nil {
		t.Fatal(err)
	}
	var out, diagnostics bytes.Buffer
	code := runRun(&g, nil, &out, &diagnostics)
	if code != exitError || out.Len() != 0 {
		t.Fatalf("exit %d, stdout %q", code, out.String())
	}
	want := "togi run: read session.start stamp: invalid character 'o' in literal null (expecting 'u')\n"
	if diff := cmp.Diff(want, diagnostics.String()); diff != "" {
		t.Fatalf("journal refusal (-want +got): %s", diff)
	}
	got, err := os.ReadFile(filepath.Join(g.stateDir, "events.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(before, string(got)); diff != "" {
		t.Fatalf("refusal modified journal: %s", diff)
	}
}

type failedClearBootloader struct{ clearingBootloader }

func (b *failedClearBootloader) ClearSavedEntry() (string, string, error) {
	b.calls++
	b.environmentCalls = append(b.environmentCalls, "clear saved_entry")
	return "togi", "togi", errors.New("saved entry is read-only")
}

func TestCompatibilityRefusalReportsFailedClear(t *testing.T) {
	t.Parallel()
	var out bytes.Buffer
	err := &journal.IncompatibleError{Field: "ruleset", Journal: journal.Build{Ruleset: 99}, Binary: session.Build()}
	if code := runResult(session.Stop{}, err, &out, render.Renderer{}, &failedClearBootloader{}); code != exitIncompatible {
		t.Fatalf("exit %d", code)
	}
	want := "togi run: " + err.Error() + "\ntogi: clear GRUB saved entry: saved entry is read-only; no reboot requested\n"
	if diff := cmp.Diff(want, out.String()); diff != "" {
		t.Fatalf("failed clear refusal (-want +got): %s", diff)
	}
}

func TestRunRefusalPersistsFixedReasonBeforeClearing(t *testing.T) {
	t.Parallel()
	for _, tt := range []struct {
		name, reason string
		err          error
	}{
		{"incompatible", "journal incompatible", &journal.IncompatibleError{Field: "ruleset", Journal: journal.Build{Ruleset: 99}, Binary: session.Build()}},
		{"unknown kind", "unknown event kind", &journal.UnknownKindError{}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			bootloader := &clearingBootloader{}
			var diagnostics bytes.Buffer
			code := runResult(session.Stop{}, tt.err, &diagnostics, render.Renderer{}, bootloader)
			if code != exitIncompatible {
				t.Fatalf("refusal exit %d; diagnostics %s", code, diagnostics.String())
			}
			if diff := cmp.Diff([]string{"set", "clear saved_entry"}, bootloader.environmentCalls); diff != "" {
				t.Fatalf("refusal order (-want +got): %s", diff)
			}
			reason, err := tuningboot.ReadReason(bootloader)
			if err != nil || reason == nil || reason.Count != 0 || reason.Reason != tt.reason {
				t.Fatalf("pending refusal reason %+v, %v", reason, err)
			}
		})
	}
}

func TestRunRefusalStillClearsAfterReasonWriteFailure(t *testing.T) {
	t.Parallel()
	for _, failedClear := range []bool{false, true} {
		t.Run(fmt.Sprintf("clear failure %v", failedClear), func(t *testing.T) {
			fake := &clearingBootloader{setErr: errors.New("reason is read-only")}
			var bootloader session.Bootloader = fake
			if failedClear {
				bootloader = &failedClearBootloader{clearingBootloader: *fake}
				fake = &bootloader.(*failedClearBootloader).clearingBootloader
			}
			var diagnostics bytes.Buffer
			refusal := &journal.UnknownKindError{}
			if code := runResult(session.Stop{}, refusal, &diagnostics, render.Renderer{}, bootloader); code != exitIncompatible {
				t.Fatalf("changed refusal exit: %d", code)
			}
			if diff := cmp.Diff([]string{"set", "clear saved_entry"}, fake.environmentCalls); diff != "" {
				t.Fatalf("failure skipped clear (-want +got): %s", diff)
			}
			if !strings.Contains(diagnostics.String(), "persist GRUB leave reason: write leave reason: reason is read-only") {
				t.Fatalf("reason failure hidden: %s", diagnostics.String())
			}
			wantClear := "cleared GRUB saved entry"
			if failedClear {
				wantClear = "clear GRUB saved entry: saved entry is read-only"
			}
			if !strings.Contains(diagnostics.String(), wantClear) || strings.Contains(diagnostics.String(), "journal-write") {
				t.Fatalf("clear result or error classification: %s", diagnostics.String())
			}
		})
	}
}
