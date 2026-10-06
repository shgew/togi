package main

import (
	"errors"
	"flag"
	"fmt"
	"io"
	"io/fs"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"

	"github.com/shgew/togi/internal/detect"
	"github.com/shgew/togi/internal/hostlock"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/render"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/tuner"
)

const resetHelp = `Usage: togi reset --core <N> | --all

Reset one core, so the next run restarts its search from the baseline, or archive
the whole session, so the next run starts a new one that carries nothing from it.
Give exactly one of the two. --core clears its failure point and every combination
that includes it. --all warns if a configured candidate solo limit reached a failure
point in the archived session; missing or invalid configuration does not prevent
archiving.
--core refuses a different journal ruleset or schema; --all archives either.
Both forms refuse unknown event kinds; install the build that wrote them.
Only one run or reset can own this machine, even with different state directories.
A busy host lock stops reset before it changes the session.

Examples:
  sudo togi reset --core 3   Search core 3 again from its baseline
  sudo togi reset --all      Archive the session and start over`

func resetFlags(g *globals, core **int, all *bool) *flag.FlagSet {
	flags := newFlagSet("reset", g)
	flags.Func("core", "reset core `N`: clear its failure point and every combination that includes it; restart its search from the baseline", coreFlag(core))
	flags.BoolVar(all, "all", false, "archive the session; the next run starts a new one")
	return flags
}

func runReset(g *globals, args []string, stdout, stderr io.Writer) int {
	var (
		core *int
		all  bool
	)
	flags := resetFlags(g, &core, &all)
	if code, ok := parseFlags(flags, args, resetHelp, stdout, stderr); !ok {
		return code
	}
	if (core != nil) == all {
		fmt.Fprintln(stderr, "togi reset: exactly one of --core or --all")
		commandUsage(flags, resetHelp, stderr)
		return exitUsage
	}
	lock, err := hostlock.Acquire(g.hostLockPath)
	if err != nil {
		return resetError(err, hostlock.ErrLocked, stderr)
	}
	defer lock.Close()
	if _, err := os.Stat(filepath.Join(g.stateDir, "events.jsonl")); errors.Is(err, fs.ErrNotExist) {
		if !all || !pendingMarker(g.stateDir) {
			fmt.Fprintf(stderr, "togi reset: no journal at %s\n", filepath.Join(g.stateDir, "events.jsonl"))
			return exitError
		}
	}
	if events, _, err := journal.Read(g.stateDir); err == nil && len(events) == 0 {
		if !all || !pendingMarker(g.stateDir) {
			fmt.Fprintf(stderr, "togi reset: no session in %s\n", g.stateDir)
			return exitError
		}
	}
	boot, err := detect.BootID()
	if err != nil {
		return resetError(err, journal.ErrLocked, stderr)
	}
	build := session.Build()
	if all {
		build.Ruleset = 0
	}
	j, err := journal.Lock(g.stateDir, journal.Options{Boot: boot, Sync: true, Build: build})
	if err != nil {
		return resetError(err, journal.ErrLocked, stderr)
	}
	defer j.Close()
	if events, _, readErr := journal.Read(g.stateDir); readErr == nil {
		if err := journal.KnownKinds(events, session.Build()); err != nil {
			fmt.Fprintf(stderr, "togi reset: %s\n", render.EscapeText(err.Error()))
			return exitError
		}
	}
	if all {
		dropped, dropErr := j.DropPendingCarry()
		if dropErr != nil {
			return resetError(dropErr, journal.ErrLocked, stderr)
		}
		stamp, id, err := journal.Scan(g.stateDir)
		if errors.Is(err, fs.ErrNotExist) {
			if boundaryErr := j.MarkResetAll(); boundaryErr != nil {
				return resetError(boundaryErr, journal.ErrLocked, stderr)
			}
			recovered, recoverErr := j.RecoverPendingArchive()
			if recoverErr != nil {
				return resetError(recoverErr, journal.ErrLocked, stderr)
			}
			if recovered != "" {
				fmt.Fprintf(stdout, "session %s archived to %s without appending to the incompatible journal; the next togi run starts a new session\n", render.EscapeText(recovered), render.EscapeText(filepath.Join("archive", recovered+".jsonl")))
				return exitOK
			}
			if dropped != "" {
				fmt.Fprintf(stdout, "carry from session %s dropped; the next togi run starts a new session with nothing carried\n", render.EscapeText(dropped))
				return exitOK
			}
		}
		if err == nil && stamp.Schema != journal.Schema {
			if boundaryErr := j.MarkResetAll(); boundaryErr != nil {
				return resetError(boundaryErr, journal.ErrLocked, stderr)
			}
			path, archiveErr := j.ArchiveUnreadable(id)
			if code, ok := closeCommand("reset", j, archiveErr, stderr); !ok {
				return code
			}
			fmt.Fprintf(stdout, "session %s archived to %s without appending to the incompatible journal; the next togi run starts a new session\n", render.EscapeText(id), render.EscapeText(path))
			return exitOK
		}
	}
	id, code, ok := openForCommand("reset", j, stderr, all)
	if !ok {
		return code
	}
	opened := len(j.Events())
	if core != nil {
		err := session.ResetCore(j, *core)
		render.Renderer{}.Log(stderr, j.Events()[opened:]...)
		if code, ok := closeCommand("reset", j, err, stderr); !ok {
			return code
		}
		fmt.Fprintf(stdout, "reset of core %02d queued; the next togi run restarts its search from the baseline\n", *core)
		return exitOK
	}
	warnings := resetWarnings(j.Events(), g)
	path, err := session.ResetAll(j)
	render.Renderer{}.Log(stderr, j.Events()[opened:]...)
	if code, ok := closeCommand("reset", j, err, stderr); !ok {
		return code
	}
	fmt.Fprintf(stdout, "session %s archived to %s; the next togi run starts a new session\n", render.EscapeText(id), render.EscapeText(path))
	for _, warning := range warnings {
		fmt.Fprintln(stderr, warning)
	}
	return exitOK
}

