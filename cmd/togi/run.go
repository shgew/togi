package main

import (
	"bufio"
	"bytes"
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"io/fs"
	"os"
	"os/exec"
	"os/signal"
	"slices"
	"strconv"
	"strings"
	"syscall"
	"time"

	"golang.org/x/term"

	"github.com/shgew/togi/internal/carry"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/detect"
	"github.com/shgew/togi/internal/hardware"
	"github.com/shgew/togi/internal/hostlock"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/render"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/tuningboot"
)

const runHelp = `Usage: togi run [--cycles <N>] [--tuning-boot <grubenv>] [--no-tui]

Start or resume the tuning session in the foreground: search for each core's
solo limit, hunt the core or combination behind unattributed failures with
parked trials, deepen the profile to the most total depth its failure points
and combinations allow, then keep checking it with full cycles. After a crash,
the next run attributes it from the journal and continues. On resume, known defects
affecting past decisions name the cores; in a terminal run offers to reset them.
An unanswered too-aggressive defect stops an unattended run. It needs root.
A journal from an older ruleset, schema or evidence epoch, and no newer one,
is archived. The new session starts each core from its solo limit and failure point,
carrying eligible same-BIOS trial facts for solo-limit checks, hunts, reruns and
deepening. Passes carry only from the current evidence epoch; cycles still
require live passes. A newer ruleset, schema or evidence epoch stops the run
before another event is written; reset --all archives that session. An older
journal is archived even with unknown event kinds; carry uses only known events.
Unknown kinds in a current-ruleset, current-schema, current-epoch journal stop
both run and reset; install the build that wrote them. Journal lines are colored
on terminals and in the system journal unless NO_COLOR is set.

Only one run or reset can own this machine, even with different state directories.
A busy /run/lock/togi.lock stops the command before any hardware access or event.

When stdin and stderr are terminals, run shows the session as the watch
dashboard instead of one line per event, and prints the outcome when it stops:
the restored offsets and why it stopped, or the dead end or error;
events.jsonl still records every event. --no-tui prints the lines instead.

--cycles N stops after N clean cycles valid for the current
profile once every core is at its limit and deepening can reach no more depth. An
earlier cycle can count after a deepening if its profile was at least as deep
and no failure since the last reset contradicted it.

Without --tuning-boot, run checks the hardware watchdog once and records a
session.warning if none is active, then continues; the dashboard and togi events
show it. A freeze without reset protection needs a manual reset; start sessions
from the tuning boot, especially after a breaking update.

--tuning-boot runs the unattended service with an armed hardware watchdog.
The first durable journal append resets its consecutive restart-limit count and
records a pending leave reason once. Dead ends and incompatible or unknown-kind
journals persist a short leave reason before clearing the saved GRUB entry.

Examples:
  sudo togi run                     Tune this machine until a signal or a dead end
  sudo togi run --cycles 1            Stop after search finishes and one clean cycle passes
  sudo togi run --no-tui            Print one line per event instead of the dashboard`

func runFlags(g *globals, cycles *int, grubenv *string, noTUI *bool) *flag.FlagSet {
	flags := newFlagSet("run", g)
	flags.Func("cycles", "stop after `N` clean cycles valid for the current profile once every core is at its limit and deepening can reach no more depth (default endless)", func(s string) error {
		v, err := strconv.Atoi(s)
		if err != nil || v < 1 {
			return errors.New("must be a positive integer")
		}
		*cycles = v
		return nil
	})
	flags.StringVar(grubenv, "tuning-boot", "", "run as the tuning boot service: require an armed hardware watchdog within 30s; reset retry count on the first durable journal append; persist a leave reason before clearing saved_entry in this GRUB environment `file`, and reboot after a boot loop")
	flags.BoolVar(noTUI, "no-tui", false, "print one line per event instead of the dashboard on a terminal")
	return flags
}

func runRun(g *globals, args []string, stdout, stderr io.Writer) int {
	var (
		cycles  int
		grubenv string
		noTUI   bool
	)
	flags := runFlags(g, &cycles, &grubenv, &noTUI)
	if code, ok := parseFlags(flags, args, runHelp, stdout, stderr); !ok {
		return code
	}
	renderer := render.NewRenderer(stderr, os.Getenv)
	var bootloader session.Bootloader
	if grubenv != "" {
		bootloader = hardware.GRUB{Env: grubenv}
	}
	if stamp, _, scanErr := journal.Scan(g.stateDir); scanErr == nil {
		if stamp.Schema != 0 && !journal.Older(stamp, session.Build()) {
			if err := journal.Compatible(stamp, session.Build()); err != nil {
				return runStartupRefusal(g, err, stderr, renderer, bootloader)
			}
		}
	} else if !errors.Is(scanErr, fs.ErrNotExist) {
		fmt.Fprintf(stderr, "togi run: %s\n", render.EscapeText(scanErr.Error()))
		return exitError
	}
	if events, _, readErr := journal.Read(g.stateDir); readErr == nil && !journal.Older(journal.BuildOf(events), session.Build()) {
		if err := journal.KnownKinds(events, session.Build()); err != nil {
			return runStartupRefusal(g, err, stderr, renderer, bootloader)
		}
	}
	cfg, file, err := loadConfig(g)
	if err != nil {
		fmt.Fprintf(stderr, "togi run: %v\n", err)
		return exitUsage
	}
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM, syscall.SIGHUP)
	defer cancel()
	var dash *dashboard
	if out, ok := stderr.(*os.File); ok && !noTUI && interactive(out) {
		dash = &dashboard{dir: g.stateDir, out: out}
	}
	return runHardware(ctx, g, cfg, file, bootloader, cycles, stderr, renderer, dash, hardware.New)
}

