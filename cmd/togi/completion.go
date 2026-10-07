package main

import (
	"errors"
	"flag"
	"fmt"
	"io"
	"slices"
	"strconv"
	"strings"
	"text/template"

	"github.com/shgew/togi/internal/journal"
)

const completionHelp = `Usage: togi completion bash|zsh|fish|nushell

Print the completion script for one shell to stdout. It completes command
names, each command's flags, file paths for --config, --state-dir and
--tuning-boot, and the comma-separated kinds and groups for events --kind.
In nushell, --kind completes one kind or group, not the items after a comma;
the script needs nushell 0.115.1 or newer. The Nix package installs all four
scripts; regenerate a script you saved yourself after updating togi.

Examples:
  source <(togi completion bash)    Load completions in bash, such as from ~/.bashrc
  source <(togi completion zsh)     Load completions in zsh after compinit
  togi completion fish | source     Load completions in fish
  togi completion nushell | save -f togi.nu   Write a script for nushell's source command`

type completionShell struct {
	name   string
	script *template.Template
}

var completionShells = []completionShell{
	{"bash", completionTemplate("bash", bashCompletion)},
	{"zsh", completionTemplate("zsh", zshCompletion)},
	{"fish", completionTemplate("fish", fishCompletion)},
	{"nushell", completionTemplate("nushell", nushellCompletion)},
}

func completionShellNames() []string {
	names := make([]string, len(completionShells))
	for i, s := range completionShells {
		names[i] = s.name
	}
	return names
}

func runCompletion(g *globals, args []string, stdout, stderr io.Writer) int {
	flags := newFlagSet("completion", g)
	err := flags.Parse(args)
	if errors.Is(err, flag.ErrHelp) {
		commandUsage(flags, completionHelp, stdout)
		return exitOK
	}
	if err == nil {
		switch flags.NArg() {
		case 0:
			err = errors.New("missing shell")
		case 1:
		default:
			err = fmt.Errorf("unexpected argument %q", flags.Arg(1))
		}
	}
	if err != nil {
		return flagError(flags, completionHelp, err, stderr)
	}
	i := slices.IndexFunc(completionShells, func(s completionShell) bool { return s.name == flags.Arg(0) })
	if i < 0 {
		return flagError(flags, completionHelp, fmt.Errorf("unknown shell %q", flags.Arg(0)), stderr)
	}
	var b strings.Builder
	if err := completionShells[i].script.Execute(&b, newCompletionModel()); err != nil {
		fmt.Fprintf(stderr, "togi completion: render %s script: %v\n", flags.Arg(0), err)
		return exitError
	}
	_, _ = io.WriteString(stdout, b.String())
	return exitOK
}

// completionFlag is one flag as completion offers it. Value is "file" for a
// path, "kinds" for event kind selectors, and empty for a value with nothing
// to complete.
type completionFlag struct {
	Name        string
	Bool        bool
	Placeholder string
	Value       string
}

type completionCommand struct {
	Name    string
	Summary string
	Flags   []completionFlag
	Args    []string
}

type completionModel struct {
	Top      completionCommand
	Commands []completionCommand
	Kinds    []string
}

func newCompletionModel() completionModel {
	m := completionModel{
		Top:   completionCommand{Name: "togi", Flags: completionFlags(topFlags(&globals{}, new(bool)))},
		Kinds: journal.KindSelectors(),
	}
	for _, c := range slices.SortedFunc(slices.Values(commands), func(a, b command) int { return strings.Compare(a.name, b.name) }) {
		m.Commands = append(m.Commands, completionCommand{Name: c.name, Summary: c.summary, Flags: completionFlags(c.flags(&globals{})), Args: c.args})
	}
	return m
}

func completionFlags(fs *flag.FlagSet) []completionFlag {
	var flags []completionFlag
	fs.VisitAll(func(f *flag.Flag) {
		arg, _ := flag.UnquoteUsage(f)
		cf := completionFlag{Name: f.Name, Placeholder: arg}
		if b, ok := f.Value.(interface{ IsBoolFlag() bool }); ok && b.IsBoolFlag() {
			cf.Bool = true
		}
		switch arg {
		case "path", "file":
			cf.Value = "file"
		case "kinds":
			cf.Value = "kinds"
		}
		flags = append(flags, cf)
	})
	return flags
}

// dashed returns the --name form of the flags that take a value of the given
// kind, of every flag that takes a value when kind is "value", or of every
// flag when kind is "all".
func (c completionCommand) dashed(kind string) []string {
	var names []string
	for _, f := range c.Flags {
		if kind == "all" || !f.Bool && (kind == "value" || kind == f.Value) {
			names = append(names, "--"+f.Name)
		}
	}
	return names
}

