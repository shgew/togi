//go:build integration

package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	neturl "net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

// cleanGit runs git in dir with no inherited configuration, attributes or Git environment.
func cleanGit(t *testing.T, home, dir string, args ...string) string {
	t.Helper()
	cmd := exec.Command("git", args...)
	cmd.Dir = dir
	cmd.Env = []string{
		"PATH=" + os.Getenv("PATH"), "HOME=" + home, "GIT_CONFIG_NOSYSTEM=1", "GIT_ATTR_NOSYSTEM=1",
		"GIT_AUTHOR_NAME=Snapshot Test", "GIT_AUTHOR_EMAIL=snapshot@example.com",
		"GIT_COMMITTER_NAME=Snapshot Test", "GIT_COMMITTER_EMAIL=snapshot@example.com",
	}
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		t.Fatalf("git %v: %v: %s", args, err, stderr.String())
	}
	return stdout.String()
}

// largePull is a pull request in a local origin: a shared ancestor, base commits after it and head commits with a text change, a textconv target, a rename, a mode-only change and excluded paths.
type largePull struct {
	origin, caller, base, head string
}

func newLargePull(t *testing.T) largePull {
	t.Helper()
	root, home := t.TempDir(), t.TempDir()
	work := filepath.Join(root, "work")
	origin := filepath.Join(root, "origin.git")
	caller := filepath.Join(root, "caller")
	git := func(args ...string) string { t.Helper(); return cleanGit(t, home, work, args...) }
	write := func(name, content string, mode os.FileMode) {
		t.Helper()
		path := filepath.Join(work, name)
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(content), mode); err != nil {
			t.Fatal(err)
		}
		if err := os.Chmod(path, mode); err != nil {
			t.Fatal(err)
		}
	}
	cleanGit(t, home, root, "init", "--quiet", "--initial-branch=main", work)
	write("src/text.go", "package src\n\nfunc One() int {\n\treturn 1\n}\n\nfunc Two() int {\n\treturn 2\n}\n", 0o644)
	write("src/conv.go", "package src\n\nconst Name = \"before\"\n", 0o644)
	write("docs/old.md", "# Notes\n\nA page long enough\nto be detected\nas a rename\nwhen it moves.\n", 0o644)
	write("run.sh", "#!/bin/sh\necho run\n", 0o644)
	write("go.sum", "example.com/a v1.0.0 h1:one\n", 0o644)
	write("src/testdata/in.txt", "input one\n", 0o644)
	write("base.txt", "base 0\n", 0o644)
	git("add", ".")
	git("commit", "--quiet", "-m", "ancestor")
	git("switch", "--quiet", "-c", "feature")
	write("src/text.go", "package src\n\nfunc One() int {\n\treturn 1\n}\n\nfunc Two() int {\n\treturn 22\n}\n", 0o644)
	write("src/conv.go", "package src\n\nconst Name = \"after\"\n", 0o644)
	git("mv", "docs/old.md", "docs/new.md")
	git("commit", "--quiet", "-am", "head one")
	write("run.sh", "#!/bin/sh\necho run\n", 0o755)
	write("go.sum", "example.com/a v1.0.0 h1:two\n", 0o644)
	write("src/testdata/in.txt", "input two\n", 0o644)
	git("commit", "--quiet", "-am", "head two")
	head := strings.TrimSpace(git("rev-parse", "HEAD"))
	git("switch", "--quiet", "main")
	for i := 1; i <= 2; i++ {
		write("base.txt", fmt.Sprintf("base %d\n", i), 0o644)
		write(fmt.Sprintf("base%d.go", i), "package base\n", 0o644)
		git("add", ".")
		git("commit", "--quiet", "-m", fmt.Sprintf("base %d", i))
	}
	base := strings.TrimSpace(git("rev-parse", "HEAD"))
	cleanGit(t, home, root, "init", "--quiet", "--bare", "--initial-branch=main", origin)
	git("push", "--quiet", origin, "main", "feature")
	cleanGit(t, home, root, "clone", "--quiet", "--depth", "1", "--branch", "main", "file://"+origin, caller)
	if shallow := cleanGit(t, home, caller, "rev-parse", "--is-shallow-repository"); strings.TrimSpace(shallow) != "true" {
		t.Fatalf("caller is not shallow: %s", shallow)
	}
	return largePull{origin: origin, caller: caller, base: base, head: head}
}

