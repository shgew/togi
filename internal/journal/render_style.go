package journal

import (
	"fmt"
	"io"
	"os"
	"strconv"
	"strings"
	"syscall"
	"time"
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

func StyleOf(e Event) Style {
	switch p := e.Data.(type) {
	case *TrialEnd:
		switch p.Outcome {
		case OutcomeFailure:
			return Red
		case OutcomeInconclusive:
			return Dim
		case OutcomePass:
		}
	case *Failure, *CrashDetected:
		return Red
	case *DefectFound:
		return Yellow
	case *DeadEnd:
		return RedBold
	case *TunerDecision:
		switch p.Decision {
		case StepDeeper, Deepen:
			return Green
		case Backoff, Yield:
			return Yellow
		case CheckSoloLimit:
		}
	case *CorePhase:
		if p.To == PhaseAtLimit || p.To == PhaseHasRoom && p.From == PhaseSearch {
			return GreenBold
		}
	case *CheckingCycle:
		if p.Event == CycleEnd && p.Passed && p.Full {
			return GreenBold
		}
	case *HuntStart, *Combination, *TunerWarning, *SessionWarning:
		return Yellow
	case *HuntEnd:
		if p.Result == "culprit" || p.Result == "combination" || p.Result == "direct" {
			return Green
		}
	case *DeepeningRound:
		if p.Event == CycleEnd && p.Passed {
			return GreenBold
		}
	case *BackendRetry:
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

func (r Renderer) Line(e Event, loc *time.Location) string {
	return r.Text(e, FormatLine(e, loc))
}

func (r Renderer) Text(e Event, line string) string {
	return r.Styled(StyleOf(e), line)
}

// Styled escapes untrusted text before applying terminal and journald style.
func (r Renderer) Styled(style Style, line string) string {
	line = EscapeText(line)
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
