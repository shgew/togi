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
	"github.com/shgew/togi/internal/render"
)

const eventsHelp = `Usage: togi events [--core <N>] [--kind <kinds>] [--trial <ID>] [--since <time>] [--until <time>] [--json]

Print the journal, one readable line per event, oldest first. Filters combine,
so you can narrow it to one core, one trial or a time window. A different
ruleset warns before rendering, including --json; a different schema is
refused. Kind selectors must name a known exact kind or group; unknown names
and explicitly empty lists are usage errors (exit 2). Valid filters with no
matches print nothing and exit 0. Readable lines are colored on terminals and
in the system journal unless NO_COLOR is set.

Examples:
  togi events --core 3                     Everything that happened to core 3
  togi events --kind trial,checking.cycle   Every trial event and checking cycle
  togi events --json --trial 0413          The raw events of trial 0413`

func eventsFlags(g *globals, filter *journal.Filter, rawJSON *bool) *flag.FlagSet {
	flags := newFlagSet("events", g)
	flags.Func("core", "only events naming core `N`", coreFlag(&filter.Core))
	flags.Func("kind", "only these comma-separated known `kinds`, or groups such as trial for every trial.* kind", func(s string) error {
		start := len(filter.Kinds)
		for k := range strings.SplitSeq(s, ",") {
			if k = strings.TrimSpace(k); k != "" {
				if err := journal.ValidateKindSelector(k); err != nil {
					return err
				}
				filter.Kinds = append(filter.Kinds, k)
			}
		}
		if len(filter.Kinds) == start {
			return journal.ValidateKindSelector("")
		}
		return nil
	})
	flags.StringVar(&filter.Trial, "trial", "", "only events of trial `ID`")
	flags.Func("since", "only events at or after this RFC 3339 `time`", timeFlag(&filter.Since))
	flags.Func("until", "only events before this RFC 3339 `time`", timeFlag(&filter.Until))
	flags.BoolVar(rawJSON, "json", false, "print raw, uncolored JSON events")
	return flags
}

func runEvents(g *globals, args []string, stdout, stderr io.Writer) int {
	var (
		filter  journal.Filter
		rawJSON bool
	)
	flags := eventsFlags(g, &filter, &rawJSON)
	if code, ok := parseFlags(flags, args, eventsHelp, stdout, stderr); !ok {
		return code
	}

	events, torn, err := journal.Read(g.stateDir)
	if errors.Is(err, fs.ErrNotExist) {
		fmt.Fprintf(stderr, "togi events: no journal at %s\n", filepath.Join(g.stateDir, "events.jsonl"))
		return exitError
	}
	if incompatible, ok := errors.AsType[*journal.IncompatibleError](err); ok {
		fmt.Fprintln(stderr, render.NewRenderer(stderr, os.Getenv).Styled(render.RedBold, "togi events: "+incompatible.Error()))
		return exitError
	}
	if err != nil {
		fmt.Fprintf(stderr, "togi events: %s\n", render.EscapeText(err.Error()))
		return exitError
	}
	if len(events) > 0 {
		warnRuleset(events, stderr)
	}
	renderer := render.NewRenderer(stdout, os.Getenv)
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
