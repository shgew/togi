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
	stateDirSet  bool
	hostLockPath string
}

type command struct {
	name    string
	summary string
	run     func(g *globals, args []string, stdout, stderr io.Writer) int
}

var commands = []command{
	{name: "cert", summary: "Render the certificate", run: runCert},
	{name: "events", summary: "Render the journal", run: runEvents},
	{name: "reset", summary: "Reset one core or archive the session", run: runReset},
	{name: "run", summary: "Start or resume the session in the foreground", run: runRun},
	{name: "status", summary: "Show core marks, hunt or refine activity, tier and exposure", run: runStatus},
	{name: "watch", summary: "Show the session as a live dashboard", run: runWatch},
}

func main() {
	os.Exit(cli(os.Args[1:], os.Stdout, os.Stderr))
}

func cli(args []string, stdout, stderr io.Writer) int {
	return cliWithGlobals(args, stdout, stderr, globals{config: config.DefaultPath, stateDir: defaultStateDir, hostLockPath: hostlock.Path})
}

func cliWithGlobals(args []string, stdout, stderr io.Writer, g globals) int {
	var version bool
	fs := flag.NewFlagSet("togi", flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	registerGlobals(fs, &g)
	fs.BoolVar(&version, "version", false, "print the build version and git revision")
	if err := fs.Parse(args); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			usage(stdout)
			return exitOK
		}
		fmt.Fprintf(stderr, "togi: %v\n", err)
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
	if g.configSet && name != "run" && name != "reset" {
		return commands[i].run(&g, []string{"--config"}, stdout, stderr)
	}
	return commands[i].run(&g, fs.Args()[1:], stdout, stderr)
}

func registerGlobals(fs *flag.FlagSet, g *globals) {
	if fs.Name() == "togi" || fs.Name() == "run" || fs.Name() == "reset" {
		fs.Func("config", "configuration file `path` for run and reset (default "+config.DefaultPath+")", func(s string) error {
			g.config, g.configSet = s, true
			return nil
		})
	}
	fs.Func("state-dir", "state directory `path` (default "+defaultStateDir+")", func(s string) error {
		g.stateDir, g.stateDirSet = s, true
		return nil
	})
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
	b.WriteString("Examples:\n")
	b.WriteString("  sudo togi run --rotations 1   Stop after search and one qualifying rotation\n")
	b.WriteString("  togi status                   Show core marks, activity, tier and exposure\n\n")
	b.WriteString("Commands:\n")
	sorted := slices.SortedFunc(slices.Values(commands), func(a, b command) int { return strings.Compare(a.name, b.name) })
	for _, c := range sorted {
		fmt.Fprintf(&b, "  %-9s%s\n", c.name, c.summary)
	}
	fs := flag.NewFlagSet("togi", flag.ContinueOnError)
	registerGlobals(fs, &globals{})
	fs.Bool("version", false, "print the build version and git revision")
	writeFlags(&b, "Flags", fs, func(*flag.Flag) bool { return true })
	b.WriteString("\nRun 'togi <command> --help' for its description, examples and flags.\n")
	_, _ = io.WriteString(w, b.String())
}
