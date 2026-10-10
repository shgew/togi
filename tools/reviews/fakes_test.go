package main

import (
	"bytes"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

var update = flag.Bool("update", false, "rewrite testdata golden files from the current output")

const (
	headSHA     = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	movedSHA    = "cccccccccccccccccccccccccccccccccccccccc"
	baseSHA     = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
	oldHeadSHA  = "1111111111111111111111111111111111111111"
	oldBaseSHA  = "2222222222222222222222222222222222222222"
	fixSHA      = "dddddddddddddddddddddddddddddddddddddddd"
	prURL       = "https://github.com/shgew/togi/pull/700"
	previousURL = "https://github.com/shgew/togi/pull/700#issuecomment-4242"
)

// call is one fake command invocation.
type call struct {
	Name string
	Args []string
}

func (c call) String() string { return c.Name + " " + strings.Join(c.Args, " ") }

// fakeRunner stands in for gh and git: every call is recorded and answered by handle.
type fakeRunner struct {
	name   string
	handle func(args []string) ([]byte, error)
	calls  []call
}

func (f *fakeRunner) run(args ...string) ([]byte, error) {
	f.calls = append(f.calls, call{f.name, args})
	return f.handle(args)
}

func (f *fakeRunner) joined() []string {
	var out []string
	for _, c := range f.calls {
		out = append(out, c.String())
	}
	return out
}

// testTools returns tools whose gh and git are the given handlers and whose output goes to the returned buffers.
func testTools(t *testing.T, gh, git func(args []string) ([]byte, error)) (tools, *fakeRunner, *fakeRunner, *bytes.Buffer, *bytes.Buffer) {
	t.Helper()
	g, c := &fakeRunner{name: "gh", handle: gh}, &fakeRunner{name: "git", handle: git}
	if gh == nil {
		g.handle = func(args []string) ([]byte, error) { t.Fatalf("unexpected gh %v", args); return nil, nil }
	}
	if git == nil {
		c.handle = func(args []string) ([]byte, error) { t.Fatalf("unexpected git %v", args); return nil, nil }
	}
	var stdout, stderr bytes.Buffer
	return tools{
		gh: g.run, git: c.run, tmp: t.TempDir(), stdin: strings.NewReader(""),
		getenv: func(k string) string {
			if k == "GH_TOKEN" {
				return "token"
			}
			return ""
		},
		stdout: &stdout, stderr: &stderr,
	}, g, c, &stdout, &stderr
}

func checkGolden(t *testing.T, name, got string) {
	t.Helper()
	path := filepath.Join("testdata", name)
	if *update {
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(got), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read golden (run with -update to create it): %v", err)
	}
	if diff := cmp.Diff(string(want), got); diff != "" {
		t.Errorf("%s differs (-want +got):\n%s", name, diff)
	}
}

func readFile(t *testing.T, path string) string {
	t.Helper()
	b, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}

// orDefault returns s, or def when s is empty.
func orDefault(s, def string) string {
	if s == "" {
		return def
	}
	return s
}

// exitStatus is a failed command's exit status, as exec.ExitError reports it through ExitCode.
type exitStatus int

func (e exitStatus) Error() string { return fmt.Sprintf("exit status %d", int(e)) }

func (e exitStatus) ExitCode() int { return int(e) }
