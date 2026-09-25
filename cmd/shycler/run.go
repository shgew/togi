package main

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"os/exec"
	"os/signal"
	"strconv"
	"strings"
	"syscall"
	"time"
	"unsafe"

	"code.marleb.org/shgew/shycler/internal/config"
	"code.marleb.org/shgew/shycler/internal/defect"
	"code.marleb.org/shgew/shycler/internal/detect"
	"code.marleb.org/shgew/shycler/internal/hardware"
	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/session"
)

const runHelp = `Usage: shycler run [--rotations <N>] [--tuning-boot <grubenv>]

Start or resume the tuning session in the foreground: search each core's deepest
stable offset, confirm it, then keep guarding all offsets together. After a crash,
the next run attributes it from the journal and continues. On resume, known
defects affecting past decisions name the cores; in a terminal run offers to reset
them. An unanswered too-aggressive defect stops an unattended run. It needs root.
A different journal ruleset or schema stops the run before another event is
written; reset --all archives that session. Journal lines are colored on
terminals and in the system journal unless NO_COLOR is set.

Examples:
  sudo shycler run                     Tune this machine until a signal or a dead end
  sudo shycler run --rotations 1       Stop after the first clean guard rotation`

func runRun(g *globals, args []string, stdout, stderr io.Writer) int {
	var (
		rotations int
		grubenv   string
	)
	flags := newFlagSet("run", g)
	flags.Func("rotations", "stop after `N` clean guard rotations of one profile (default endless)", func(s string) error {
		v, err := strconv.Atoi(s)
		if err != nil || v < 1 {
			return errors.New("must be a positive integer")
		}
		rotations = v
		return nil
	})
	flags.StringVar(&grubenv, "tuning-boot", "", "run as the tuning boot service: at a dead end clear saved_entry in this GRUB environment `file`, and reboot after a boot loop")
	if code, ok := parseFlags(flags, args, runHelp, stdout, stderr); !ok {
		return code
	}
	renderer := journal.NewRenderer(stderr, os.Getenv)
	if stamp, _, scanErr := journal.Scan(g.stateDir); scanErr == nil {
		if stamp.Schema != 0 {
			if err := journal.Compatible(stamp, session.Build()); err != nil {
				if grubenv != "" {
					return runResult(session.Stop{}, err, stderr, renderer, hardware.GRUB{Env: grubenv})
				}
				return runResult(session.Stop{}, err, stderr, renderer)
			}
		}
	} else if !errors.Is(scanErr, fs.ErrNotExist) {
		fmt.Fprintf(stderr, "shycler run: %v\n", scanErr)
		return exitError
	}
	cfg, file, err := loadConfig(g)
	if err != nil {
		fmt.Fprintf(stderr, "shycler run: %v\n", err)
		return exitUsage
	}
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM, syscall.SIGHUP)
	defer cancel()
	var bootloader session.Bootloader
	if grubenv != "" {
		bootloader = hardware.GRUB{Env: grubenv}
	}
	return runHardware(ctx, g, cfg, file, bootloader, rotations, stderr, renderer)
}

func runHardware(ctx context.Context, g *globals, cfg config.Config, file bool, bootloader session.Bootloader, rotations int, stderr io.Writer, renderer journal.Renderer) int {
	if err := hardware.CheckPlatform(); err != nil {
		fmt.Fprintf(stderr, "shycler run: %v\n", err)
		return exitError
	}
	boot, err := detect.BootID()
	if err != nil {
		fmt.Fprintf(stderr, "shycler run: %v\n", err)
		return exitError
	}
	j, err := journal.Open(g.stateDir, journal.Options{Boot: boot, Sync: true, Log: stderr, Renderer: renderer, Build: session.Build()})
	if err != nil {
		return runResult(session.Stop{}, err, stderr, renderer, bootloader)
	}
	if len(j.Events()) > 0 {
		if err := journal.Compatible(journal.BuildOf(j.Events()), session.Build()); err != nil {
			if cerr := j.Close(); cerr != nil {
				err = errors.Join(err, cerr)
			}
			return runResult(session.Stop{}, err, stderr, renderer, bootloader)
		}
	}
	m, err := hardware.New(cfg, g.stateDir)
	if err != nil {
		_ = j.Close()
		fmt.Fprintf(stderr, "shycler run: %v\n", err)
		return exitError
	}
	stop, err := session.Run(ctx, session.Input{Config: cfg, ConfigPath: g.config, ConfigFile: file, Boot: boot, Journal: j, Machine: m, Rotations: rotations, Bootloader: bootloader, Prompt: defectPrompt(stderr)})
	if cerr := j.Close(); err == nil && cerr != nil {
		err = cerr
	}
	return runResult(stop, err, stderr, renderer, bootloader)
}

