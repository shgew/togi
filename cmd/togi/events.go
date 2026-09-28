package main

import (
	"errors"
	"flag"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/shgew/togi/internal/journal"
)

const eventsHelp = `Usage: togi events [--core <N>] [--kind <kinds>] [--trial <ID>] [--since <time>] [--until <time>] [--json]

Print the journal, one readable line per event, oldest first. Filters combine,
so you can narrow it to one core, one trial or a time window. A different
ruleset warns before rendering, including --json; a different schema is
refused. Readable lines are colored on terminals and in the system journal
unless NO_COLOR is set.

Examples:
  togi events --core 3                   Everything that happened to core 3
  togi events --kind trial,tier.change   Every trial event and tier change
  togi events --json --trial 0413        The raw events of trial 0413`

func runEvents(g *globals, args []string, stdout, stderr io.Writer) int {
	var (
		filter  journal.Filter
		rawJSON bool
	)
	flags := newFlagSet("events", g)
	flags.Func("core", "only events naming core `N`", coreFlag(&filter.Core))
	flags.Func("kind", "only these comma-separated `kinds`, or groups such as trial for every trial.* kind", func(s string) error {
		for k := range strings.SplitSeq(s, ",") {
			if k = strings.TrimSpace(k); k != "" {
				filter.Kinds = append(filter.Kinds, k)
			}
		}
		return nil
	})
	flags.StringVar(&filter.Trial, "trial", "", "only events of trial `ID`")
	flags.Func("since", "only events at or after this RFC 3339 `time`", timeFlag(&filter.Since))
	flags.Func("until", "only events before this RFC 3339 `time`", timeFlag(&filter.Until))
	flags.BoolVar(&rawJSON, "json", false, "print raw, uncolored JSON events")
	if code, ok := parseFlags(flags, args, eventsHelp, stdout, stderr); !ok {
		return code
	}

	events, torn, err := journal.Read(g.stateDir)
	if errors.Is(err, fs.ErrNotExist) {
		fmt.Fprintf(stderr, "togi events: no journal at %s\n", filepath.Join(g.stateDir, "events.jsonl"))
		return exitError
	}
	if incompatible, ok := errors.AsType[*journal.IncompatibleError](err); ok {
		fmt.Fprintln(stderr, journal.NewRenderer(stderr, os.Getenv).Styled(journal.RedBold, "togi events: "+incompatible.Error()))
		return exitError
	}
	if err != nil {
		fmt.Fprintf(stderr, "togi events: %v\n", err)
		return exitError
	}
	if len(events) > 0 {
		warnRuleset(events, stderr)
	}
	renderer := journal.NewRenderer(stdout, os.Getenv)
	for _, e := range events {
		if !filter.Match(e) {
			continue
		}
		if rawJSON {
			fmt.Fprintf(stdout, "%s\n", e.Raw)
		} else {
			fmt.Fprintln(stdout, renderer.Line(e, time.Local))
		}
	}
	if len(torn) > 0 {
		fmt.Fprintf(stderr, "togi events: journal ends with %d torn bytes; the next run records journal.torn\n", len(torn))
	}
	return exitOK
}

func timeFlag(t *time.Time) func(string) error {
	return func(s string) error {
		v, err := time.Parse(time.RFC3339, s)
		if err != nil {
			return errors.New("must be an RFC 3339 time")
		}
		*t = v
		return nil
	}
}

func newFlagSet(name string, g *globals) *flag.FlagSet {
	flags := flag.NewFlagSet(name, flag.ContinueOnError)
	flags.SetOutput(io.Discard)
	registerGlobals(flags, g)
	return flags
}

func parseFlags(flags *flag.FlagSet, args []string, help string, stdout, stderr io.Writer) (int, bool) {
	err := flags.Parse(args)
	if err == nil && flags.NArg() > 0 {
		err = fmt.Errorf("unexpected argument %q", flags.Arg(0))
	}
	if err == nil {
		return exitOK, true
	}
	if errors.Is(err, flag.ErrHelp) {
		commandUsage(flags, help, stdout)
		return exitOK, false
	}
	fmt.Fprintf(stderr, "togi %s: %v\n", flags.Name(), err)
	commandUsage(flags, help, stderr)
	return exitUsage, false
}

func commandUsage(flags *flag.FlagSet, help string, w io.Writer) {
	var b strings.Builder
	b.WriteString(help)
	b.WriteString("\n")
	writeFlags(&b, "Flags", flags, func(f *flag.Flag) bool { return !isGlobal(f) })
	writeFlags(&b, "Global flags", flags, isGlobal)
	_, _ = io.WriteString(w, b.String())
}
