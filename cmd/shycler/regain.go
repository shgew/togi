package main

import (
	"errors"
	"fmt"
	"io"
	"io/fs"
	"path/filepath"
	"strconv"
	"strings"

	"code.marleb.org/shgew/shycler/internal/detect"
	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/session"
)

const (
	regainHelp = `Usage: shycler regain [--core <N>]

Queue one count of regain on every confirmed core with unproven depth, or on
core N only. Unproven depth is what a suspect backoff gave up without proof; the
next run confirms one count deeper on each queued core. A different journal
ruleset or schema is refused without queuing anything.

Examples:
  sudo shycler regain            Every core with unproven depth
  sudo shycler regain --core 3   Core 3 only`
	resetHelp = `Usage: shycler reset --core <N> | --all

Reset one core, so the next run restarts its search from the baseline, or archive
the whole session, so the next run starts a new one. Give exactly one of the two.
--core refuses a different journal ruleset or schema; --all archives either.

Examples:
  sudo shycler reset --core 3   Search core 3 again from its baseline
  sudo shycler reset --all      Archive the session and start over`
)

func runRegain(g *globals, args []string, stdout, stderr io.Writer) int {
	var core *int
	flags := newFlagSet("regain", g)
	flags.Func("core", "only core `N`", coreFlag(&core))
	if code, ok := parseFlags(flags, args, regainHelp, stdout, stderr); !ok {
		return code
	}
	j, _, code, ok := openForCommand("regain", g.stateDir, stderr, false)
	if !ok {
		return code
	}
	cores, err := session.Regain(j, core)
	if code, ok := closeCommand("regain", j, err, stderr); !ok {
		return code
	}
	ids := make([]string, len(cores))
	for i, c := range cores {
		ids[i] = fmt.Sprintf("%02d", c)
	}
	fmt.Fprintf(stdout, "regain queued for cores %s; the next shycler run confirms one count deeper on each\n", strings.Join(ids, ", "))
	return exitOK
}

func runReset(g *globals, args []string, stdout, stderr io.Writer) int {
	var (
		core *int
		all  bool
	)
	flags := newFlagSet("reset", g)
	flags.Func("core", "reset core `N`: its search restarts from the baseline", coreFlag(&core))
	flags.BoolVar(&all, "all", false, "archive the session; the next run starts a new one")
	if code, ok := parseFlags(flags, args, resetHelp, stdout, stderr); !ok {
		return code
	}
	if (core != nil) == all {
		fmt.Fprintln(stderr, "shycler reset: exactly one of --core or --all")
		commandUsage(flags, resetHelp, stderr)
		return exitUsage
	}
	if all {
		stamp, id, err := journal.Scan(g.stateDir)
		if errors.Is(err, fs.ErrNotExist) {
			recovered, recoverErr := journal.RecoverPendingArchive(g.stateDir)
			if recoverErr != nil {
				fmt.Fprintf(stderr, "shycler reset: %v\n", recoverErr)
				if errors.Is(recoverErr, journal.ErrLocked) {
					return exitLocked
				}
				return exitError
			}
			if recovered != "" {
				fmt.Fprintf(stdout, "session %s archived to %s without appending to the incompatible journal; the next shycler run starts a new session\n", recovered, filepath.Join("archive", recovered+".jsonl"))
				return exitOK
			}
		}
		if err == nil && stamp.Schema != journal.Schema {
			boot, bootErr := detect.BootID()
			if bootErr != nil {
				fmt.Fprintf(stderr, "shycler reset: %v\n", bootErr)
				return exitError
			}
			j, openErr := journal.OpenForArchive(g.stateDir, journal.Options{Boot: boot, Sync: true})
			if openErr != nil {
				fmt.Fprintf(stderr, "shycler reset: %v\n", openErr)
				if errors.Is(openErr, journal.ErrLocked) {
					return exitLocked
				}
				return exitError
			}
			path, archiveErr := j.ArchiveUnreadable(id)
			if code, ok := closeCommand("reset", j, archiveErr, stderr); !ok {
				return code
			}
			fmt.Fprintf(stdout, "session %s archived to %s without appending to the incompatible journal; the next shycler run starts a new session\n", id, path)
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
		fmt.Fprintf(stdout, "reset of core %02d queued; the next shycler run restarts its search from the baseline\n", *core)
		return exitOK
	}
	path, err := session.ResetAll(j)
	if code, ok := closeCommand("reset", j, err, stderr); !ok {
		return code
	}
	fmt.Fprintf(stdout, "session %s archived to %s; the next shycler run starts a new session\n", id, path)
	return exitOK
}

// openForCommand refuses a directory without a session before it creates anything there.
func openForCommand(name, dir string, stderr io.Writer, allowRuleset bool) (*journal.Journal, string, int, bool) {
	if !allowRuleset {
		if stamp, _, scanErr := journal.Scan(dir); scanErr == nil && stamp.Schema != 0 {
			if err := journal.Compatible(stamp, session.Build()); err != nil {
				fmt.Fprintf(stderr, "shycler %s: %v\n", name, err)
				return nil, "", exitError, false
			}
		}
	}
	events, _, err := journal.Read(dir)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		fmt.Fprintf(stderr, "shycler %s: no journal at %s\n", name, filepath.Join(dir, "events.jsonl"))
		return nil, "", exitError, false
	case err != nil:
		fmt.Fprintf(stderr, "shycler %s: %v\n", name, err)
		return nil, "", exitError, false
	case len(events) == 0:
		fmt.Fprintf(stderr, "shycler %s: no session in %s\n", name, dir)
		return nil, "", exitError, false
	}
	if !allowRuleset {
		if err := journal.Compatible(journal.BuildOf(events), session.Build()); err != nil {
			fmt.Fprintf(stderr, "shycler %s: %v\n", name, err)
			return nil, "", exitError, false
		}
	}
	boot, err := detect.BootID()
	if err != nil {
		fmt.Fprintf(stderr, "shycler %s: %v\n", name, err)
		return nil, "", exitError, false
	}
	build := session.Build()
	if allowRuleset {
		build.Ruleset = 0
	}
	j, err := journal.Open(dir, journal.Options{Boot: boot, Sync: true, Log: stderr, Build: build})
	if err != nil {
		fmt.Fprintf(stderr, "shycler %s: %v\n", name, err)
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
	fmt.Fprintf(stderr, "shycler %s: %v\n", name, err)
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