// makeHostile points every configuration and attribute source the caller's git reads at settings that change a diff, and makes the caller the working directory.
func makeHostile(t *testing.T, caller string) {
	t.Helper()
	hostile := t.TempDir()
	config := "[diff]\n\tnoprefix = true\n\tmnemonicPrefix = true\n\tcontext = 0\n[diff \"conv\"]\n\ttextconv = sed s/^/CONVERTED:/\n[core]\n\tattributesFile = " + filepath.Join(hostile, "attributes") + "\n"
	attributes := "src/conv.go diff=conv\nsrc/text.go -diff\n"
	files := map[string]string{
		filepath.Join(hostile, ".gitconfig"):                config,
		filepath.Join(hostile, "attributes"):                attributes,
		filepath.Join(hostile, "xdg", "git", "config"):      config,
		filepath.Join(hostile, "xdg", "git", "attributes"):  attributes,
		filepath.Join(caller, ".git", "info", "attributes"): attributes,
		filepath.Join(hostile, "external"):                  "#!/bin/sh\necho EXTERNAL\n",
	}
	for name, content := range files {
		if err := os.MkdirAll(filepath.Dir(name), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(name, []byte(content), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	f, err := os.OpenFile(filepath.Join(caller, ".git", "config"), os.O_APPEND|os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := f.WriteString(config); err != nil {
		t.Fatal(err)
	}
	if err := f.Close(); err != nil {
		t.Fatal(err)
	}
	t.Setenv("HOME", hostile)
	t.Setenv("XDG_CONFIG_HOME", filepath.Join(hostile, "xdg"))
	t.Setenv("GIT_CONFIG_NOSYSTEM", "")
	t.Setenv("GIT_DIFF_OPTS", "--unified=1")
	t.Setenv("GIT_EXTERNAL_DIFF", filepath.Join(hostile, "external"))
	t.Setenv("GIT_CONFIG_COUNT", "2")
	t.Setenv("GIT_CONFIG_KEY_0", "diff.noprefix")
	t.Setenv("GIT_CONFIG_VALUE_0", "true")
	t.Setenv("GIT_CONFIG_KEY_1", "diff.renames")
	t.Setenv("GIT_CONFIG_VALUE_1", "false")
	t.Chdir(caller)
}

// originGH answers pr view with the frozen base and head, refuses pr diff as too large, and reads files at the head from the origin.
func originGH(t *testing.T, p largePull) ghFunc {
	home := t.TempDir()
	const filesPrefix = "repos/shgew/togi/contents/"
	return func(args ...string) ([]byte, error) {
		switch {
		case args[0] == "pr" && args[1] == "view":
			return fmt.Appendf(nil, `{"headRefOid":%q,"baseRefOid":%q,"url":%q}`, p.head, p.base, prURL), nil
		case args[0] == "pr" && args[1] == "diff":
			return nil, errors.New("gh pr: could not find pull request diff: HTTP 406: Sorry, the diff exceeded the maximum number of lines (20000) (https://api.github.com/repos/shgew/togi/pulls/700)\nPullRequest.diff too_large")
		case len(args) == 4 && args[0] == "api" && strings.HasPrefix(args[3], filesPrefix):
			name, ref, _ := strings.Cut(strings.TrimPrefix(args[3], filesPrefix), "?ref=")
			name, err := neturl.PathUnescape(name)
			if err != nil {
				return nil, err
			}
			return []byte(cleanGit(t, home, p.origin, "show", ref+":"+name)), nil
		}
		return nil, fmt.Errorf("unexpected gh %v", args)
	}
}

func TestSnapshotTooLargeDiffIsolatedFromCaller(t *testing.T) {
	p := newLargePull(t)
	want := cleanGit(t, t.TempDir(), p.origin, "diff", "-M", p.base+"..."+p.head)
	for _, s := range []string{"rename from docs/old.md", "old mode 100644", "-\treturn 2\n+\treturn 22\n", "-const Name = \"before\"\n+const Name = \"after\"\n"} {
		if !strings.Contains(want, s) {
			t.Fatalf("the reference diff lacks %q:\n%s", s, want)
		}
	}
	if strings.Contains(want, "base") {
		t.Fatalf("the reference diff holds base-only changes:\n%s", want)
	}
	makeHostile(t, p.caller)
	var stdout, stderr bytes.Buffer
	tl := tools{
		gh: originGH(t, p), git: runGit, isolatedGit: runIsolatedGit,
		getenv: func(string) string { return "" }, tmp: t.TempDir(),
		stdin: strings.NewReader(""), stdout: &stdout, stderr: &stderr,
	}
	if err := runSnapshot(tl, []string{"700"}); err != nil {
		t.Fatal(err)
	}
	dir := filepath.Join(tl.tmp, "togi-review", "700-"+p.head[:8])
	if diff := cmp.Diff(want, readFile(t, filepath.Join(dir, "pr.diff"))); diff != "" {
		t.Errorf("pr.diff differs from the origin's merge-base diff (-want +got):\n%s", diff)
	}
	var m manifest
	if err := json.Unmarshal([]byte(readFile(t, filepath.Join(dir, "manifest.json"))), &m); err != nil {
		t.Fatal(err)
	}
	sections, err := parseDiff(want)
	if err != nil {
		t.Fatal(err)
	}
	type entry struct{ Path, OldPath, Status, Exclusion string }
	var got []entry
	for i, f := range m.Files {
		got = append(got, entry{f.Path, f.OldPath, f.Status, f.Exclusion})
		hunk := filepath.Join(dir, hunkFileName(i+1))
		if f.Exclusion != "" {
			if _, err := os.Stat(hunk); !os.IsNotExist(err) {
				t.Errorf("excluded file %s has a hunk file: %v", f.Path, err)
			}
			continue
		}
		if f.HunkFile != hunkFileName(i+1) || readFile(t, hunk) != sections[i].Text {
			t.Errorf("%s's hunk file is not its section of the origin's diff", f.Path)
		}
	}
	wantFiles := []entry{
		{"docs/new.md", "docs/old.md", "renamed", ""},
		{"go.sum", "", "modified", "generated"},
		{"run.sh", "", "modified", ""},
		{"src/conv.go", "", "modified", ""},
		{"src/testdata/in.txt", "", "modified", "test data (`**/testdata/**`)"},
		{"src/text.go", "", "modified", ""},
	}
	if diff := cmp.Diff(wantFiles, got); diff != "" {
		t.Errorf("manifest files (-want +got):\n%s", diff)
	}
	if m.IncludedFiles != 4 || m.IncludedLines != 4 || m.Reviewers != reviewerCount(4, 4) {
		t.Errorf("manifest totals: %d files, %d lines, %d reviewers; want 4, 4, %d", m.IncludedFiles, m.IncludedLines, m.Reviewers, reviewerCount(4, 4))
	}
	entries, err := os.ReadDir(tl.tmp)
	if err != nil {
		t.Fatal(err)
	}
	for _, e := range entries {
		if e.Name() != "togi-review" {
			t.Errorf("snapshot left %s in its temporary directory", e.Name())
		}
	}
}
