package main

import (
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"strings"

	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/hardware"
	"github.com/shgew/togi/internal/hostlock"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/render"
	"github.com/shgew/togi/internal/session"
)

const doctorHelp = `Usage: togi doctor

Check whether this machine is ready for togi run without starting a session:
run's preflight checks, the hardware watchdog and, when a session is recorded,
whether the BIOS context still matches it. Writes no journal event, changes no
offset and leaves GRUB alone. As root it makes every check and takes the
host lock, so a running run or reset stops it. Without root it checks the CPU,
the ryzen_smu driver identity, the backends, the backend user and the watchdog,
and marks the PM table, readback, slot mapping, systemd-run and BIOS context
checks skipped.

Prints one row per check, with ok, FAIL, warn or skipped, then ready, not fully
checked, or not ready with the failed checks: those run would turn into a
preflight dead end. A missing hardware watchdog is a warning, as in a run
started outside the tuning boot. A journal with a newer schema, ruleset or
evidence epoch, or with unknown event kinds, is refused as run refuses it.

Examples:
  togi doctor        Make the checks that need no root
  sudo togi doctor   Make every check run makes`

// diagnoseFunc is hardware.Diagnose: the run checks made, and those skipped with the reason.
type diagnoseFunc func(cfg config.Config, recorded *machine.BIOSContext, privileged bool) (ran, skipped []machine.Check, err error)

func runDoctor(g *globals, args []string, stdout, stderr io.Writer) int {
	return doctor(g, args, stdout, stderr, os.Geteuid() == 0, hardware.Diagnose)
}

func doctor(g *globals, args []string, stdout, stderr io.Writer, privileged bool, diagnose diagnoseFunc) int {
	flags := newFlagSet("doctor", g)
	if code, ok := parseFlags(flags, args, doctorHelp, stdout, stderr); !ok {
		return code
	}
	renderer := render.NewRenderer(stderr, os.Getenv)
	recorded, code, ok := doctorJournal(g.stateDir, stderr, renderer)
	if !ok {
		return code
	}
	cfg, _, err := loadConfig(g)
	if err != nil {
		fmt.Fprintf(stderr, "togi doctor: %v\n", err)
		return exitUsage
	}
	if privileged {
		lock, err := hostlock.Acquire(g.hostLockPath)
		if err != nil {
			fmt.Fprintf(stderr, "togi doctor: %v\n", err)
			if errors.Is(err, hostlock.ErrLocked) {
				return exitLocked
			}
			return exitError
		}
		defer lock.Close()
	}
	ran, skipped, err := diagnose(cfg, recorded, privileged)
	if err != nil {
		fmt.Fprintf(stderr, "togi doctor: %s\n", render.EscapeText(err.Error()))
		return exitError
	}
	failed := writeDoctor(stdout, ran, skipped)
	switch {
	case len(failed) > 0:
		line := "not ready: " + strings.Join(failed, ", ")
		fmt.Fprintln(stdout, line)
		fmt.Fprintln(stderr, renderer.Styled(render.RedBold, "togi doctor: "+line))
		return exitPreflight
	case !privileged:
		fmt.Fprintln(stdout, "not fully checked: run sudo togi doctor")
	default:
		fmt.Fprintln(stdout, "ready")
	}
	return exitOK
}

// doctorJournal makes run's startup checks on the journal and returns the BIOS
// context of the session run would continue, if one is recorded.
func doctorJournal(dir string, stderr io.Writer, renderer render.Renderer) (*machine.BIOSContext, int, bool) {
	build := session.Build()
	stamp, _, err := journal.Scan(dir)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return nil, exitOK, true
	case err != nil:
		fmt.Fprintf(stderr, "togi doctor: %s\n", render.EscapeText(err.Error()))
		return nil, exitError, false
	case stamp.Schema == 0 || journal.Older(stamp, build):
		// run archives an older journal and starts a new session.
		return nil, exitOK, true
	}
	if err := journal.Compatible(stamp, build); err != nil {
		fmt.Fprintln(stderr, renderer.Styled(render.RedBold, "togi doctor: "+err.Error()))
		return nil, exitIncompatible, false
	}
	events, _, err := journal.Read(dir)
	if err != nil {
		fmt.Fprintf(stderr, "togi doctor: %s\n", render.EscapeText(err.Error()))
		return nil, exitError, false
	}
	if err := journal.KnownKinds(events, build); err != nil {
		fmt.Fprintln(stderr, renderer.Styled(render.RedBold, "togi doctor: "+err.Error()))
		return nil, exitIncompatible, false
	}
	for _, e := range events {
		if c, ok := e.Data.(*journal.SessionContext); ok {
			return &c.BIOSContext, exitOK, true
		}
	}
	return nil, exitOK, true
}

// writeDoctor prints the check table and returns the names of the failed checks.
func writeDoctor(w io.Writer, ran, skipped []machine.Check) []string {
	tw := newTable(w)
	fmt.Fprintln(tw, "CHECK\tRESULT\tDETAIL")
	var failed []string
	for _, c := range ran {
		result := "ok"
		switch {
		case c.OK:
		case c.Name == "watchdog":
			result = "warn"
		default:
			result = "FAIL"
			failed = append(failed, c.Name)
		}
		fmt.Fprintf(tw, "%s\t%s\t%s\n", c.Name, result, render.EscapeText(c.Detail))
	}
	for _, c := range skipped {
		fmt.Fprintf(tw, "%s\tskipped\t%s\n", c.Name, render.EscapeText(c.Detail))
	}
	_ = tw.Flush()
	return failed
}
