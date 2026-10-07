package main

import (
	"bytes"
	"flag"
	"maps"
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/journal"
)

func completionScript(t *testing.T, shell string) string {
	t.Helper()
	var stdout, stderr bytes.Buffer
	if code := cli([]string{"completion", shell}, &stdout, &stderr); code != exitOK || stderr.Len() != 0 {
		t.Fatalf("togi completion %s: exit %d, stderr %q", shell, code, stderr.String())
	}
	return stdout.String()
}

func TestCompletionScripts(t *testing.T) {
	t.Parallel()
	for _, shell := range completionShellNames() {
		t.Run(shell, func(t *testing.T) {
			t.Parallel()
			golden(t, "completion-"+shell, completionScript(t, shell))
		})
	}
}

// completionForms says how each script spells a command, a flag and its list
// of kind selectors.
var completionForms = map[string]struct {
	command    func(string) string
	flag       func(string) string
	kindsAfter string
}{
	"bash": {
		command:    func(name string) string { return "\n    " + name + ")\n" },
		flag:       func(name string) string { return "--" + name },
		kindsAfter: "    for kind in ",
	},
	"zsh": {
		command:    func(name string) string { return "\n    '" + name + ":" },
		flag:       func(name string) string { return "'--" + name },
		kindsAfter: "  _sequence compadd - ",
	},
	"fish": {
		command:    func(name string) string { return " -a " + name + " -d " },
		flag:       func(name string) string { return " -l " + name },
		kindsAfter: "    for kind in ",
	},
	"nushell": {
		command:    func(name string) string { return `export extern "togi ` + name + `" [` },
		flag:       func(name string) string { return "\n    --" + name },
		kindsAfter: "def \"nu-complete togi kinds\" [] {\n    ",
	},
}

func TestCompletionCoversCommands(t *testing.T) {
	t.Parallel()
	if got, want := slices.Sorted(maps.Keys(completionForms)), slices.Sorted(slices.Values(completionShellNames())); !slices.Equal(got, want) {
		t.Fatalf("completion forms cover shells %v, want %v", got, want)
	}
	for _, c := range commands {
		if c.flags == nil {
			t.Fatalf("command %s has no flags entry, so completion cannot offer its flags", c.name)
		}
	}
	for shell, form := range completionForms {
		t.Run(shell, func(t *testing.T) {
			t.Parallel()
			script := completionScript(t, shell)
			flags := func(fs *flag.FlagSet) {
				fs.VisitAll(func(f *flag.Flag) {
					if !strings.Contains(script, form.flag(f.Name)) {
						t.Errorf("script omits flag --%s of %s", f.Name, fs.Name())
					}
				})
			}
			flags(topFlags(&globals{}, new(bool)))
			for _, c := range commands {
				if !strings.Contains(script, form.command(c.name)) {
					t.Errorf("script omits command %s", c.name)
				}
				flags(c.flags(&globals{}))
			}
			_, rest, ok := strings.Cut(script, form.kindsAfter)
			if !ok {
				t.Fatalf("script has no kind list after %q", form.kindsAfter)
			}
			line, _, _ := strings.Cut(rest, "\n")
			line = strings.TrimSuffix(line, "; do")
			var kinds []string
			for _, kind := range strings.Fields(strings.Trim(line, "[]")) {
				kinds = append(kinds, strings.Trim(kind, `"`))
			}
			if diff := cmp.Diff(journal.KindSelectors(), kinds); diff != "" {
				t.Errorf("kind list differs from journal.KindSelectors (-want +got):\n%s", diff)
			}
		})
	}
}

func TestCompletionExitCodes(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		args []string
		code int
		err  string
	}{
		{name: "no shell", args: []string{"completion"}, code: exitUsage, err: "missing shell"},
		{name: "unknown shell", args: []string{"completion", "tcsh"}, code: exitUsage, err: `unknown shell "tcsh"`},
		{name: "two shells", args: []string{"completion", "bash", "zsh"}, code: exitUsage, err: `unexpected argument "zsh"`},
		{name: "unknown flag", args: []string{"completion", "--shell", "bash"}, code: exitUsage, err: "flag provided but not defined: -shell"},
		{name: "bash", args: []string{"completion", "bash"}, code: exitOK},
		{name: "global flag", args: []string{"--state-dir", "unused", "completion", "fish"}, code: exitOK},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			var stdout, stderr bytes.Buffer
			code := cli(tc.args, &stdout, &stderr)
			if code != tc.code {
				t.Fatalf("exit %d, want %d; stderr %q", code, tc.code, stderr.String())
			}
			if tc.code == exitOK {
				if stderr.Len() != 0 || stdout.Len() == 0 {
					t.Fatalf("stdout %d bytes, stderr %q; want a script and no stderr", stdout.Len(), stderr.String())
				}
				return
			}
			if stdout.Len() != 0 {
				t.Fatalf("stdout %q on a usage error", stdout.String())
			}
			want := "togi completion: " + tc.err + "\n" + completionHelp
			if !strings.HasPrefix(stderr.String(), want) {
				t.Fatalf("stderr %q, want it to start with %q", stderr.String(), want)
			}
		})
	}
}
