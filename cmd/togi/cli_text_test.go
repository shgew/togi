package main

import (
	"bytes"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"unicode/utf8"

	"github.com/shgew/togi/internal/journal"
)

var exampleLine = regexp.MustCompile(`^  (\S.*?)   +(\S.*)$`)

func TestHelpExamplesShareOneDescriptionColumn(t *testing.T) {
	t.Parallel()
	argLists := [][]string{{"--help"}}
	for _, c := range commands {
		argLists = append(argLists, []string{c.name, "--help"})
	}
	for _, args := range argLists {
		t.Run(strings.Join(args, " "), func(t *testing.T) {
			t.Parallel()
			var stdout, stderr bytes.Buffer
			if code := cli(args, &stdout, &stderr); code != exitOK {
				t.Fatalf("exit %d, stderr %s", code, stderr.String())
			}
			_, block, ok := strings.Cut(stdout.String(), "Examples:\n")
			if !ok {
				t.Fatalf("no Examples in help:\n%s", stdout.String())
			}
			block, _, _ = strings.Cut(block, "\n\n")
			column := -1
			for line := range strings.SplitSeq(block, "\n") {
				m := exampleLine.FindStringSubmatchIndex(line)
				if m == nil {
					continue
				}
				got := utf8.RuneCountInString(line[:m[4]])
				if column >= 0 && got != column {
					t.Errorf("description of %q starts at column %d, the first at %d", line, got, column)
				}
				column = got
			}
		})
	}
}

func TestCyclesExampleWordedOnce(t *testing.T) {
	t.Parallel()
	var top, run, discard bytes.Buffer
	if code := cli([]string{"--help"}, &top, &discard); code != exitOK {
		t.Fatalf("exit %d", code)
	}
	if code := cli([]string{"run", "--help"}, &run, &discard); code != exitOK {
		t.Fatalf("exit %d", code)
	}
	for name, help := range map[string]string{"togi --help": top.String(), "togi run --help": run.String()} {
		if !strings.Contains(help, "sudo togi run --cycles 1  ") || !strings.Contains(help, cyclesOneExample) {
			t.Errorf("%s lacks the --cycles 1 example %q:\n%s", name, cyclesOneExample, help)
		}
	}
	if !strings.Contains(cyclesOneExample, "at its limit") || !strings.Contains(cyclesOneExample, "deepening") {
		t.Errorf("example %q must name the limit and deepening conditions", cyclesOneExample)
	}
}

func TestFlagErrorsSpellFlagsWithTwoDashes(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		args []string
		want string
	}{
		{[]string{"status", "--bogus"}, "togi status: flag provided but not defined: --bogus"},
		{[]string{"events", "--core", "x"}, `togi events: invalid value "x" for flag --core: must be a non-negative integer`},
		{[]string{"events", "--core"}, "togi events: flag needs an argument: --core"},
		{[]string{"watch", "--width", "abc"}, `togi watch: invalid value "abc" for flag --width: parse error`},
		{[]string{"reset", "--all=maybe"}, `togi reset: invalid boolean value "maybe" for --all: parse error`},
		{[]string{"--bogus"}, "togi: flag provided but not defined: --bogus"},
		{[]string{"--state-dir"}, "togi: flag needs an argument: --state-dir"},
	} {
		t.Run(strings.Join(tc.args, " "), func(t *testing.T) {
			t.Parallel()
			var stdout, stderr bytes.Buffer
			if code := cli(tc.args, &stdout, &stderr); code != exitUsage {
				t.Fatalf("exit %d, want %d", code, exitUsage)
			}
			first, _, _ := strings.Cut(stderr.String(), "\n")
			if first != tc.want {
				t.Fatalf("first line %q, want %q", first, tc.want)
			}
		})
	}
}

func TestConfigOnCommandsWithoutConfigIsAUsageError(t *testing.T) {
	t.Parallel()
	const want = "--config applies only to doctor, run and reset"
	dir := t.TempDir()
	for _, name := range []string{"status", "events", "watch"} {
		for _, args := range [][]string{
			{"--state-dir", dir, name, "--config", "/nonexistent"},
			{"--config", "/nonexistent", "--state-dir", dir, name},
		} {
			var stdout, stderr bytes.Buffer
			if code := cli(args, &stdout, &stderr); code != exitUsage {
				t.Fatalf("%v: exit %d, want %d", args, code, exitUsage)
			}
			first, _, _ := strings.Cut(stderr.String(), "\n")
			if wantLine := "togi " + name + ": " + want; first != wantLine || stdout.Len() != 0 {
				t.Fatalf("%v: first line %q, stdout %q; want %q", args, first, stdout.String(), wantLine)
			}
		}
	}
}

func TestStatusFitsEightyColumns(t *testing.T) {
	t.Parallel()
	goldens, err := filepath.Glob("testdata/status*.golden")
	if err != nil || len(goldens) == 0 {
		t.Fatalf("status goldens: %v, %v", goldens, err)
	}
	for _, name := range goldens {
		data, err := os.ReadFile(name)
		if err != nil {
			t.Fatal(err)
		}
		for i, line := range strings.Split(string(data), "\n") {
			if n := utf8.RuneCountInString(line); n > statusWidth {
				t.Errorf("%s:%d is %d columns: %s", name, i+1, n, line)
			}
		}
	}
}

func TestWrapLines(t *testing.T) {
	t.Parallel()
	var b bytes.Buffer
	wrapLines(&b, "\nnote ", "  ", strings.Repeat("word ", 20)+"unbreakable"+strings.Repeat("x", 90))
	for _, line := range strings.Split(strings.TrimSpace(b.String()), "\n") {
		if n := utf8.RuneCountInString(line); n > statusWidth && !strings.Contains(line, "unbreakable") {
			t.Errorf("line of %d columns: %q", n, line)
		}
	}
	if !strings.HasPrefix(b.String(), "\nnote word") || !strings.Contains(b.String(), "\n  word") {
		t.Fatalf("prefixes not applied:\n%s", b.String())
	}
}

func TestStatusHuntTrialsNeverExceedTheNeed(t *testing.T) {
	t.Parallel()
	st := journal.State{
		Phase: string(journal.PhaseHunt),
		Hunt: &journal.HuntState{Hunt: 1, Groups: []journal.GroupState{
			{Group: 1, Cores: []int{8}, Outcome: "pass", Passes: 37, Needed: 5},
			{Group: 2, Cores: []int{9}, Outcome: "failure", Passes: 0, Needed: 5},
			{Group: 3, Cores: []int{3}, Outcome: "pending", Passes: 2, Needed: 5},
		}},
		Session: &journal.SessionInfo{ID: "s"},
	}
	var out bytes.Buffer
	writeStatus(&out, st, nil)
	for _, want := range []string{"pass     5/5", "failure  0/5", "pending  2/5"} {
		if !strings.Contains(out.String(), want) {
			t.Errorf("status lacks %q:\n%s", want, out.String())
		}
	}
	if strings.Contains(out.String(), "37") {
		t.Errorf("status shows accumulated passes above the need:\n%s", out.String())
	}
}
