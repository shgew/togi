package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
)

const unreleased = "## [Unreleased]\n\n### Fixed\n\n- A fix ([#35]).\n\n## [0.1.0] - 2026-09-25\n\n### Added\n\n- New option ([#34]).\n\n[0.1.0]: https://forge.example/o/r/releases/tag/v0.1.0\n\n[#34]: https://forge.example/o/r/pulls/34\n[#35]: https://forge.example/o/r/pulls/35\n"
const firstUnreleased = "## [Unreleased]\n\n### Added\n\n- New option ([#34]).\n\n[#34]: https://forge.example/o/r/pulls/34\n"
const released = "## [Unreleased]\n\n## [0.1.0] - 2026-09-25\n\n### Added\n\n- New option ([#34]).\n\n[0.1.0]: https://forge.example/o/r/releases/tag/v0.1.0\n\n[#34]: https://forge.example/o/r/pulls/34\n"

type fakeGit struct {
	responses map[string]string
	failures  map[string]error
	stderr    map[string]string
	calls     []gitCmd
}

func (f *fakeGit) run(c gitCmd) (string, string, error) {
	f.calls = append(f.calls, c)
	key := strings.Join(c.args, " ")
	return f.responses[key], f.stderr[key], f.failures[key]
}

func (f *fakeGit) commands() []string {
	var got []string
	for _, c := range f.calls {
		got = append(got, strings.Join(c.args, " "))
	}
	return got
}

func (f *fakeGit) stdin(command string) []string {
	var got []string
	for _, c := range f.calls {
		if strings.Join(c.args, " ") == command {
			got = append(got, c.stdin)
		}
	}
	return got
}

func mainGit(version, changelog, tags string) *fakeGit {
	return &fakeGit{
		responses: map[string]string{
			"rev-parse --verify origin/main^{commit}":    "base\n",
			"show base:version.txt":                      version + "\n",
			"show base:CHANGELOG.md":                     changelog,
			"show base:go.mod":                           "module forge.example/o/r\n\ngo 1.27\n",
			"ls-remote --tags origin refs/tags/v*":       tags,
			"rev-parse --git-path shycler-release-index": "index\n",
			"hash-object -w --stdin":                     "blob\n",
			"write-tree":                                 "tree\n",
			"commit-tree tree -p base -F -":              "commit\n",
		},
		failures: map[string]error{},
		stderr:   map[string]string{},
	}
}

func prepareRunner(git *fakeGit, out *bytes.Buffer, dryRun bool) runner {
	return runner{
		git:    git.run,
		now:    func() time.Time { return time.Date(2026, 9, 26, 0, 0, 0, 0, time.UTC) },
		out:    out,
		dryRun: dryRun,
	}
}

var readMain = []string{
	"fetch --quiet origin main",
	"rev-parse --verify origin/main^{commit}",
	"show base:version.txt",
	"show base:CHANGELOG.md",
	"show base:go.mod",
}

