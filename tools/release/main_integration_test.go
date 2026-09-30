//go:build integration

package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
)

type releaseRepository struct {
	dir, remote string
	git         gitFunc
}

func temporaryReleaseRepository(t *testing.T, changelog string) releaseRepository {
	t.Helper()
	root := t.TempDir()
	repo := releaseRepository{dir: filepath.Join(root, "checkout"), remote: filepath.Join(root, "remote.git")}
	if err := os.Mkdir(repo.dir, 0o700); err != nil {
		t.Fatal(err)
	}
	repo.git = func(c gitCmd) (string, string, error) {
		cmd := exec.Command("git", c.args...)
		cmd.Dir = repo.dir
		cmd.Env = append(os.Environ(), "GIT_CONFIG_NOSYSTEM=1", "GIT_CONFIG_GLOBAL=/dev/null", "GIT_AUTHOR_NAME=Release Test", "GIT_AUTHOR_EMAIL=release@example.com", "GIT_COMMITTER_NAME=Release Test", "GIT_COMMITTER_EMAIL=release@example.com")
		cmd.Env = append(cmd.Env, c.env...)
		cmd.Stdin = strings.NewReader(c.stdin)
		var stdout, stderr bytes.Buffer
		cmd.Stdout, cmd.Stderr = &stdout, &stderr
		err := cmd.Run()
		if err != nil {
			err = fmt.Errorf("git %s: %w: %s", strings.Join(c.args, " "), err, stderr.String())
		}
		return stdout.String(), stderr.String(), err
	}
	repo.command(t, "init", "--bare", "--initial-branch=main", repo.remote)
	repo.command(t, "init", "--initial-branch=main")
	repo.command(t, "remote", "add", "origin", repo.remote)
	repo.write(t, "version.txt", "0.0.9\n")
	repo.write(t, "CHANGELOG.md", changelog)
	repo.write(t, "go.mod", "module forge.example/o/r\n\ngo 1.27\n")
	repo.command(t, "add", ".")
	repo.command(t, "commit", "-m", "Previous version")
	repo.write(t, "version.txt", "0.1.0\n")
	repo.command(t, "add", "version.txt")
	repo.command(t, "commit", "-m", "Release 0.1.0")
	repo.command(t, "push", "origin", "HEAD:refs/heads/main")
	return repo
}

func (repo releaseRepository) command(t *testing.T, args ...string) string {
	t.Helper()
	out, _, err := repo.git(gitCmd{args: args})
	if err != nil {
		t.Fatal(err)
	}
	return strings.TrimSpace(out)
}

func (repo releaseRepository) write(t *testing.T, path, content string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(repo.dir, path), []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
}

func (repo releaseRepository) runner(out *bytes.Buffer, commit bool) runner {
	return runner{git: repo.git, out: out, commit: commit, now: func() time.Time { return time.Date(2026, 9, 26, 0, 0, 0, 0, time.UTC) }, requireGreen: func(string) error { return nil }}
}

func TestPublishTemporaryRepository(t *testing.T) {
	for _, tc := range []struct {
		name                           string
		tag, annotated, wrong, release bool
	}{
		{name: "no tag"},
		{name: "tag only", tag: true},
		{name: "annotated tag only", tag: true, annotated: true},
		{name: "Release present", tag: true, release: true},
		{name: "wrong target", tag: true, wrong: true},
		{name: "wrong annotated target", tag: true, annotated: true, wrong: true},
		{name: "wrong target with Release", tag: true, wrong: true, release: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			repo := temporaryReleaseRepository(t, released)
			commit := repo.command(t, "rev-parse", "HEAD")
			target := commit
			if tc.wrong {
				target = repo.command(t, "rev-parse", "HEAD^")
			}
			if tc.tag {
				args := []string{"tag", "v0.1.0", target}
				if tc.annotated {
					args = []string{"tag", "-a", "v0.1.0", target, "-m", "Release"}
				}
				repo.command(t, args...)
				repo.command(t, "push", "origin", "refs/tags/v0.1.0:refs/tags/v0.1.0")
			}
			var posted map[string]string
			handler := http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
				switch req.Method + " " + req.URL.Path {
				case "GET /repos/o/r/releases/tags/v0.1.0":
					if !tc.release {
						w.WriteHeader(http.StatusNotFound)
					}
					fmt.Fprint(w, `{}`)
				case "POST /repos/o/r/releases":
					if err := json.NewDecoder(req.Body).Decode(&posted); err != nil {
						t.Error(err)
					}
					w.WriteHeader(http.StatusCreated)
					fmt.Fprint(w, `{"html_url":"https://forge.example/o/r/releases/tag/v0.1.0"}`)
				default:
					t.Errorf("unexpected request: %s %s", req.Method, req.URL)
				}
			})
			api := github{base: "https://api.forge.example", token: "test-token", client: &http.Client{Transport: handlerTransport{handler}}}
			var out bytes.Buffer
			err := repo.runner(&out, false).publish(api, repository{"o", "r"})
			if tc.wrong {
				if err == nil || !strings.Contains(err.Error(), target) || !strings.Contains(err.Error(), commit) {
					t.Fatalf("wrong-target error = %v; want both %s and %s", err, target, commit)
				}
				if posted != nil {
					t.Fatalf("published wrong target: %v", posted)
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			if tc.release {
				if posted != nil {
					t.Fatalf("duplicated Release: %v", posted)
				}
				return
			}
			want := map[string]string{"tag_name": "v0.1.0", "target_commitish": commit, "name": "0.1.0", "body": "### Added\n\n- New option ([#34]).\n\n[#34]: https://forge.example/o/r/pulls/34"}
			if diff := cmp.Diff(want, posted); diff != "" {
				t.Fatalf("release mismatch (-want +got):\n%s", diff)
			}
			t.Log(strings.TrimSpace(out.String()))
		})
	}
}