func runStartupRefusal(g *globals, err error, stderr io.Writer, renderer render.Renderer, bootloader session.Bootloader) int {
	if bootloader != nil {
		lock, lockErr := hostlock.Acquire(g.hostLockPath)
		if lockErr != nil {
			fmt.Fprintf(stderr, "togi run: %v\n", lockErr)
			if errors.Is(lockErr, hostlock.ErrLocked) {
				return exitLocked
			}
			return exitError
		}
		defer lock.Close()
	}
	return runResult(session.Stop{}, err, stderr, renderer, bootloader)
}

func runHardware(ctx context.Context, g *globals, cfg config.Config, file bool, bootloader session.Bootloader, cycles int, stderr io.Writer, renderer render.Renderer, dash *dashboard, newMachine func(config.Config, string) (machine.Machine, error)) int {
	if err := hardware.CheckPlatform(); err != nil {
		fmt.Fprintf(stderr, "togi run: %v\n", err)
		return exitError
	}
	lock, err := hostlock.Acquire(g.hostLockPath)
	if err != nil {
		fmt.Fprintf(stderr, "togi run: %v\n", err)
		if errors.Is(err, hostlock.ErrLocked) {
			return exitLocked
		}
		return exitError
	}
	defer lock.Close()
	boot, err := detect.BootID()
	if err != nil {
		fmt.Fprintf(stderr, "togi run: %v\n", err)
		return exitError
	}
	m, err := newMachine(cfg, g.stateDir)
	if err != nil {
		fmt.Fprintf(stderr, "togi run: %v\n", err)
		return exitError
	}
	var current *machine.BIOSContext
	if m.Host.ValidateSMU() == nil {
		if bios, err := m.Host.BIOSContext(); err == nil {
			current = &bios
		}
	}
	log := stderr
	if dash != nil {
		log = dash
	}
	j, err := journal.Lock(g.stateDir, journal.Options{Boot: boot, Sync: true, Build: session.Build(), Monotonic: m.Clock.Monotonic})
	if err != nil {
		return runResult(session.Stop{}, err, stderr, renderer, bootloader)
	}
	defer j.Close()
	carried, err := carry.Prepare(j, session.Build(), nil, current)
	if err != nil {
		return runResult(session.Stop{}, err, stderr, renderer, bootloader)
	}
	prompt := defectPrompt(stderr)
	if dash != nil {
		dash.show()
		if ask := prompt; ask != nil {
			prompt = func(f defect.Finding) (bool, error) {
				dash.hide()
				defer dash.show()
				return ask(f)
			}
		}
	}
	sessionStderr := stderr
	var hidden bytes.Buffer
	if dash != nil {
		sessionStderr = &hidden
	}
	torn, err := j.Open()
	if err != nil {
		if dash != nil {
			dash.hide()
		}
		return runResult(session.Stop{}, err, stderr, renderer, bootloader)
	}
	renderer.Log(log, torn...)
	stop, err := session.Run(ctx, session.Input{Config: cfg, ConfigPath: g.config, ConfigFile: file, Boot: boot, Journal: j, Machine: m, Cycles: cycles, Bootloader: bootloader, Prompt: prompt, Carry: carried, Stderr: sessionStderr, Log: log, Renderer: renderer, Close: j.Close, SessionID: j.SessionID})
	if dash != nil {
		dash.hide()
		_, _ = hidden.WriteTo(stderr)
		if err == nil && stop.Reason != session.StopDeadEnd {
			printCleanStop(j.Events(), stderr, renderer)
		}
	}
	return runResult(stop, err, stderr, renderer, bootloader)
}

// printCleanStop repeats the closing profile.restored and shutdown lines the dashboard kept off the screen.
func printCleanStop(events []journal.Event, stderr io.Writer, renderer render.Renderer) {
	restored, shutdown := -1, -1
	for i, e := range slices.Backward(events) {
		if e.Kind == journal.KindSessionWarning {
			continue
		}
		if e.Kind == journal.KindProfileRestored {
			restored = i
			break
		}
		if e.Kind != journal.KindShutdown || shutdown >= 0 {
			break
		}
		shutdown = i
	}
	for _, i := range [2]int{restored, shutdown} {
		if i >= 0 {
			e := events[i]
			fmt.Fprintln(stderr, renderer.Line(e, time.Local))
		}
	}
}