func TestPrepare(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name, version, changelog, tags string
		next, reason, changelogOut     string
	}{
		{
			name: "bump", version: "0.1.0", changelog: unreleased, tags: "sha\trefs/tags/v0.1.0\n",
			next: "0.1.1", reason: "No breaking changes (patch bump).",
			changelogOut: "## [Unreleased]\n\n## [0.1.1] - 2026-09-26\n\n### Fixed\n\n- A fix ([#35]).\n\n## [0.1.0] - 2026-09-25\n\n### Added\n\n- New option ([#34]).\n\n[0.1.0]: https://forge.example/o/r/releases/tag/v0.1.0\n\n[0.1.1]: https://forge.example/o/r/releases/tag/v0.1.1\n\n[#34]: https://forge.example/o/r/pulls/34\n[#35]: https://forge.example/o/r/pulls/35\n",
		},
		{
			name: "first release", version: "0.1.0", changelog: firstUnreleased,
			next: "0.1.0", reason: "First release; version.txt sets the initial version.",
			changelogOut: "## [Unreleased]\n\n## [0.1.0] - 2026-09-26\n\n### Added\n\n- New option ([#34]).\n\n[0.1.0]: https://forge.example/o/r/releases/tag/v0.1.0\n\n[#34]: https://forge.example/o/r/pulls/34\n",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			git := mainGit(tc.version, tc.changelog, tc.tags)
			push := "push origin commit:refs/for/main -o topic=release"
			git.stderr[push] = "remote:   https://forge.example/o/r/pulls/60\n"
			var out bytes.Buffer
			if err := prepareRunner(git, &out, false).prepare(); err != nil {
				t.Fatal(err)
			}
			want := append(append([]string{}, readMain...),
				"ls-remote --tags origin refs/tags/v*",
				"rev-parse --git-path shycler-release-index",
				"read-tree base",
				"hash-object -w --stdin",
				"update-index --cacheinfo 100644,blob,version.txt",
				"hash-object -w --stdin",
				"update-index --cacheinfo 100644,blob,CHANGELOG.md",
				"write-tree",
				"commit-tree tree -p base -F -",
				push,
			)
			if diff := cmp.Diff(want, git.commands()); diff != "" {
				t.Fatalf("git commands mismatch (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff([]string{tc.next + "\n", tc.changelogOut}, git.stdin("hash-object -w --stdin")); diff != "" {
				t.Fatalf("committed files mismatch (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff([]string{"Release " + tc.next + "\n\n" + tc.reason + "\n"}, git.stdin("commit-tree tree -p base -F -")); diff != "" {
				t.Fatalf("commit message mismatch (-want +got):\n%s", diff)
			}
			for _, c := range git.calls[len(readMain)+2 : len(git.calls)-1] {
				if diff := cmp.Diff([]string{"GIT_INDEX_FILE=index"}, c.env); diff != "" {
					t.Fatalf("%s env mismatch (-want +got):\n%s", strings.Join(c.args, " "), diff)
				}
			}
			if got := out.String(); got != git.stderr[push] {
				t.Fatalf("output = %q", got)
			}
		})
	}
}

func TestPrepareRefusesWhileReleaseOpen(t *testing.T) {
	t.Parallel()
	git := mainGit("0.1.0", unreleased, "sha\trefs/tags/v0.1.0\n")
	push := "push origin commit:refs/for/main -o topic=release"
	git.failures[push] = errors.New("exit status 1")
	git.stderr[push] = " ! [remote rejected] commit -> refs/for/main (Updates were rejected because the tip of your current branch is behind its remote counterpart. If this is intentional, set the `force-push` option by adding `-o force-push=true` to your `git push` command.)\n"
	var out bytes.Buffer
	err := prepareRunner(git, &out, false).prepare()
	if err == nil || !strings.Contains(err.Error(), "a release pull request is already open") {
		t.Fatalf("prepare error = %v", err)
	}
}

func TestPrepareNothingToRelease(t *testing.T) {
	t.Parallel()
	git := mainGit("0.1.0", released, "sha\trefs/tags/v0.1.0\n")
	var out bytes.Buffer
	if err := prepareRunner(git, &out, false).prepare(); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(readMain, git.commands()); diff != "" {
		t.Fatalf("git commands mismatch (-want +got):\n%s", diff)
	}
	if got := out.String(); got != "nothing to release\n" {
		t.Fatalf("output = %q", got)
	}
}

func TestPrepareDryRun(t *testing.T) {
	t.Parallel()
	git := mainGit("0.1.0", unreleased, "sha\trefs/tags/v0.1.0\n")
	var out bytes.Buffer
	if err := prepareRunner(git, &out, true).prepare(); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(append(append([]string{}, readMain...), "ls-remote --tags origin refs/tags/v*"), git.commands()); diff != "" {
		t.Fatalf("git commands mismatch (-want +got):\n%s", diff)
	}
	want := "Would open a release pull request from origin/main:\n\nRelease 0.1.1\n\nNo breaking changes (patch bump).\n\n### Fixed\n\n- A fix ([#35]).\n\n[#35]: https://forge.example/o/r/pulls/35\n"
	if diff := cmp.Diff(want, out.String()); diff != "" {
		t.Fatalf("output mismatch (-want +got):\n%s", diff)
	}
}

