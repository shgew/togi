// carry-facts prepares a transition in a temporary copy of recorded state, without hardware.
package main

import (
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strings"

	"github.com/shgew/togi/internal/carry"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/tuner"
)

func main() {
	if err := run(os.Args[1:], os.Stdout, os.Stderr); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(args []string, out, errOut io.Writer) (err error) {
	flags := flag.NewFlagSet("carry-facts", flag.ContinueOnError)
	flags.SetOutput(errOut)
	dirArg := flags.String("state-dir", "", "required temporary copy of the state directory; preparation mutates this copy")
	if err := flags.Parse(args); err != nil {
		return err
	}
	if *dirArg == "" || flags.NArg() != 0 {
		return fmt.Errorf("carry-facts: supply --state-dir with a temporary state copy and no positional arguments")
	}
	dir, err := filepath.EvalSymlinks(*dirArg)
	if err != nil {
		return fmt.Errorf("carry-facts: resolve state copy: %w", err)
	}
	dir, err = filepath.Abs(dir)
	if err != nil {
		return fmt.Errorf("carry-facts: resolve absolute state copy: %w", err)
	}
	if dir == "/var/lib/togi" || strings.HasPrefix(dir, "/var/lib/togi"+string(filepath.Separator)) {
		return fmt.Errorf("carry-facts: refusing the real state directory")
	}
	temp, err := filepath.EvalSymlinks(os.TempDir())
	if err != nil {
		return fmt.Errorf("carry-facts: resolve temporary directory: %w", err)
	}
	rel, err := filepath.Rel(temp, dir)
	if err != nil || rel == "." || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
		return fmt.Errorf("carry-facts: --state-dir must name a copy beneath %s", temp)
	}
	for _, name := range []string{"archive", "events.jsonl", "lock"} {
		info, err := os.Lstat(filepath.Join(dir, name))
		if os.IsNotExist(err) {
			continue
		}
		if err != nil {
			return fmt.Errorf("carry-facts: inspect %s: %w", name, err)
		}
		if info.Mode()&os.ModeSymlink != 0 {
			return fmt.Errorf("carry-facts: state copy must not symlink %s", name)
		}
	}
	build := journal.Build{Schema: journal.Schema, Ruleset: tuner.Ruleset + 1}
	j, err := journal.Lock(dir, journal.Options{Build: build})
	if err != nil {
		return fmt.Errorf("carry-facts: lock state copy: %w", err)
	}
	defer func() {
		if closeErr := j.Close(); err == nil && closeErr != nil {
			err = fmt.Errorf("carry-facts: close state copy: %w", closeErr)
		}
	}()
	live, err := facts.ReadJournal(filepath.Join(dir, "events.jsonl"))
	if err != nil {
		return fmt.Errorf("carry-facts: read live recorded facts: %w", err)
	}
	if live.Context == nil {
		return fmt.Errorf("carry-facts: live journal has no recorded BIOS context")
	}
	prepared, err := carry.Prepare(j, build, nil, live.Context)
	if err != nil {
		return fmt.Errorf("carry-facts: prepare transition: %w", err)
	}
	if prepared == nil {
		return fmt.Errorf("carry-facts: state copy produced no pending transition")
	}
	type counts struct{ passes, failures int }
	bySource := map[string]counts{}
	for _, fact := range prepared.Facts {
		count := bySource[fact.Session]
		switch fact.Outcome {
		case journal.OutcomePass:
			count.passes++
		case journal.OutcomeFailure:
			count.failures++
		case journal.OutcomeInconclusive:
			continue
		}
		bySource[fact.Session] = count
	}
	ids := make([]string, 0, len(bySource))
	for id := range bySource {
		ids = append(ids, id)
	}
	slices.SortFunc(ids, journal.CompareSessionIDs)
	if _, err := fmt.Fprintf(out, "evidence epoch %d; prepared ruleset %d transition in temporary copy\nSOURCE SESSION       PASSES FAILURES\n", tuner.EvidenceEpoch, build.Ruleset); err != nil {
		return err
	}
	for _, id := range ids {
		count := bySource[id]
		if _, err := fmt.Fprintf(out, "%s %6d %8d\n", id, count.passes, count.failures); err != nil {
			return err
		}
	}
	return nil
}