func (c completionCommand) AllFlags() []string   { return c.dashed("all") }
func (c completionCommand) ValueFlags() []string { return c.dashed("value") }
func (c completionCommand) FileFlags() []string  { return c.dashed("file") }
func (c completionCommand) KindFlags() []string  { return c.dashed("kinds") }
func (c completionCommand) PlainFlags() []string { return c.dashed("") }

func (m completionModel) CommandNames() []string {
	names := make([]string, len(m.Commands))
	for i, c := range m.Commands {
		names[i] = c.Name
	}
	return names
}

func completionTemplate(name, text string) *template.Template {
	return template.Must(template.New(name).Funcs(template.FuncMap{
		"join": strings.Join,
		// sq quotes a string for a POSIX shell, zsh or fish.
		"sq": func(s string) string { return "'" + strings.ReplaceAll(s, "'", `'\''`) + "'" },
		// dq quotes a string for nushell.
		"dq": strconv.Quote,
	}).Parse(text))
}

const bashCompletion = `# bash completion for togi, generated by togi completion bash.

_togi_words() {
    local word
    for word in "$@"; do
        if [[ $word == "$cur"* ]]; then
            COMPREPLY+=("$word")
        fi
    done
}

_togi_files() {
    local file
    compopt -o filenames 2>/dev/null
    while IFS= read -r file; do
        COMPREPLY+=("$file")
    done < <(compgen -f -- "$cur")
}

_togi_kinds() {
    local prefix= word=$cur kind
    if [[ $cur == *,* ]]; then
        prefix=${cur%,*},
        word=${cur##*,}
    fi
    for kind in {{join .Kinds " "}}; do
        if [[ $kind == "$word"* ]]; then
            COMPREPLY+=("$prefix$kind")
        fi
    done
}

_togi() {
    local cur=${COMP_WORDS[COMP_CWORD]} prev=${COMP_WORDS[COMP_CWORD-1]} cmd= i
    COMPREPLY=()
    if [[ $cur == = ]]; then
        cur=
    elif [[ $prev == = ]]; then
        prev=${COMP_WORDS[COMP_CWORD-2]}
    fi
    for ((i = 1; i < COMP_CWORD; i++)); do
        case ${COMP_WORDS[i]} in
{{- with .Top.ValueFlags}}
        {{join . "|"}})
            ((i++))
            if ((i < COMP_CWORD)) && [[ ${COMP_WORDS[i]} =~ ^=+$ ]]; then
                ((i++))
            fi
            while ((i + 1 < COMP_CWORD)) && [[ ${COMP_WORDS[i+1]} =~ ^=+$ ]]; do
                ((i += 2))
            done
            ;;
{{- end}}
        -*) ;;
        *)
            cmd=${COMP_WORDS[i]}
            break
            ;;
        esac
    done
    case $cmd in
    "")
        case $prev in
{{- with .Top.FileFlags}}
        {{join . "|"}})
            _togi_files
            return
            ;;
{{- end}}
        esac
        if [[ $cur == -* ]]; then
            _togi_words {{join .Top.AllFlags " "}}
        else
            _togi_words {{join .CommandNames " "}}
        fi
        ;;
{{- range .Commands}}
    {{.Name}})
        case $prev in
{{- with .FileFlags}}
        {{join . "|"}})
            _togi_files
            return
            ;;
{{- end}}
{{- with .KindFlags}}
        {{join . "|"}})
            _togi_kinds
            return
            ;;
{{- end}}
{{- with .PlainFlags}}
        {{join . "|"}})
            return
            ;;
{{- end}}
        esac
{{- if .Args}}
        if [[ $cur == -* ]]; then
            _togi_words {{join .AllFlags " "}}
        else
            _togi_words {{join .Args " "}}
        fi
{{- else}}
        _togi_words {{join .AllFlags " "}}
{{- end}}
        ;;
{{- end}}
    esac
}

complete -F _togi togi
`