type handlerTransport struct{ handler http.Handler }

func (h handlerTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	r := req.Clone(req.Context())
	if r.Body == nil {
		r.Body = http.NoBody
	}
	rec := httptest.NewRecorder()
	h.handler.ServeHTTP(rec, r)
	res := rec.Result()
	res.Request = req
	return res, nil
}

func publishFixture(t *testing.T, version, changelog string, tagStatus int) (*fakeGit, forgejo, *[]string, *map[string]string) {
	t.Helper()
	git := &fakeGit{responses: map[string]string{
		"show HEAD:version.txt":                                 version + "\n",
		"show HEAD:CHANGELOG.md":                                changelog,
		"log --first-parent -1 --format=%H HEAD -- version.txt": "merge-sha\n",
	}}
	requests := new([]string)
	posted := new(map[string]string)
	handler := http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		*requests = append(*requests, req.Method+" "+req.URL.RequestURI())
		if got := req.Header.Get("Authorization"); got != "token test-token" {
			t.Errorf("Authorization = %q", got)
		}
		switch req.Method + " " + req.URL.Path {
		case "GET /api/v1/repos/o/r/tags/v" + version:
			w.WriteHeader(tagStatus)
			fmt.Fprint(w, `{}`)
		case "POST /api/v1/repos/o/r/releases":
			if err := json.NewDecoder(req.Body).Decode(posted); err != nil {
				t.Error(err)
			}
			w.WriteHeader(http.StatusCreated)
			fmt.Fprint(w, `{"html_url":"https://forge.example/o/r/releases/tag/v`+version+`"}`)
		default:
			t.Errorf("unexpected request %s %s", req.Method, req.URL)
		}
	})
	api := forgejo{base: "https://forge.example/api/v1", token: "test-token", client: &http.Client{Transport: handlerTransport{handler}}}
	return git, api, requests, posted
}

func TestPublish(t *testing.T) {
	t.Parallel()
	git, api, requests, posted := publishFixture(t, "0.1.0", released, http.StatusNotFound)
	var out bytes.Buffer
	if err := prepareRunner(git, &out, false).publish(api, repository{"o", "r"}); err != nil {
		t.Fatal(err)
	}
	wantPost := map[string]string{
		"tag_name": "v0.1.0", "target_commitish": "merge-sha", "name": "0.1.0",
		"body": "### Added\n\n- New option ([#34]).\n\n[#34]: https://forge.example/o/r/pulls/34",
	}
	if diff := cmp.Diff(wantPost, *posted); diff != "" {
		t.Fatalf("release payload mismatch (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff([]string{"GET /api/v1/repos/o/r/tags/v0.1.0", "POST /api/v1/repos/o/r/releases"}, *requests); diff != "" {
		t.Fatalf("requests mismatch (-want +got):\n%s", diff)
	}
	if got := out.String(); got != "https://forge.example/o/r/releases/tag/v0.1.0\n" {
		t.Fatalf("output = %q", got)
	}
}

func TestPublishSkips(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name, version, changelog string
		tagStatus                int
		requests                 []string
		output                   string
	}{
		{"already published", "0.1.0", released, http.StatusOK, []string{"GET /api/v1/repos/o/r/tags/v0.1.0"}, "nothing to publish: v0.1.0 already exists\n"},
		{"not released yet", "0.1.0", firstUnreleased, http.StatusNotFound, nil, "nothing to publish: CHANGELOG.md has no released [0.1.0] section\n"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			git, api, requests, posted := publishFixture(t, tc.version, tc.changelog, tc.tagStatus)
			var out bytes.Buffer
			if err := prepareRunner(git, &out, false).publish(api, repository{"o", "r"}); err != nil {
				t.Fatal(err)
			}
			if *posted != nil {
				t.Fatalf("published %v", *posted)
			}
			if diff := cmp.Diff(tc.requests, *requests); diff != "" {
				t.Fatalf("requests mismatch (-want +got):\n%s", diff)
			}
			if got := out.String(); got != tc.output {
				t.Fatalf("output = %q", got)
			}
		})
	}
}
