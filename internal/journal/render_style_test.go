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

	"github.com/google/go-cmp/cmp"
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
		{"candidate solo limit", &CorePhase{From: PhaseSearch, To: PhaseHasRoom}, GreenBold},
		{"at_limit", &CorePhase{From: PhaseHasRoom, To: PhaseAtLimit}, GreenBold},
		{"deepen", &TunerDecision{Decision: Deepen}, Green},
		{"passed cycle", &CheckingCycle{Event: CycleEnd, Passed: true, Full: true}, GreenBold},
		{"proven backoff", &TunerDecision{Decision: Backoff}, Yellow},
		{"yield", &TunerDecision{Decision: Yield}, Yellow},
		{"defect found", &DefectFound{}, Yellow},
		{"defect answered", &DefectAnswered{}, Plain},
		{"inconclusive trial", &TrialEnd{Outcome: OutcomeInconclusive}, Dim},
		{"single passed trial", &TrialEnd{Outcome: OutcomePass}, Plain},
		{"cycle start", &CheckingCycle{Event: CycleStart}, Plain},
		{"unpassed cycle", &CheckingCycle{Event: CycleEnd}, Plain},
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
		{"at_limit", &CorePhase{From: PhaseHasRoom, To: PhaseAtLimit}, "\x1b[1;32m"},
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
	if got := journald.PrefixedLine(failure, time.UTC, "  evidence: "); got != "<3>\x1b[31m  evidence: "+plain+"\x1b[0m" {
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

func TestFormatLineEscapesTerminalControls(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name string
		msg  string
		want string
	}{
		{"CSI", "\x1b[2J\x1b[31merror\x1b[0m", `\x1b[2J\x1b[31merror\x1b[0m`},
		{"OSC BEL", "\x1b]52;c;c2VjcmV0\x07", `\x1b]52;c;c2VjcmV0\x07`},
		{"OSC ST", "\x1b]0;fake title\x1b\\", `\x1b]0;fake title\x1b\`},
		{"line controls", "first\rsecond\nthird\tfourth", `first\rsecond\nthird\tfourth`},
		{"C0 and DEL", "\x00\x01\x08\x0b\x0c\x1f\x7f", `\x00\x01\x08\x0b\x0c\x1f\x7f`},
		{"Unicode C1", "\u0080\u0085\u009b2J\u009d52;c;text\u009c\u009f", `\u0080\u0085\u009b2J\u009d52;c;text\u009c\u009f`},
		{"raw C1 bytes", "\x9b2J\x9d52;c;text\x9c", `\x9b2J\x9d52;c;text\x9c`},
		{"Unicode separators", "before\u2028middle\u2029after", `before\u2028middle\u2029after`},
		{"bidi embeddings and overrides", "\u202a\u202b\u202c\u202d\u202e", `\u202a\u202b\u202c\u202d\u202e`},
		{"bidi isolates", "\u2066\u2067\u2068\u2069", `\u2066\u2067\u2068\u2069`},
		{"bidi marks", "\u061c\u200e\u200f", `\u061c\u200e\u200f`},
		{"readable Unicode", "71°C 日本語 café 🧪", "71°C 日本語 café 🧪"},
		{"visible escapes", `literal \n \x1b \u009b`, `literal \n \x1b \u009b`},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			event := Event{Time: time.Date(2026, 10, 2, 1, 14, 7, 0, time.UTC), Kind: KindTrialProgress, Msg: tt.msg}
			want := "01:14:07 trial.progress " + tt.want
			if d := cmp.Diff(want, FormatLine(event, time.UTC)); d != "" {
				t.Fatalf("line (-want +got): %s", d)
			}
		})
	}
}

func TestOpaqueEventHumanLinePreservesRawEvidence(t *testing.T) {
	t.Parallel()
	raw := []byte(`{"seq":2,"time":"2026-10-02T01:14:07Z","kind":"future.\u001b[2J","msg":"日本語\u001b]52;c;data\u0007\r\n","nested":{"value":42}}`)
	event, err := decode(raw)
	if err != nil {
		t.Fatal(err)
	}
	want := `01:14:07 future.\x1b[2J 日本語\x1b]52;c;data\x07\r\n`
	if got := (Renderer{color: true}).Line(event, time.UTC); got != want {
		t.Fatalf("opaque line = %q, want %q", got, want)
	}
	if event.Kind != "future.\x1b[2J" || event.Msg != "日本語\x1b]52;c;data\x07\r\n" || !bytes.Equal(event.Raw, raw) {
		t.Fatalf("human rendering changed raw evidence: %+v", event)
	}
}

func TestRendererEscapesBeforeApplicationStyle(t *testing.T) {
	t.Parallel()
	event := Event{
		Time: time.Date(2026, 10, 2, 1, 14, 7, 0, time.UTC),
		Kind: KindFailure, Data: &Failure{}, Msg: "bad\x1b[0m\r\n日本語",
	}
	plain := `01:14:07 failure        bad\x1b[0m\r\n日本語`
	for _, tt := range []struct {
		name     string
		renderer Renderer
		want     string
	}{
		{"plain", Renderer{}, plain},
		{"color", Renderer{color: true}, "\x1b[31m" + plain + "\x1b[0m"},
		{"journald color", Renderer{color: true, journald: true}, "<3>\x1b[31m" + plain + "\x1b[0m"},
		{"journald NO_COLOR", Renderer{journald: true}, "<3>" + plain},
	} {
		t.Run(tt.name, func(t *testing.T) {
			if got := tt.renderer.Line(event, time.UTC); got != tt.want {
				t.Fatalf("line = %q, want %q", got, tt.want)
			}
		})
	}
	renderer := Renderer{color: true}
	if got, want := renderer.PrefixedLine(event, time.UTC, "evidence: "), "\x1b[31mevidence: "+plain+"\x1b[0m"; got != want {
		t.Fatalf("shared evidence double escaped: %q, want %q", got, want)
	}
	if got, want := renderer.Text(event, "dead end: "+event.Msg), "\x1b[31m"+`dead end: bad\x1b[0m\r\n日本語`+"\x1b[0m"; got != want {
		t.Fatalf("summary = %q, want %q", got, want)
	}
	if got, want := renderer.Styled(RedBold, "refusal\x1b]0;title\x07"), "\x1b[1;31m"+`refusal\x1b]0;title\x07`+"\x1b[0m"; got != want {
		t.Fatalf("styled diagnostic = %q, want %q", got, want)
	}
}

func TestDiagnosticLogEscapesWithoutSanitizingJournal(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	var log bytes.Buffer
	j, err := Open(dir, Options{Boot: "boot", Now: fixedClock(), Log: &log})
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	if _, err := j.Append(sessionStart()); err != nil {
		t.Fatal(err)
	}
	detail := "日本語\x1b[2J\x1b]52;c;data\x07\r\n\u009b31m\u2028"
	event, err := j.Append(&TrialProgress{Trial: "0001", Detail: detail})
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(log.String(), `trial 0001 日本語\x1b[2J\x1b]52;c;data\x07\r\n\u009b31m\u2028`+"\n") {
		t.Fatalf("diagnostic not visibly escaped: %q", log.String())
	}
	events, torn, err := Read(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(torn) != 0 {
		t.Fatalf("unexpected torn journal: %q", torn)
	}
	recorded := events[len(events)-1]
	if recorded.Msg != "trial 0001 "+detail || recorded.Data.(*TrialProgress).Detail != detail {
		t.Fatalf("raw diagnostic changed: %+v", recorded)
	}
	if !bytes.Equal(recorded.Raw, event.Raw) {
		t.Fatalf("raw event changed: %q, want %q", recorded.Raw, event.Raw)
	}
}

func TestRendererClosedStreamFallsBackToPlain(t *testing.T) {
	file, err := os.CreateTemp(t.TempDir(), "log")
	if err != nil {
		t.Fatal(err)
	}
	if err := file.Close(); err != nil {
		t.Fatal(err)
	}
	renderer := NewRenderer(file, func(string) string { return "1" })
	e := Event{Time: time.Unix(0, 0), Kind: KindFailure, Data: &Failure{}, Msg: "failure"}
	if diff := cmp.Diff(FormatLine(e, time.UTC), renderer.Line(e, time.UTC)); diff != "" {
		t.Fatal(diff)
	}
}