// pendingMarker reports whether the archive in stateDir holds an unfinished archive or carry marker; an unreadable archive holds none.
func pendingMarker(stateDir string) bool {
	entries, err := os.ReadDir(filepath.Join(stateDir, "archive"))
	if err != nil {
		return false
	}
	return slices.ContainsFunc(entries, func(entry fs.DirEntry) bool { return strings.HasSuffix(entry.Name(), "-pending") })
}

func resetError(err, locked error, stderr io.Writer) int {
	fmt.Fprintf(stderr, "togi reset: %s\n", render.EscapeText(err.Error()))
	if errors.Is(err, locked) {
		return exitLocked
	}
	return exitError
}

func resetWarnings(events []journal.Event, g *globals) []string {
	cfg, _, err := loadConfig(g)
	if err != nil {
		if g.configSet {
			return []string{fmt.Sprintf("warning: cannot check candidate solo limits: %v", err)}
		}
		return nil
	}
	if len(cfg.CandidateSoloLimits) == 0 {
		return nil
	}
	var state journal.State
	t := tuner.New()
	journal.Replay(events, &state, t)
	t.Project(&state)
	failurePoints := make(map[int]int, len(state.Cores))
	for _, core := range state.Cores {
		if core.FailurePoint != nil {
			failurePoints[core.Core] = *core.FailurePoint
		}
	}
	var warnings []string
	for _, core := range slices.Sorted(maps.Keys(cfg.CandidateSoloLimits)) {
		soloLimit := cfg.CandidateSoloLimits[core]
		if failurePoint, ok := failurePoints[core]; ok && soloLimit <= failurePoint {
			remedy := "remove it"
			if failurePoint < 0 {
				remedy = fmt.Sprintf("use %d, the failure point plus one, or remove it", failurePoint+1)
			}
			warnings = append(warnings, fmt.Sprintf("warning: candidate solo limit %d for core %02d is at or deeper than its failure point %d in the archived session; %s", soloLimit, core, failurePoint, remedy))
		}
	}
	return warnings
}

func openForCommand(name string, j *journal.Journal, stderr io.Writer, allowRuleset bool) (string, int, bool) {
	dir := j.Dir()
	if !allowRuleset {
		if stamp, _, scanErr := journal.Scan(dir); scanErr == nil && stamp.Schema != 0 {
			if err := journal.Compatible(stamp, session.Build()); err != nil {
				fmt.Fprintf(stderr, "togi %s: %s\n", name, render.EscapeText(err.Error()))
				return "", exitError, false
			}
		}
	}
	events, _, err := journal.Read(dir)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		fmt.Fprintf(stderr, "togi %s: no journal at %s\n", name, filepath.Join(dir, "events.jsonl"))
		return "", exitError, false
	case err != nil:
		fmt.Fprintf(stderr, "togi %s: %s\n", name, render.EscapeText(err.Error()))
		return "", exitError, false
	case len(events) == 0:
		fmt.Fprintf(stderr, "togi %s: no session in %s\n", name, dir)
		return "", exitError, false
	}
	if !allowRuleset {
		if err := journal.Compatible(journal.BuildOf(events), session.Build()); err != nil {
			fmt.Fprintf(stderr, "togi %s: %s\n", name, render.EscapeText(err.Error()))
			return "", exitError, false
		}
	}
	torn, err := j.Open()
	if err != nil {
		fmt.Fprintf(stderr, "togi %s: %s\n", name, render.EscapeText(err.Error()))
		if errors.Is(err, journal.ErrLocked) {
			return "", exitLocked, false
		}
		return "", exitError, false
	}
	render.Renderer{}.Log(stderr, torn...)
	return events[0].Data.(*journal.SessionStart).Session, exitOK, true
}

func closeCommand(name string, j *journal.Journal, err error, stderr io.Writer) (int, bool) {
	err = errors.Join(err, j.Close())
	if err == nil {
		return exitOK, true
	}
	fmt.Fprintf(stderr, "togi %s: %s\n", name, render.EscapeText(err.Error()))
	if errors.Is(err, session.ErrNoSuchCore) {
		return exitUsage, false
	}
	return exitError, false
}

func coreFlag(dst **int) func(string) error {
	return func(s string) error {
		n, err := strconv.Atoi(s)
		if err != nil || n < 0 {
			return errors.New("must be a non-negative integer")
		}
		*dst = &n
		return nil
	}
}
