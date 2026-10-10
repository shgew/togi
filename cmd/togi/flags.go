package main

import (
	"errors"
	"flag"
	"fmt"
	"io"
	"io/fs"
	"slices"
	"strconv"
	"strings"

	"github.com/shgew/togi/internal/config"
)

var configCommands = []string{"doctor", "run", "reset"}

func acceptsConfig(name string) bool {
	return slices.Contains(configCommands, name)
}

func newFlagSet(name string, g *globals) *flag.FlagSet {
	flags := flag.NewFlagSet(name, flag.ContinueOnError)
	flags.SetOutput(io.Discard)
	registerGlobals(flags, g)
	return flags
}

func registerGlobals(fs *flag.FlagSet, g *globals) {
	if fs.Name() == "togi" || acceptsConfig(fs.Name()) {
		fs.Func("config", "configuration file `path` for doctor, run and reset (default "+config.DefaultPath+")", func(s string) error {
			g.config, g.configSet = s, true
			return nil
		})
	}
	fs.Func("state-dir", "state directory `path` (default "+defaultStateDir+")", func(s string) error {
		g.stateDir = s
		return nil
	})
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
	return flagError(flags, help, err, stderr), false
}

// errConfigScope is the usage error for --config on a command that reads no configuration.
var errConfigScope = errors.New("--config applies only to " + strings.Join(configCommands[:len(configCommands)-1], ", ") + " and " + configCommands[len(configCommands)-1])

// flagMessage words a flag package error the way the usage does: flags with two dashes.
// Only the flag token the package itself adds is reworded; a quoted submitted value and the
// cause after it stay as they were. An undefined --config on a command without one says
// where --config applies.
func flagMessage(err error) string {
	msg := err.Error()
	if msg == "flag provided but not defined: -config" {
		return errConfigScope.Error()
	}
	for _, prefix := range []string{"flag provided but not defined: ", "flag needs an argument: "} {
		if rest, ok := strings.CutPrefix(msg, prefix); ok {
			return prefix + "-" + rest
		}
	}
	for _, form := range []struct{ lead, join string }{
		{"invalid value ", " for flag "},
		{"invalid boolean value ", " for "},
	} {
		rest, ok := strings.CutPrefix(msg, form.lead)
		if !ok {
			continue
		}
		quoted, qerr := strconv.QuotedPrefix(rest)
		if qerr != nil {
			break
		}
		if tail, ok := strings.CutPrefix(rest[len(quoted):], form.join+"-"); ok {
			return form.lead + quoted + form.join + "--" + tail
		}
	}
	return msg
}

func flagError(flags *flag.FlagSet, help string, err error, stderr io.Writer) int {
	fmt.Fprintf(stderr, "togi %s: %s\n", flags.Name(), flagMessage(err))
	commandUsage(flags, help, stderr)
	return exitUsage
}

func commandUsage(flags *flag.FlagSet, help string, w io.Writer) {
	var b strings.Builder
	b.WriteString(help)
	b.WriteString("\n")
	writeFlags(&b, "Flags", flags, func(f *flag.Flag) bool { return !isGlobal(f) })
	writeFlags(&b, "Global flags", flags, isGlobal)
	_, _ = io.WriteString(w, b.String())
}

type example struct{ command, text string }

// examples renders a help Examples block with every description starting in
// one column, so that a new or reworded example cannot misalign it.
func examples(rows ...example) string {
	width := 0
	for _, r := range rows {
		width = max(width, len(r.command))
	}
	var b strings.Builder
	b.WriteString("Examples:\n")
	for i, r := range rows {
		if r.text == "" {
			fmt.Fprintf(&b, "  %s", r.command)
		} else {
			fmt.Fprintf(&b, "  %-*s   %s", width, r.command, r.text)
		}
		if i < len(rows)-1 {
			b.WriteString("\n")
		}
	}
	return b.String()
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