func runResult(stop session.Stop, err error, stderr io.Writer, renderer render.Renderer, bootloader session.Bootloader) int {
	_, incompatible := errors.AsType[*journal.IncompatibleError](err)
	_, unknown := errors.AsType[*journal.UnknownKindError](err)
	if incompatible || unknown {
		fmt.Fprintln(stderr, renderer.Styled(render.RedBold, "togi run: "+err.Error()))
		if bootloader != nil {
			reasonText := "journal incompatible"
			if unknown {
				reasonText = "unknown event kind"
			}
			reason, reasonErr := tuningboot.NewReason(reasonText, 0)
			if reasonErr == nil {
				reasonErr = tuningboot.WriteReason(bootloader, reason)
			}
			if reasonErr != nil {
				fmt.Fprintf(stderr, "togi: persist GRUB leave reason: %v\n", reasonErr)
			}
			before, after, clearErr := bootloader.ClearSavedEntry()
			if clearErr != nil {
				fmt.Fprintf(stderr, "togi: clear GRUB saved entry: %v; no reboot requested\n", clearErr)
			} else {
				fmt.Fprintf(stderr, "togi: cleared GRUB saved entry from %q to %q; the next boot selects the normal system\n", before, after)
			}
		}
		return exitIncompatible
	}
	switch {
	case errors.Is(err, session.ErrNoSuchCore):
		fmt.Fprintf(stderr, "togi run: %v\n", err)
		return exitUsage
	case errors.Is(err, journal.ErrLocked):
		fmt.Fprintf(stderr, "togi run: %v\n", err)
		return exitLocked
	case err != nil:
		fmt.Fprintf(stderr, "togi run: %s\n", render.EscapeText(err.Error()))
		return exitError
	}
	switch stop.Reason {
	case session.StopSignal, session.StopCycles:
		return exitOK
	case session.StopDeadEnd:
		line := fmt.Sprintf("togi: dead end %s: %s", stop.DeadEnd.Condition, stop.DeadEnd.Detail)
		fmt.Fprintln(stderr, renderer.Text(journal.Event{Kind: journal.KindDeadEnd, Data: stop.DeadEnd}, line))
		for _, e := range stop.Evidence {
			fmt.Fprintln(stderr, renderer.PrefixedLine(e, time.Local, "  evidence: "))
		}
		if stop.Reboot {
			fmt.Fprintln(stderr, "togi: rebooting into the normal system")
			if out, err := rebootSystem(commandOutput); err != nil {
				fmt.Fprintf(stderr, "togi: systemctl reboot: %v: %s\n", err, strings.TrimSpace(string(out)))
			}
		}
		return deadEndExit(stop.DeadEnd.Condition)
	}
	return exitError
}

func rebootSystem(command func(context.Context, string, ...string) ([]byte, error)) ([]byte, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	return command(ctx, "systemctl", "reboot")
}

func commandOutput(ctx context.Context, name string, args ...string) ([]byte, error) {
	out, err := exec.CommandContext(ctx, name, args...).CombinedOutput()
	if ctx.Err() != nil {
		err = ctx.Err()
	}
	return out, err
}

func defectPrompt(stderr io.Writer) func(defect.Finding) (bool, error) {
	out, ok := stderr.(*os.File)
	if !ok || !interactive(out) {
		return nil
	}
	reader := bufio.NewReader(os.Stdin)
	return func(f defect.Finding) (bool, error) {
		fmt.Fprintf(stderr, "togi: defect %d: %s (fixed by pull request #%d); %s decisions %v affected cores %v\n", f.Entry.ID, f.Entry.Title, f.Entry.PR, f.Entry.Direction, f.Decisions, f.Cores)
		if f.Entry.Detail != "" {
			fmt.Fprintln(stderr, f.Entry.Detail)
		}
		fmt.Fprint(stderr, "Reset cores ")
		for i, core := range f.Cores {
			if i > 0 {
				fmt.Fprint(stderr, ", ")
			}
			fmt.Fprint(stderr, core)
		}
		fmt.Fprint(stderr, "? [y/N] ")
		line, err := reader.ReadString('\n')
		return parseDefectAnswer(line, err), nil
	}
}

func interactive(out *os.File) bool {
	return term.IsTerminal(int(os.Stdin.Fd())) && term.IsTerminal(int(out.Fd()))
}

func parseDefectAnswer(line string, err error) bool {
	if err != nil || !strings.HasSuffix(line, "\n") {
		return false
	}
	answer := strings.TrimSpace(line)
	return strings.EqualFold(answer, "y") || strings.EqualFold(answer, "yes")
}
