package main

import (
	"errors"
	"fmt"
	"io"
	"io/fs"
	"maps"
	"path/filepath"
	"slices"
	"strconv"

	"github.com/shgew/togi/internal/detect"
	"github.com/shgew/togi/internal/hostlock"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/tuner"
)

const resetHelp = `Usage: togi reset --core <N> | --all

Reset one core, so the next run restarts its search from the baseline, or archive
the whole session, so the next run starts a new one that carries nothing from it.
Give exactly one of the two. --core clears its failed mark and every joint mark
that includes it. --all warns if a configured candidate edge reached a failed
mark in the archived session; missing or invalid configuration does not prevent
archiving.
--core refuses a different journal ruleset or schema; --all archives either.
Both forms refuse unknown event kinds; install the build that wrote them.
Only one run or reset can own this machine, even with different state directories.
A busy /run/lock/togi.lock stops reset before it changes the session.

Examples:
  sudo togi reset --core 3   Search core 3 again from its baseline
  sudo togi reset --all      Archive the session and start over`

func runReset(g *globals, args []string, stdout, stderr io.Writer) int {
	var (
		core *int
		all  bool
	)
	flags := newFlagSet("reset", g)
	flags.Func("core", "reset core `N`: clear its failed mark and every joint mark that includes it; restart its search from the baseline", coreFlag(&core))
	flags.BoolVar(&all, "all", false, "archive the session; the next run starts a new one")
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
	if events, _, readErr := journal.Read(g.stateDir); readErr == nil {
		if err := journal.KnownKinds(events, session.Build()); err != nil {
			fmt.Fprintf(stderr, "togi reset: %v\n", err)
			return exitError
		}
	}
	if all {
		dropped, dropErr := journal.DropPendingCarry(g.stateDir)
		if dropErr != nil {
			return resetError(dropErr, journal.ErrLocked, stderr)
		}
		stamp, id, err := journal.Scan(g.stateDir)
		if errors.Is(err, fs.ErrNotExist) {
			recovered, recoverErr := journal.RecoverPendingArchive(g.stateDir)
			if recoverErr != nil {
				return resetError(recoverErr, journal.ErrLocked, stderr)
			}
			if recovered != "" {
				fmt.Fprintf(stdout, "session %s archived to %s without appending to the incompatible journal; the next togi run starts a new session\n", recovered, filepath.Join("archive", recovered+".jsonl"))
				return exitOK
			}
			if dropped != "" {
				fmt.Fprintf(stdout, "carry from session %s dropped; the next togi run starts a new session with nothing carried\n", dropped)
				return exitOK
			}
		}
		if err == nil && stamp.Schema != journal.Schema {
			boot, bootErr := detect.BootID()
			if bootErr != nil {
				fmt.Fprintf(stderr, "togi reset: %v\n", bootErr)
				return exitError
			}
			j, openErr := journal.OpenForArchive(g.stateDir, journal.Options{Boot: boot, Sync: true})
			if openErr != nil {
				return resetError(openErr, journal.ErrLocked, stderr)
			}
			path, archiveErr := j.ArchiveUnreadable(id)
			if code, ok := closeCommand("reset", j, archiveErr, stderr); !ok {
				return code
			}
			fmt.Fprintf(stdout, "session %s archived to %s without appending to the incompatible journal; the next togi run starts a new session\n", id, path)
			return exitOK
		}
	}
	j, id, code, ok := openForCommand("reset", g.stateDir, stderr, all)
	if !ok {
		return code
	}
	if core != nil {
		err := session.ResetCore(j, *core)
		if code, ok := closeCommand("reset", j, err, stderr); !ok {
			return code
		}
		fmt.Fprintf(stdout, "reset of core %02d queued; the next togi run restarts its search from the baseline\n", *core)
		return exitOK
	}
	warnings := resetWarnings(j.Events(), g)
	path, err := session.ResetAll(j)
	if code, ok := closeCommand("reset", j, err, stderr); !ok {
		return code
	}
	fmt.Fprintf(stdout, "session %s archived to %s; the next togi run starts a new session\n", id, path)
	for _, warning := range warnings {
		fmt.Fprintln(stderr, warning)
	}
	return exitOK
}

