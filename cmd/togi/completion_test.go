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

// completionLayouts say where each script offers its commands and each
// command's flags, so a name missing from its own place fails even when it
// appears elsewhere in the script. flags takes the empty command for the top
// level.
var completionLayouts = map[string]struct {
	commands   func(script string) []string
	flags      func(script, command string) []string
	kindsAfter string
}{
	"bash": {
		commands: func(script string) []string {
			return bashWords(section(script, "\n    \"\")\n", "\n        ;;\n"), false)
		},
		flags: func(script, command string) []string {
			start := "\n    " + command + ")\n"
			if command == "" {
				start = "\n    \"\")\n"
			}
			return bashWords(section(script, start, "\n        ;;\n"), true)
		},
		kindsAfter: "    for kind in ",
	},
	"zsh": {
		commands: func(script string) []string {
			var names []string
			for line := range strings.Lines(section(script, "local -a commands=(\n", "\n  )\n")) {
				name, _, _ := strings.Cut(strings.TrimSpace(line), ":")
				names = append(names, strings.TrimPrefix(name, "'"))
			}
			return names
		},
		flags: func(script, command string) []string {
			block := section(script, "\n    "+command+")\n", "\n      ;;\n")
			if command == "" {
				block = section(script, "_arguments -C \\\n", "'1:command:->command'")
			}
			var names []string
			for line := range strings.Lines(block) {
				if spec, ok := strings.CutPrefix(strings.TrimSpace(line), "'--"); ok {
					names = append(names, "--"+strings.FieldsFunc(spec, func(r rune) bool { return r == '=' || r == '\'' })[0])
				}
			}
			return names
		},
		kindsAfter: "  _sequence compadd - ",
	},
	"fish": {
		commands: func(script string) []string {
			return fishWords(script, "", "-a")
		},
		flags: func(script, command string) []string {
			return fishWords(script, command, "-l")
		},
		kindsAfter: "    for kind in ",
	},
	"nushell": {
		commands: func(script string) []string {
			var names []string
			for line := range strings.Lines(script) {
				if rest, ok := strings.CutPrefix(line, `export extern "togi `); ok {
					name, _, _ := strings.Cut(rest, `"`)
					names = append(names, name)
				}
			}
			return names
		},
		flags: func(script, command string) []string {
			start := `export extern "togi ` + command + `" [`
			if command == "" {
				start = `extern "togi" [`
			}
			var names []string
			for line := range strings.Lines(section(script, "\n"+start+"\n", "\n]")) {
				if strings.HasPrefix(strings.TrimSpace(line), "--") {
					name, _, _ := strings.Cut(strings.TrimSpace(line), ":")
					names = append(names, name)
				}
			}
			return names
		},
		kindsAfter: "def \"nu-complete togi kinds\" [] {\n    ",
	},
}

// section returns the text between start and the next end, or nothing when
// the script lacks start.
func section(script, start, end string) string {
	_, rest, ok := strings.Cut(script, start)
	if !ok {
		return ""
	}
	s, _, _ := strings.Cut(rest, end)
	return s
}

// bashWords returns the flags, or the other words, that the _togi_words lines
// of block offer.
func bashWords(block string, flags bool) []string {
	var words []string
	for line := range strings.Lines(block) {
		rest, ok := strings.CutPrefix(strings.TrimSpace(line), "_togi_words ")
		if !ok {
			continue
		}
		for word := range strings.FieldsSeq(rest) {
			if strings.HasPrefix(word, "--") == flags {
				words = append(words, word)
			}
		}
	}
	return words
}

// fishWords returns the --name of each -l, or the name of each -a, on the
// complete lines whose condition is that command is the given one.
func fishWords(script, command, option string) []string {
	condition := `-n "__togi_command_is ` + command + `" `
	if command == "" {
		condition = `-n "__togi_command_is ''" `
	}
	var words []string
	for line := range strings.Lines(script) {
		_, rest, ok := strings.Cut(line, condition)
		if !ok {
			continue
		}
		fields := strings.Fields(rest)
		if len(fields) < 2 || fields[0] != option {
			continue
		}
		if option == "-l" {
			words = append(words, "--"+fields[1])
		} else {
			words = append(words, fields[1])
		}
	}
	return words
}

func dashedFlags(fs *flag.FlagSet) []string {
	var names []string
	fs.VisitAll(func(f *flag.Flag) { names = append(names, "--"+f.Name) })
	return names
}

func TestCompletionCoversCommands(t *testing.T) {
	t.Parallel()
	if got, want := slices.Sorted(maps.Keys(completionLayouts)), slices.Sorted(slices.Values(completionShellNames())); !slices.Equal(got, want) {
		t.Fatalf("completion layouts cover shells %v, want %v", got, want)
	}
	for _, c := range commands {
		if c.flags == nil {
			t.Fatalf("command %s has no flags entry, so completion cannot offer its flags", c.name)
		}
	}
	for shell := range completionLayouts {
		t.Run(shell, func(t *testing.T) {
			t.Parallel()
			checkCompletionCoverage(t, shell, completionScript(t, shell))
		})
	}
}

func checkCompletionCoverage(t *testing.T, shell, script string) {
	t.Helper()
	layout := completionLayouts[shell]
	var names []string
	for _, c := range commands {
		names = append(names, c.name)
	}
	if diff := cmp.Diff(slices.Sorted(slices.Values(names)), slices.Sorted(slices.Values(layout.commands(script)))); diff != "" {
		t.Errorf("offered commands differ (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(slices.Sorted(slices.Values(dashedFlags(topFlags(&globals{}, new(bool))))), slices.Sorted(slices.Values(layout.flags(script, "")))); diff != "" {
		t.Errorf("top-level flags differ (-want +got):\n%s", diff)
	}
	for _, c := range commands {
		if diff := cmp.Diff(slices.Sorted(slices.Values(dashedFlags(c.flags(&globals{})))), slices.Sorted(slices.Values(layout.flags(script, c.name)))); diff != "" {
			t.Errorf("flags of %s differ (-want +got):\n%s", c.name, diff)
		}
	}
	_, rest, ok := strings.Cut(script, layout.kindsAfter)
	if !ok {
		t.Fatalf("script has no kind list after %q", layout.kindsAfter)
	}
	line, _, _ := strings.Cut(rest, "\n")
	line = strings.TrimSuffix(line, "; do")
	var kinds []string
	for kind := range strings.FieldsSeq(strings.Trim(line, "[]")) {
		kinds = append(kinds, strings.Trim(kind, `"`))
	}
	if diff := cmp.Diff(journal.KindSelectors(), kinds); diff != "" {
		t.Errorf("kind list differs from journal.KindSelectors (-want +got):\n%s", diff)
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
		{name: "unknown flag", args: []string{"completion", "--shell", "bash"}, code: exitUsage, err: "flag provided but not defined: --shell"},
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
