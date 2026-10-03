package main

import (
	"errors"
	"flag"
	"fmt"
	"io"
	"io/fs"
	"slices"
	"strings"

	"github.com/shgew/togi/internal/config"
)

var configCommands = []string{"run", "reset"}

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
		fs.Func("config", "configuration file `path` for run and reset (default "+config.DefaultPath+")", func(s string) error {
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

func flagError(flags *flag.FlagSet, help string, err error, stderr io.Writer) int {
	fmt.Fprintf(stderr, "togi %s: %v\n", flags.Name(), err)
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
