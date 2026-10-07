// stats is a read-only development program for reviewing tuning journals.
package main

import (
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"time"

	"github.com/shgew/togi/internal/facts"
)

func main() {
	if err := run(os.Args[1:], os.Stdout, os.Stderr); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(args []string, out, errOut io.Writer) error {
	flags := flag.NewFlagSet("stats", flag.ContinueOnError)
	flags.SetOutput(errOut)
	dir := flags.String("state-dir", "/var/lib/togi", "state directory containing events.jsonl")
	file := flags.String("journal", "", "one current or archived journal file")
	sinceArg := flags.String("since", "", "include activity starting at or after this RFC3339 time")
	forecastFile := flags.String("forecast", "", "score the run in --state-dir against this forecast `file` from tools/bench --forecast, instead of reporting; refuses a state directory whose journal through the forecast's anchor changed")
	if err := flags.Parse(args); err != nil {
		return err
	}
	if flags.NArg() != 0 {
		return fmt.Errorf("stats: unexpected arguments: %v", flags.Args())
	}
	if *forecastFile != "" {
		if *file != "" || *sinceArg != "" {
			return fmt.Errorf("stats: --forecast scores a whole --state-dir and cannot be combined with --journal or --since")
		}
		return scoreForecast(out, *dir, *forecastFile)
	}
	var since time.Time
	if *sinceArg != "" {
		var err error
		since, err = time.Parse(time.RFC3339, *sinceArg)
		if err != nil {
			return fmt.Errorf("stats: --since: %w", err)
		}
	}
	path := *file
	if path == "" {
		path = filepath.Join(*dir, "events.jsonl")
	}
	session, err := facts.ReadJournal(path)
	if err != nil {
		return err
	}
	return report(out, session, since)
}