const zshCompletion = `#compdef togi
# zsh completion for togi, generated by togi completion zsh.

_togi_kinds() {
  _sequence compadd -{{range .Kinds}} {{.}}{{end}}
}

_togi() {
  local curcontext=$curcontext state line
  local -a commands=(
{{- range .Commands}}
    {{sq (printf "%s:%s" .Name .Summary)}}
{{- end}}
  )
  _arguments -C \
{{- range .Top.Flags}}
    {{template "spec" .}} \
{{- end}}
    '1:command:->command' \
    '*::argument:->argument'
  case $state in
  command)
    _describe -t commands 'togi command' commands
    ;;
  argument)
    case $line[1] in
{{- range .Commands}}
    {{.Name}})
      _arguments
{{- range .Flags}} \
        {{template "spec" .}}
{{- end}}
{{- if .Args}} \
        {{sq (printf "1:argument:(%s)" (join .Args " "))}}
{{- end}}
      ;;
{{- end}}
    esac
    ;;
  esac
}

if [[ $zsh_eval_context[-1] == loadautofunc ]]; then
  _togi "$@"
else
  compdef _togi togi
fi
{{define "spec"}}
{{- if .Bool}}{{sq (printf "--%s" .Name)}}
{{- else if eq .Value "file"}}{{sq (printf "--%s=:%s:_files" .Name .Placeholder)}}
{{- else if eq .Value "kinds"}}{{sq (printf "--%s=:%s:_togi_kinds" .Name .Placeholder)}}
{{- else}}{{sq (printf "--%s=:%s: " .Name .Placeholder)}}
{{- end}}
{{- end}}`

const fishCompletion = `# fish completion for togi, generated by togi completion fish.

function __togi_kinds
    set -l token (commandline -ct)
    set token (string replace -r -- '^--?[^=]*=' '' "$token")
    set -l prefix (string replace -r -- '[^,]*$' '' "$token")
    for kind in {{join .Kinds " "}}
        echo "$prefix$kind"
    end
end

function __togi_command_is
    # commandline -x, which expands variables, is fish 4 only.
    set -l tokens (commandline -xpc 2>/dev/null)
    or set tokens (commandline -opc)
    set -e tokens[1]
    set -l command
    set -l skip
    for token in $tokens
        if test -n "$skip"
            set skip
            continue
        end
        switch $token
{{- with .Top.ValueFlags}}
            case {{join . " "}}
                set skip 1
{{- end}}
            case '-*'
            case '*'
                set command $token
                break
        end
    end
    test "$command" = "$argv[1]"
end

complete -c togi -f
{{- $top := "__togi_command_is ''"}}
{{- range .Commands}}
complete -c togi -n "{{$top}}" -a {{.Name}} -d {{sq .Summary}}
{{- end}}
{{- range .Top.Flags}}
complete -c togi -n "{{$top}}" {{template "flag" .}}
{{- end}}
{{- range .Commands}}
{{- $is := printf "__togi_command_is %s" .Name}}
{{- range .Flags}}
complete -c togi -n "{{$is}}" {{template "flag" .}}
{{- end}}
{{- if .Args}}
complete -c togi -n "{{$is}}" -a {{sq (join .Args " ")}}
{{- end}}
{{- end}}
{{define "flag"}}
{{- if .Bool}}-l {{.Name}}
{{- else if eq .Value "file"}}-l {{.Name}} -r -F
{{- else if eq .Value "kinds"}}-l {{.Name}} -x -a '(__togi_kinds)'
{{- else}}-l {{.Name}} -x
{{- end}}
{{- end}}`

const nushellCompletion = `# nushell completion for togi, generated by togi completion nushell.
# Needs nushell 0.115.1 or newer.

def "nu-complete togi kinds" [] {
    [{{range $i, $k := .Kinds}}{{if $i}} {{end}}{{dq $k}}{{end}}]
}
{{- range .Commands}}
{{- if .Args}}

def "nu-complete togi {{.Name}}" [] {
    [{{range $i, $a := .Args}}{{if $i}} {{end}}{{dq $a}}{{end}}]
}
{{- end}}
{{- end}}

# Not exported: a module named togi, as with use togi.nu *, cannot export togi.

extern "togi" [
{{- range .Top.Flags}}
    {{template "flag" .}}
{{- end}}
]
{{- range .Commands}}

# {{.Summary}}
export extern {{dq (printf "togi %s" .Name)}} [
{{- if .Args}}
    argument?: string@{{dq (printf "nu-complete togi %s" .Name)}}
{{- end}}
{{- range .Flags}}
    {{template "flag" .}}
{{- end}}
]
{{- end}}
{{define "flag"}}
{{- if .Bool}}--{{.Name}}
{{- else if eq .Value "file"}}--{{.Name}}: path
{{- else if eq .Value "kinds"}}--{{.Name}}: string@"nu-complete togi kinds"
{{- else}}--{{.Name}}: string
{{- end}}
{{- end}}`