func runResult(stop session.Stop, err error, stderr io.Writer, renderer journal.Renderer, bootloader ...session.Bootloader) int {
	if incompatible, ok := errors.AsType[*journal.IncompatibleError](err); ok {
		fmt.Fprintln(stderr, renderer.Styled(journal.RedBold, "shycler run: "+incompatible.Error()))
		if len(bootloader) > 0 && bootloader[0] != nil {
			before, after, clearErr := bootloader[0].ClearSavedEntry()
			if clearErr != nil {
				fmt.Fprintf(stderr, "shycler: clear GRUB saved entry: %v; no reboot requested\n", clearErr)
			} else {
				fmt.Fprintf(stderr, "shycler: cleared GRUB saved entry from %q to %q; the next boot selects the normal system\n", before, after)
			}
		}
		return exitIncompatible
	}
	switch {
	case errors.Is(err, session.ErrNoSuchCore):
		fmt.Fprintf(stderr, "shycler run: %v\n", err)
		return exitUsage
	case errors.Is(err, journal.ErrLocked):
		fmt.Fprintf(stderr, "shycler run: %v\n", err)
		return exitLocked
	case err != nil:
		fmt.Fprintf(stderr, "shycler run: %v\n", err)
		return exitError
	}
	switch stop.Reason {
	case session.StopSignal, session.StopRotations:
		return exitOK
	case session.StopDeadEnd:
		line := fmt.Sprintf("shycler: dead end %s: %s", stop.DeadEnd.Condition, stop.DeadEnd.Detail)
		fmt.Fprintln(stderr, renderer.Text(journal.Event{Kind: journal.KindDeadEnd, Data: stop.DeadEnd}, line))
		for _, e := range stop.Evidence {
			fmt.Fprintln(stderr, renderer.Text(e, "  evidence: "+journal.FormatLine(e, time.Local)))
		}
		if stop.Reboot {
			fmt.Fprintln(stderr, "shycler: rebooting into the normal system")
			if out, err := exec.Command("systemctl", "reboot").CombinedOutput(); err != nil {
				fmt.Fprintf(stderr, "shycler: systemctl reboot: %v: %s\n", err, strings.TrimSpace(string(out)))
			}
		}
		return deadEndExit(stop.DeadEnd.Condition)
	}
	return exitError
}

func defectPrompt(stderr io.Writer) func(defect.Finding) (bool, error) {
	out, ok := stderr.(*os.File)
	if !ok {
		return nil
	}
	isTerminal := func(file *os.File) bool {
		var termios syscall.Termios
		_, _, errno := syscall.Syscall(syscall.SYS_IOCTL, file.Fd(), getTermios, uintptr(unsafe.Pointer(&termios)))
		return errno == 0
	}
	if !isTerminal(os.Stdin) || !isTerminal(out) {
		return nil
	}
	reader := bufio.NewReader(os.Stdin)
	return func(f defect.Finding) (bool, error) {
		fmt.Fprintf(stderr, "shycler: defect %d: %s (fixed by pull request #%d); %s decisions %v affected cores %v\n", f.Entry.ID, f.Entry.Title, f.Entry.PR, f.Entry.Direction, f.Decisions, f.Cores)
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

func parseDefectAnswer(line string, err error) bool {
	if err != nil || !strings.HasSuffix(line, "\n") {
		return false
	}
	answer := strings.TrimSpace(line)
	return strings.EqualFold(answer, "y") || strings.EqualFold(answer, "yes")
}

func loadConfig(g *globals) (config.Config, bool, error) {
	cfg, err := config.Load(g.config)
	if err == nil {
		return cfg, true, nil
	}
	if !g.configSet && errors.Is(err, fs.ErrNotExist) {
		return config.Default(), false, nil
	}
	return config.Config{}, false, err
}
