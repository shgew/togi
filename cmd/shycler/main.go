// Command shycler finds and tests per-core Curve Optimizer offsets on Zen 5 desktop CPUs.
package main

import (
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"slices"
	"strings"

	"code.marleb.org/shgew/shycler/internal/config"
)

const defaultStateDir = "/var/lib/shycler"

const banner = `         __               __
   _____/ /_  __  _______/ /__  _____
  / ___/ __ \/ / / / ___/ / _ \/ ___/
 (__  ) / / / /_/ / /__/ /  __/ /
/____/_/ /_/\__, /\___/_/\___/_/
           /____/

  per-core Curve Optimizer

`

type globals struct {
	config      string
	configSet   bool
	stateDir    string
	stateDirSet bool
}

type command struct {
	name    string
	summary string
	run     func(g *globals, args []string, stdout, stderr io.Writer) int
}

var commands = []command{
	{name: "cert", summary: "Render the certificate", run: runCert},
	{name: "events", summary: "Render the journal", run: runEvents},
	{name: "regain", summary: "Queue regain of unproven depth", run: runRegain},
	{name: "reset", summary: "Reset one core or archive the session", run: runReset},
	{name: "run", summary: "Start or resume the session in the foreground", run: runRun},
	{name: "status", summary: "Show per-core offsets, tier and clean hours", run: runStatus},
}

func main() {
	os.Exit(cli(os.Args[1:], os.Stdout, os.Stderr))
}

func cli(args []string, stdout, stderr io.Writer) int {
	g := globals{config: config.DefaultPath, stateDir: defaultStateDir}
	fs := flag.NewFlagSet("shycler", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	registerGlobals(fs, &g)
	if err := fs.Parse(args); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			usage(stdout)
			return exitOK
		}
		fmt.Fprintf(stderr, "shycler: %v\n", err)
		usage(stderr)
		return exitUsage
	}
	if fs.NArg() == 0 {
		_, _ = io.WriteString(stderr, banner)
		usage(stderr)
		return exitUsage
	}
	name := fs.Arg(0)
	i := slices.IndexFunc(commands, func(c command) bool { return c.name == name })
	if i < 0 {
		fmt.Fprintf(stderr, "shycler: unknown command %q\n", name)
		usage(stderr)
		return exitUsage
	}
	return commands[i].run(&g, fs.Args()[1:], stdout, stderr)
}

func registerGlobals(fs *flag.FlagSet, g *globals) {
	fs.Func("config", "configuration file `path` (default "+config.DefaultPath+")", func(s string) error {
		g.config, g.configSet = s, true
		return nil
	})
	fs.Func("state-dir", "state directory `path` (default "+defaultStateDir+")", func(s string) error {
		g.stateDir, g.stateDirSet = s, true
		return nil
	})
}

func isGlobal(f *flag.Flag) bool {
	return f.Name == "config" || f.Name == "state-dir"
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
	b.WriteString("Usage: shycler [--config <path>] [--state-dir <path>] <command> [flags]\n\n")
	b.WriteString("Finds and tests per-core Curve Optimizer offsets on Zen 5 desktop CPUs.\n\n")
	b.WriteString("Examples:\n")
	b.WriteString("  shycler run --sim 1   Simulate a session, without hardware or root\n")
	b.WriteString("  shycler status        Show per-core offsets, tier and clean hours\n\n")
	b.WriteString("Commands:\n")
	sorted := slices.SortedFunc(slices.Values(commands), func(a, b command) int { return strings.Compare(a.name, b.name) })
	for _, c := range sorted {
		fmt.Fprintf(&b, "  %-9s%s\n", c.name, c.summary)
	}
	fs := flag.NewFlagSet("shycler", flag.ContinueOnError)
	registerGlobals(fs, &globals{})
	writeFlags(&b, "Flags", fs, isGlobal)
	b.WriteString("\nRun 'shycler <command> --help' for its description, examples and flags.\n")
	_, _ = io.WriteString(w, b.String())
}