func resetError(err, locked error, stderr io.Writer) int {
	fmt.Fprintf(stderr, "togi reset: %v\n", err)
	if errors.Is(err, locked) {
		return exitLocked
	}
	return exitError
}

func resetWarnings(events []journal.Event, g *globals) []string {
	cfg, _, err := loadConfig(g)
	if err != nil {
		if g.configSet {
			return []string{fmt.Sprintf("warning: cannot check candidate edges: %v", err)}
		}
		return nil
	}
	if len(cfg.CandidateEdges) == 0 {
		return nil
	}
	var state journal.State
	t := tuner.New()
	journal.Replay(events, &state, t)
	t.Project(&state)
	marks := make(map[int]int, len(state.Cores))
	for _, core := range state.Cores {
		if core.FailedMark != nil {
			marks[core.Core] = *core.FailedMark
		}
	}
	var warnings []string
	for _, core := range slices.Sorted(maps.Keys(cfg.CandidateEdges)) {
		edge := cfg.CandidateEdges[core]
		if mark, ok := marks[core]; ok && edge <= mark {
			remedy := "remove it"
			if mark < 0 {
				remedy = fmt.Sprintf("use %d, the failed mark plus one, or remove it", mark+1)
			}
			warnings = append(warnings, fmt.Sprintf("warning: candidate edge %d for core %02d is at or deeper than its failed mark %d in the archived session; %s", edge, core, mark, remedy))
		}
	}
	return warnings
}

// openForCommand refuses a directory without a session before it creates anything there.
func openForCommand(name, dir string, stderr io.Writer, allowRuleset bool) (*journal.Journal, string, int, bool) {
	if !allowRuleset {
		if stamp, _, scanErr := journal.Scan(dir); scanErr == nil && stamp.Schema != 0 {
			if err := journal.Compatible(stamp, session.Build()); err != nil {
				fmt.Fprintf(stderr, "togi %s: %v\n", name, err)
				return nil, "", exitError, false
			}
		}
	}
	events, _, err := journal.Read(dir)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		fmt.Fprintf(stderr, "togi %s: no journal at %s\n", name, filepath.Join(dir, "events.jsonl"))
		return nil, "", exitError, false
	case err != nil:
		fmt.Fprintf(stderr, "togi %s: %v\n", name, err)
		return nil, "", exitError, false
	case len(events) == 0:
		fmt.Fprintf(stderr, "togi %s: no session in %s\n", name, dir)
		return nil, "", exitError, false
	}
	if !allowRuleset {
		if err := journal.Compatible(journal.BuildOf(events), session.Build()); err != nil {
			fmt.Fprintf(stderr, "togi %s: %v\n", name, err)
			return nil, "", exitError, false
		}
	}
	boot, err := detect.BootID()
	if err != nil {
		fmt.Fprintf(stderr, "togi %s: %v\n", name, err)
		return nil, "", exitError, false
	}
	build := session.Build()
	if allowRuleset {
		build.Ruleset = 0
	}
	j, err := journal.Open(dir, journal.Options{Boot: boot, Sync: true, Log: stderr, Build: build})
	if err != nil {
		fmt.Fprintf(stderr, "togi %s: %v\n", name, err)
		if errors.Is(err, journal.ErrLocked) {
			return nil, "", exitLocked, false
		}
		return nil, "", exitError, false
	}
	return j, events[0].Data.(*journal.SessionStart).Session, exitOK, true
}

func closeCommand(name string, j *journal.Journal, err error, stderr io.Writer) (int, bool) {
	err = errors.Join(err, j.Close())
	if err == nil {
		return exitOK, true
	}
	fmt.Fprintf(stderr, "togi %s: %v\n", name, err)
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