func TestRecoveryPreviewPreservesCheckout(t *testing.T) {
	repo := temporaryReleaseRepository(t, released)
	repo.command(t, "switch", "-c", "work")
	repo.write(t, "staged.txt", "staged\n")
	repo.command(t, "add", "staged.txt")
	repo.write(t, "staged.txt", "unstaged\n")
	repo.write(t, "untracked.txt", "untracked\n")
	snapshot := func() []string {
		t.Helper()
		index, err := os.ReadFile(filepath.Join(repo.dir, ".git", "index"))
		if err != nil {
			t.Fatal(err)
		}
		staged, err := os.ReadFile(filepath.Join(repo.dir, "staged.txt"))
		if err != nil {
			t.Fatal(err)
		}
		untracked, err := os.ReadFile(filepath.Join(repo.dir, "untracked.txt"))
		if err != nil {
			t.Fatal(err)
		}
		return []string{repo.command(t, "symbolic-ref", "HEAD"), repo.command(t, "rev-parse", "HEAD"), string(index), repo.command(t, "diff", "--binary", "HEAD"), repo.command(t, "status", "--porcelain=v1"), string(staged), string(untracked)}
	}
	before := snapshot()
	var out bytes.Buffer
	if err := repo.runner(&out, false).release(); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(before, snapshot()); diff != "" {
		t.Fatalf("preview changed checkout (-before +after):\n%s", diff)
	}
	t.Log(strings.TrimSpace(out.String()))
}

func TestReleaseWithoutTagsBumpsVersion(t *testing.T) {
	repo := temporaryReleaseRepository(t, firstUnreleased)
	var out bytes.Buffer
	if err := repo.runner(&out, true).release(); err != nil {
		t.Fatal(err)
	}
	if got := repo.command(t, "show", "HEAD:version.txt"); got != "0.1.1" {
		t.Fatalf("release version = %s, want 0.1.1", got)
	}
	if got := repo.command(t, "log", "-1", "--format=%s"); got != "Release 0.1.1" {
		t.Fatalf("release message = %s", got)
	}
	t.Log(strings.TrimSpace(out.String()))
}

func TestReleaseResumesTagOnlyRepository(t *testing.T) {
	repo := temporaryReleaseRepository(t, released)
	commit := repo.command(t, "rev-parse", "HEAD")
	repo.command(t, "tag", "v0.1.0")
	repo.command(t, "push", "origin", "refs/tags/v0.1.0:refs/tags/v0.1.0")
	repo.command(t, "switch", "-c", "work")
	repo.write(t, "later.txt", "later\n")
	repo.command(t, "add", "later.txt")
	repo.command(t, "commit", "-m", "Later work")
	var out bytes.Buffer
	if err := repo.runner(&out, true).release(); err != nil {
		t.Fatal(err)
	}
	if got := repo.command(t, "rev-parse", "HEAD"); got != commit {
		t.Fatalf("retry HEAD = %s, want existing release commit %s", got, commit)
	}
	t.Log(strings.TrimSpace(out.String()))
}

func TestPublishTagOnlyFromShallowLaterCommit(t *testing.T) {
	repo := temporaryReleaseRepository(t, released)
	commit := repo.command(t, "rev-parse", "HEAD")
	repo.command(t, "tag", "v0.1.0")
	repo.command(t, "push", "origin", "refs/tags/v0.1.0:refs/tags/v0.1.0")
	repo.write(t, "later.txt", "later\n")
	repo.command(t, "add", "later.txt")
	repo.command(t, "commit", "-m", "Later work")
	repo.command(t, "push", "origin", "HEAD:refs/heads/main")
	clone := filepath.Join(t.TempDir(), "shallow")
	repo.command(t, "clone", "--depth=1", "file://"+repo.remote, clone)
	baseGit := repo.git
	repo.git = func(c gitCmd) (string, string, error) {
		c.args = append([]string{"-C", clone}, c.args...)
		return baseGit(c)
	}
	if got := repo.command(t, "rev-parse", "--is-shallow-repository"); got != "true" {
		t.Fatalf("fixture shallow = %s", got)
	}
	_, api, _, posted := publishFixture(t, "0.1.0", released, http.StatusNotFound)
	var out bytes.Buffer
	if err := repo.runner(&out, false).publish(api, repository{"o", "r"}); err != nil {
		t.Fatal(err)
	}
	if got := (*posted)["target_commitish"]; got != commit {
		t.Fatalf("published target = %s, want original release commit %s", got, commit)
	}
	t.Log(strings.TrimSpace(out.String()))
}
