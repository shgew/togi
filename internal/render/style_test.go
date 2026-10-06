package render

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
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func fixedClock() func() time.Time {
	t := time.Date(2026, 10, 2, 1, 14, 7, 120000000, time.UTC)
	return func() time.Time {
		t = t.Add(time.Second)
		return t
	}
}

func sessionStart() *journal.SessionStart {
	return &journal.SessionStart{Schema: journal.Schema, Session: "20261002T011407Z", Cores: []machine.CoreInfo{{Core: 0, CCD: 0, CPUs: []int{0, 16}}, {Core: 7, CCD: 0, CPUs: []int{7, 23}}}}
}

func TestStyleOf(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name string
		data journal.Payload
		want Style
	}{
		{"failed trial", &journal.TrialEnd{Outcome: journal.OutcomeFailure}, Red},
		{"failure", &journal.Failure{}, Red},
		{"crash", &journal.CrashDetected{}, Red},
		{"dead end", &journal.DeadEnd{}, RedBold},
		{"passed search step", &journal.TunerDecision{Decision: journal.StepDeeper}, Green},
		{"candidate solo limit", &journal.CorePhase{From: journal.PhaseSearch, To: journal.PhaseHasRoom}, GreenBold},
		{"at_limit", &journal.CorePhase{From: journal.PhaseHasRoom, To: journal.PhaseAtLimit}, GreenBold},
		{"deepen", &journal.TunerDecision{Decision: journal.Deepen}, Green},
		{"passed cycle", &journal.CheckingCycle{Event: journal.CycleEnd, Passed: true, Full: true}, GreenBold},
		{"proven backoff", &journal.TunerDecision{Decision: journal.Backoff}, Yellow},
		{"yield", &journal.TunerDecision{Decision: journal.Yield}, Yellow},
		{"defect found", &journal.DefectFound{}, Yellow},
		{"defect answered", &journal.DefectAnswered{}, Plain},
		{"inconclusive trial", &journal.TrialEnd{Outcome: journal.OutcomeInconclusive}, Dim},
		{"single passed trial", &journal.TrialEnd{Outcome: journal.OutcomePass}, Plain},
		{"cycle start", &journal.CheckingCycle{Event: journal.CycleStart}, Plain},
		{"unpassed cycle", &journal.CheckingCycle{Event: journal.CycleEnd}, Plain},
		{"initial core phase", &journal.CorePhase{To: journal.PhaseSearch}, Plain},
		{"other event", &journal.SessionBaseline{}, Plain},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := StyleOf(journal.Event{Kind: tt.data.Kind(), Data: tt.data}); got != tt.want {
				t.Fatalf("style = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestPayloadStyles(t *testing.T) {
	t.Parallel()
	tests := []struct {
		p     journal.Payload
		style Style
	}{
		{&journal.HostRanking{Ranking: []int{3, 11}}, Plain},
		{&journal.HostRanking{Detail: "missing"}, Plain},
		{&journal.HuntStart{Hunt: 4, Regime: machine.R7, Trial: "0007", Candidates: []int{3, 11}, Trials: 5, TrialS: 120}, Yellow},
		{&journal.HuntGroup{Hunt: 4, Group: 1, Cores: []int{3, 11}, Skipped: true, Reason: "already checked"}, Plain},
		{&journal.HuntGroup{Hunt: 4, Group: 2, Cores: []int{3}, Inferred: "pass", Reason: "complement failed"}, Plain},
		{&journal.HuntGroup{Hunt: 4, Group: 3, Cores: []int{11}, Inferred: "failure", Reason: "complement passed"}, Plain},
		{&journal.HuntGroup{Hunt: 4, Group: 4, Cores: []int{3, 11}, DurationS: 120}, Plain},
		{&journal.HuntGroup{Hunt: 4, Group: 5, Cores: []int{3}, Probe: &journal.CombinationMember{Core: 11, Offset: -22}, Held: []journal.CombinationMember{{Core: 3, Offset: -40}}, DurationS: 120}, Plain},
		{&journal.HuntSkipped{Failure: 904, Reason: "failure point already known"}, Plain},
		{&journal.HuntEnd{Hunt: 3, Result: "culprit", Cores: []int{13}, Groups: 4}, Green},
		{&journal.Combination{Combination: 2, Members: []journal.CombinationMember{{Core: 3, Offset: -40}, {Core: 11, Offset: -30}}, Hunt: 4}, Yellow},
		{&journal.DeepeningRound{Round: 2, Event: journal.CycleEnd, Passed: true}, GreenBold},
		{&journal.TunerWarning{Warning: "monotonicity", Trial: "0520", Passes: []int{1, 2}}, Yellow},
		{&journal.BackendRetry{Backend: "mprime", Attempt: 2, WaitS: 300, Reason: "setup failed"}, Dim},
		{&journal.CheckingCycle{Event: journal.CycleEnd, Passed: true}, Plain},
		{&journal.Failure{Signal: machine.Crash, Attribution: journal.Attributed, Core: new(1), Offset: new(-38), Trial: "0385", Regime: machine.R7, Condition: machine.Parked}, Red},
		{&journal.Failure{Signal: machine.Crash, Attribution: journal.Attributed, Core: new(2), Offset: new(-12), Regime: machine.R6, Condition: machine.Together}, Red},
		{&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Trial: "0310", Regime: machine.R7, Condition: machine.Parked}, Red},
	}
	for _, tt := range tests {
		t.Run(string(tt.p.Kind()), func(t *testing.T) {
			if d := cmp.Diff(tt.style, StyleOf(journal.Event{Data: tt.p})); d != "" {
				t.Errorf("style (-want +got): %s", d)
			}
		})
	}
}

func TestRendererColors(t *testing.T) {
	t.Parallel()
	renderer := Renderer{color: true}
	tests := []struct {
		name string
		data journal.Payload
		sgr  string
	}{
		{"failure", &journal.Failure{}, "\x1b[31m"},
		{"dead end", &journal.DeadEnd{}, "\x1b[1;31m"},
		{"passed step", &journal.TunerDecision{Decision: journal.StepDeeper}, "\x1b[32m"},
		{"at_limit", &journal.CorePhase{From: journal.PhaseHasRoom, To: journal.PhaseAtLimit}, "\x1b[1;32m"},
		{"backoff", &journal.TunerDecision{Decision: journal.Backoff}, "\x1b[33m"},
		{"inconclusive", &journal.TrialEnd{Outcome: journal.OutcomeInconclusive}, "\x1b[2m"},
		{"trial pass", &journal.TrialEnd{Outcome: journal.OutcomePass}, ""},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			event := journal.Event{Kind: tt.data.Kind(), Data: tt.data, Time: time.Date(2026, 10, 2, 1, 14, 7, 0, time.UTC), Msg: tt.name}
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
	failure := journal.Event{Kind: journal.KindFailure, Data: &journal.Failure{}, Time: time.Date(2026, 10, 2, 1, 14, 7, 0, time.UTC), Msg: "failure"}
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
	if got := journald.Line(journal.Event{Kind: journal.KindTrialEnd, Data: &journal.TrialEnd{Outcome: journal.OutcomePass}, Msg: "pass", Time: failure.Time}, time.UTC); strings.HasPrefix(got, "<3>") || strings.ContainsRune(got, '\x1b') {
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
	j, err := journal.Open(dir, journal.Options{Boot: "boot", Now: fixedClock()})
	if err != nil {
		t.Fatal(err)
	}
	for _, p := range []journal.Payload{sessionStart(), &journal.Failure{}} {
		e, err := j.Append(p)
		if err != nil {
			t.Fatal(err)
		}
		renderer.Log(&log, e)
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
			event := journal.Event{Time: time.Date(2026, 10, 2, 1, 14, 7, 0, time.UTC), Kind: journal.KindTrialProgress, Msg: tt.msg}
			want := "01:14:07 trial.progress " + tt.want
			if d := cmp.Diff(want, FormatLine(event, time.UTC)); d != "" {
				t.Fatalf("line (-want +got): %s", d)
			}
		})
	}
}

func TestOpaqueEventHumanLinePreservesRawEvidence(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	raw := []byte(`{"seq":2,"time":"2026-10-02T01:14:07Z","kind":"future.\u001b[2J","msg":"日本語\u001b]52;c;data\u0007\r\n","nested":{"value":42}}`)
	start := fmt.Sprintf(`{"seq":1,"kind":"session.start","schema":%d}`, journal.Schema)
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte(start+"\n"+string(raw)+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	events, torn, err := journal.Read(dir)
	if err != nil || len(torn) != 0 || len(events) != 2 {
		t.Fatalf("valid opaque event was rejected by the reader: %d events, torn %q, %v", len(events), torn, err)
	}
	event := events[1]
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
	event := journal.Event{
		Time: time.Date(2026, 10, 2, 1, 14, 7, 0, time.UTC),
		Kind: journal.KindFailure, Data: &journal.Failure{}, Msg: "bad\x1b[0m\r\n日本語",
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
	j, err := journal.Open(dir, journal.Options{Boot: "boot", Now: fixedClock()})
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	if _, err := j.Append(sessionStart()); err != nil {
		t.Fatal(err)
	}
	detail := "日本語\x1b[2J\x1b]52;c;data\x07\r\n\u009b31m\u2028"
	event, err := j.Append(&journal.TrialProgress{Trial: "0001", Detail: detail})
	if err != nil {
		t.Fatal(err)
	}
	Renderer{}.Log(&log, event)
	if !strings.Contains(log.String(), `trial 0001 日本語\x1b[2J\x1b]52;c;data\x07\r\n\u009b31m\u2028`+"\n") {
		t.Fatalf("diagnostic not visibly escaped: %q", log.String())
	}
	events, torn, err := journal.Read(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(torn) != 0 {
		t.Fatalf("unexpected torn journal: %q", torn)
	}
	recorded := events[len(events)-1]
	if recorded.Msg != "trial 0001 "+detail || recorded.Data.(*journal.TrialProgress).Detail != detail {
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
	e := journal.Event{Time: time.Unix(0, 0), Kind: journal.KindFailure, Data: &journal.Failure{}, Msg: "failure"}
	if diff := cmp.Diff(FormatLine(e, time.UTC), renderer.Line(e, time.UTC)); diff != "" {
		t.Fatal(diff)
	}
}
