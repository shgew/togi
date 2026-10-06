package render

import (
	"fmt"
	"io"
	"os"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/shgew/togi/internal/journal"
)

type Style uint8

const (
	Plain Style = iota
	Red
	RedBold
	Green
	GreenBold
	Yellow
	Dim
)

func StyleOf(e journal.Event) Style {
	switch p := e.Data.(type) {
	case *journal.TrialEnd:
		switch p.Outcome {
		case journal.OutcomeFailure:
			return Red
		case journal.OutcomeInconclusive:
			return Dim
		case journal.OutcomePass:
		}
	case *journal.Failure, *journal.CrashDetected:
		return Red
	case *journal.DefectFound:
		return Yellow
	case *journal.DeadEnd:
		return RedBold
	case *journal.TunerDecision:
		switch p.Decision {
		case journal.StepDeeper, journal.Deepen:
			return Green
		case journal.Backoff, journal.Yield:
			return Yellow
		case journal.CheckSoloLimit:
		}
	case *journal.CorePhase:
		if p.To == journal.PhaseAtLimit || p.To == journal.PhaseHasRoom && p.From == journal.PhaseSearch {
			return GreenBold
		}
	case *journal.CheckingCycle:
		if p.Event == journal.CycleEnd && p.Passed && p.Full {
			return GreenBold
		}
	case *journal.HuntStart, *journal.Combination, *journal.TunerWarning, *journal.SessionWarning:
		return Yellow
	case *journal.HuntEnd:
		if p.Result == "culprit" || p.Result == "combination" || p.Result == "direct" {
			return Green
		}
	case *journal.DeepeningRound:
		if p.Event == journal.CycleEnd && p.Passed {
			return GreenBold
		}
	case *journal.BackendRetry:
		return Dim
	}
	return Plain
}

type Renderer struct {
	color    bool
	journald bool
}

func NewRenderer(stream io.Writer, getenv func(string) string) Renderer {
	var r Renderer
	f, ok := stream.(*os.File)
	if !ok {
		return r
	}
	info, err := f.Stat()
	if err != nil {
		return r
	}
	if stat, ok := info.Sys().(*syscall.Stat_t); ok {
		device, inode, paired := strings.Cut(getenv("JOURNAL_STREAM"), ":")
		if paired {
			ino, inoErr := strconv.ParseUint(inode, 10, 64)
			r.journald = device == fmt.Sprint(stat.Dev) && inoErr == nil && stat.Ino == ino
		}
	}
	r.color = getenv("NO_COLOR") == "" && (info.Mode()&os.ModeCharDevice != 0 || r.journald)
	return r
}

func (r Renderer) Line(e journal.Event, loc *time.Location) string {
	return r.styled(StyleOf(e), FormatLine(e, loc))
}

// Log writes the line of each persisted event to the run log w, in local time; a nil w discards them.
func (r Renderer) Log(w io.Writer, events ...journal.Event) {
	if w == nil {
		return
	}
	for _, e := range events {
		fmt.Fprintln(w, r.Line(e, time.Local))
	}
}

func (r Renderer) PrefixedLine(e journal.Event, loc *time.Location, prefix string) string {
	return r.styled(StyleOf(e), EscapeText(prefix)+FormatLine(e, loc))
}

func (r Renderer) Text(e journal.Event, line string) string {
	return r.Styled(StyleOf(e), line)
}

// Styled escapes untrusted text before applying terminal and journald style.
func (r Renderer) Styled(style Style, line string) string {
	return r.styled(style, EscapeText(line))
}

func (r Renderer) styled(style Style, line string) string {
	if r.color {
		var sgr string
		switch style {
		case Plain:
		case Red:
			sgr = "\x1b[31m"
		case RedBold:
			sgr = "\x1b[1;31m"
		case Green:
			sgr = "\x1b[32m"
		case GreenBold:
			sgr = "\x1b[1;32m"
		case Yellow:
			sgr = "\x1b[33m"
		case Dim:
			sgr = "\x1b[2m"
		}
		if sgr != "" {
			line = sgr + line + "\x1b[0m"
		}
	}
	if r.journald && (style == Red || style == RedBold) {
		return "<3>" + line
	}
	return line
}
