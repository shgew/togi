package journal

import (
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
	case *DeadEnd:
		return RedBold
	case *TunerDecision:
		switch p.Decision {
		case StepDeeper:
			return Green
		case Backoff, SuspectBackoff:
			return Yellow
		}
	case *CorePhase:
		if p.From == PhaseRegain && p.To == PhaseConfirmed && p.Backoff {
			return Yellow
		}
		if p.To == PhaseConfirmation && p.From == PhaseSearch || p.To == PhaseConfirmed && (p.From == PhaseConfirmation || p.From == PhaseRegain) {
			return GreenBold
		}
	case *GuardRotation:
		if p.Event == RotationEnd && p.Clean {
			return GreenBold
		}
	case *TierChange:
		order := func(t Tier) int {
			switch t {
			case TierNone:
				return 0
			case TierBronze:
				return 1
			case TierSilver:
				return 2
			case TierGold:
				return 3
			case TierPlatinum:
				return 4
			}
			return 0
		}
		if order(p.To) > order(p.From) {
			return GreenBold
		}
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
			dev, devErr := strconv.ParseUint(device, 10, 64)
			ino, inoErr := strconv.ParseUint(inode, 10, 64)
			r.journald = devErr == nil && inoErr == nil && uint64(stat.Dev) == dev && stat.Ino == ino
		}
	}
	r.color = getenv("NO_COLOR") == "" && (info.Mode()&os.ModeCharDevice != 0 || r.journald)
	return r
}

func (r Renderer) Line(e Event, loc *time.Location) string {
	return r.Text(e, FormatLine(e, loc))
}

func (r Renderer) Text(e Event, line string) string {
	style := StyleOf(e)
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
