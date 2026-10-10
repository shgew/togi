// Command togi finds and tests per-core Curve Optimizer offsets on Zen 5 desktop CPUs.
package main

import (
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"slices"
	"strings"

	"github.com/shgew/togi"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/hostlock"
	"github.com/shgew/togi/internal/journal"
)

const defaultStateDir = "/var/lib/togi"

const banner = "   __              _\n" +
	"  / /_____  ____ _(_)\n" +
	" / __/ __ \\/ __ `/ /\n" +
	"/ /_/ /_/ / /_/ / /\n" +
	"\\__/\\____/\\__, /_/\n" +
	"         /____/\n" +
	"\n" +
	"  per-core Curve Optimizer\n" +
	"\n"

type globals struct {
	config       string
	configSet    bool
	stateDir     string
	hostLockPath string
}

type command struct {
	name    string
	summary string
	help    string
	flags   func(g *globals) *flag.FlagSet
	// args lists the positional values the command accepts, for completion.
	args []string
	run  func(g *globals, args []string, stdout, stderr io.Writer) int
}

// commands is assigned in init because completion reads it.
var commands []command

func init() {
	commands = []command{
		{name: "completion", summary: "Print a shell completion script", help: completionHelp, flags: func(g *globals) *flag.FlagSet {
			return newFlagSet("completion", g)
		}, args: completionShellNames(), run: runCompletion},
		{name: "doctor", summary: "Check whether this machine is ready for a run, writing nothing", help: doctorHelp, flags: func(g *globals) *flag.FlagSet {
			return newFlagSet("doctor", g)
		}, run: runDoctor},
		{name: "events", summary: "Render the journal", help: eventsHelp, flags: func(g *globals) *flag.FlagSet {
			return eventsFlags(g, &journal.Filter{}, new(bool))
		}, run: runEvents},
		{name: "reset", summary: "Reset one core or archive the session", help: resetHelp, flags: func(g *globals) *flag.FlagSet {
			return resetFlags(g, new(*int), new(bool))
		}, run: runReset},
		{name: "restart-limit", summary: "Recover after the tuning service reaches its restart limit", help: restartLimitHelp, flags: func(g *globals) *flag.FlagSet {
			return restartLimitFlags(g, new(string))
		}, run: runRestartLimit},
		{name: "run", summary: "Start or resume the session in the foreground", help: runHelp, flags: func(g *globals) *flag.FlagSet {
			return runFlags(g, new(int), new(bool), new(string), new(bool))
		}, run: runRun},
		{name: "status", summary: "Show core failure points, combinations, activity and clean cycles", help: statusHelp, flags: func(g *globals) *flag.FlagSet {
			return newFlagSet("status", g)
		}, run: runStatus},
		{name: "watch", summary: "Show the session as a live dashboard", help: watchHelp, flags: func(g *globals) *flag.FlagSet {
			return watchFlags(g, new(int), new(int))
		}, run: runWatch},
	}
}

func main() {
	os.Exit(cli(os.Args[1:], os.Stdout, os.Stderr))
}

func cli(args []string, stdout, stderr io.Writer) int {
	return cliWithGlobals(args, stdout, stderr, globals{config: config.DefaultPath, stateDir: defaultStateDir, hostLockPath: hostlock.Path})
}

func cliWithGlobals(args []string, stdout, stderr io.Writer, g globals) int {
	var version bool
	fs := topFlags(&g, &version)
	if err := fs.Parse(args); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			usage(stdout)
			return exitOK
		}
		fmt.Fprintf(stderr, "togi: %s\n", flagMessage(err))
		usage(stderr)
		return exitUsage
	}
	if version {
		fmt.Fprintf(stdout, "togi %s\n", togi.String())
		return exitOK
	}
	if fs.NArg() == 0 {
		_, _ = io.WriteString(stderr, banner)
		usage(stderr)
		return exitUsage
	}
	name := fs.Arg(0)
	i := slices.IndexFunc(commands, func(c command) bool { return c.name == name })
	if i < 0 {
		fmt.Fprintf(stderr, "togi: unknown command %q\n", name)
		usage(stderr)
		return exitUsage
	}
	c := commands[i]
	if g.configSet && !acceptsConfig(c.name) {
		return flagError(c.flags(&g), c.help, errConfigScope, stderr)
	}
	return c.run(&g, fs.Args()[1:], stdout, stderr)
}

func topFlags(g *globals, version *bool) *flag.FlagSet {
	fs := flag.NewFlagSet("togi", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	registerGlobals(fs, g)
	fs.BoolVar(version, "version", false, "print the build version and git revision")
	return fs
}

func isGlobal(f *flag.Flag) bool {
	return f.Name == "state-dir" || f.Name == "version"
}

func writeFlags(b *strings.Builder, title string, flags *flag.FlagSet, include func(*flag.Flag) bool) {
	first := true
	flags.VisitAll(func(f *flag.Flag) {
		if !include(f) {
			return
		}
		if first {
			fmt.Fprintf(b, "\n%s:\n", title)
			first = false
		}
		arg, text := flag.UnquoteUsage(f)
		name := "--" + f.Name
		if arg != "" {
			name += " <" + arg + ">"
		}
		fmt.Fprintf(b, "  %-21s%s\n", name, text)
	})
}

func usage(w io.Writer) {
	var b strings.Builder
	b.WriteString("Usage: togi [--config <path>] [--state-dir <path>] <command> [flags]\n")
	b.WriteString("       togi --version\n\n")
	b.WriteString("Finds and tests per-core Curve Optimizer offsets on Zen 5 desktop CPUs.\n\n")
	b.WriteString(examples(
		example{"sudo togi run --cycles 1", cyclesOneExample},
		example{"togi status", "Show core failure points, combinations, activity and clean cycles"},
	))
	b.WriteString("\n\n")
	b.WriteString("Commands:\n")
	sorted := slices.SortedFunc(slices.Values(commands), func(a, b command) int { return strings.Compare(a.name, b.name) })
	for _, c := range sorted {
		fmt.Fprintf(&b, "  %-15s%s\n", c.name, c.summary)
	}
	fs := topFlags(&globals{}, new(bool))
	writeFlags(&b, "Flags", fs, func(*flag.Flag) bool { return true })
	b.WriteString("\nRun 'togi <command> --help' for its description, examples and flags.\n")
	_, _ = io.WriteString(w, b.String())
}
