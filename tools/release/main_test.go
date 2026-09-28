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
	checked   []string
	checkErr  error
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

func releaseRunner(git *fakeGit, out *bytes.Buffer, commit bool) runner {
	return runner{
		git:    git.run,
		now:    func() time.Time { return time.Date(2026, 9, 26, 0, 0, 0, 0, time.UTC) },
		out:    out,
		commit: commit,
		requireGreen: func(commit string) error {
			git.checked = append(git.checked, commit)
			return git.checkErr
		},
	}
}

var readMain = []string{
	"fetch --quiet origin main",
	"rev-parse --verify origin/main^{commit}",
	"show base:version.txt",
	"show base:CHANGELOG.md",
	"show base:go.mod",
}

func TestRelease(t *testing.T) {
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
			var out bytes.Buffer
			if err := releaseRunner(git, &out, true).release(); err != nil {
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
				"checkout --quiet --detach commit",
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
			if got, want := out.String(), "Release "+tc.next+" committed as commit on top of origin/main\n"; got != want {
				t.Fatalf("output = %q", got)
			}
			if diff := cmp.Diff([]string{"base"}, git.checked); diff != "" {
				t.Fatalf("green check mismatch (-want +got):\n%s", diff)
			}
		})
	}
}

func TestReleaseRefusesWithoutGreenCheck(t *testing.T) {
	t.Parallel()
	git := mainGit("0.1.0", unreleased, "sha\trefs/tags/v0.1.0\n")
	git.checkErr = errors.New("check run u for base concluded failure")
	var out bytes.Buffer
	err := releaseRunner(git, &out, true).release()
	if got, want := fmt.Sprint(err), "require a passing check on origin/main: check run u for base concluded failure"; got != want {
		t.Fatalf("release error = %q, want %q", got, want)
	}
	if diff := cmp.Diff(append(append([]string{}, readMain...), "ls-remote --tags origin refs/tags/v*"), git.commands()); diff != "" {
		t.Fatalf("git commands mismatch (-want +got):\n%s", diff)
	}
}

func TestReleaseNothingToRelease(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name   string
		commit bool
		err    string
		output string
	}{
		{name: "preview", output: "nothing to release\n"},
		{name: "commit", commit: true, err: "nothing to release: [Unreleased] in CHANGELOG.md is empty"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			git := mainGit("0.1.0", released, "sha\trefs/tags/v0.1.0\n")
			var out bytes.Buffer
			err := releaseRunner(git, &out, tc.commit).release()
			if got := fmt.Sprint(err); tc.err != "" && got != tc.err || tc.err == "" && err != nil {
				t.Fatalf("release error = %v, want %q", err, tc.err)
			}
			if diff := cmp.Diff(append(append([]string{}, readMain...), "ls-remote --tags origin refs/tags/v*"), git.commands()); diff != "" {
				t.Fatalf("git commands mismatch (-want +got):\n%s", diff)
			}
			if got := out.String(); got != tc.output {
				t.Fatalf("output = %q", got)
			}
		})
	}
}

func TestReleaseResumesUnpublished(t *testing.T) {
	t.Parallel()
	git := mainGit("0.1.0", released, "sha\trefs/tags/v0.0.9\n")
	var out bytes.Buffer
	if err := releaseRunner(git, &out, true).release(); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(append(append([]string{}, readMain...), "ls-remote --tags origin refs/tags/v*", "checkout --quiet --detach base"), git.commands()); diff != "" {
		t.Fatalf("git commands mismatch (-want +got):\n%s", diff)
	}
	if got, want := out.String(), "0.1.0 is released in CHANGELOG.md but not yet published; publishing origin/main as it is\n"; got != want {
		t.Fatalf("output = %q", got)
	}
	if git.checked != nil {
		t.Fatalf("resume required a green check for %v", git.checked)
	}
}

func TestReleasePreview(t *testing.T) {
	t.Parallel()
	git := mainGit("0.1.0", unreleased, "sha\trefs/tags/v0.1.0\n")
	var out bytes.Buffer
	if err := releaseRunner(git, &out, false).release(); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(append(append([]string{}, readMain...), "ls-remote --tags origin refs/tags/v*"), git.commands()); diff != "" {
		t.Fatalf("git commands mismatch (-want +got):\n%s", diff)
	}
	want := "Would release from origin/main:\n\nRelease 0.1.1\n\nNo breaking changes (patch bump).\n\n### Fixed\n\n- A fix ([#35]).\n\n[#35]: https://forge.example/o/r/pulls/35\n"
	if diff := cmp.Diff(want, out.String()); diff != "" {
		t.Fatalf("output mismatch (-want +got):\n%s", diff)
	}
	if git.checked != nil {
		t.Fatalf("preview required a green check for %v", git.checked)
	}
}

func TestRequireGreenCheck(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name, runs, err string
	}{
		{name: "success", runs: `[{"html_url":"u","status":"completed","conclusion":"success"}]`},
		{name: "in progress", runs: `[{"html_url":"u","status":"in_progress","conclusion":null}]`, err: "check run u for base is in_progress; run just release again once it passes"},
		{name: "failure", runs: `[{"html_url":"u","status":"completed","conclusion":"failure"}]`, err: "check run u for base concluded failure"},
		{name: "cancelled", runs: `[{"html_url":"u","status":"completed","conclusion":"cancelled"}]`, err: "check run u for base concluded cancelled"},
		{name: "no run", runs: `[]`, err: "no check.yml run on main for base"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			var requests []string
			handler := http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
				requests = append(requests, req.Method+" "+req.URL.RequestURI())
				fmt.Fprint(w, `{"total_count":1,"workflow_runs":`+tc.runs+`}`)
			})
			api := github{base: "https://api.forge.example", token: "test-token", client: &http.Client{Transport: handlerTransport{handler}}}
			err := requireGreenCheck(api, repository{"o", "r"}, "base")
			if got := fmt.Sprint(err); tc.err != "" && got != tc.err || tc.err == "" && err != nil {
				t.Fatalf("requireGreenCheck error = %v, want %q", err, tc.err)
			}
			want := []string{"GET /repos/o/r/actions/workflows/check.yml/runs?branch=main&event=push&head_sha=base&per_page=1"}
			if diff := cmp.Diff(want, requests); diff != "" {
				t.Fatalf("requests mismatch (-want +got):\n%s", diff)
			}
		})
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

func publishFixture(t *testing.T, version, changelog string, tagStatus int) (*fakeGit, github, *[]string, *map[string]string) {
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
		if got := req.Header.Get("Authorization"); got != "Bearer test-token" {
			t.Errorf("Authorization = %q", got)
		}
		switch req.Method + " " + req.URL.Path {
		case "GET /repos/o/r/git/ref/tags/v" + version:
			w.WriteHeader(tagStatus)
			fmt.Fprint(w, `{}`)
		case "POST /repos/o/r/releases":
			if err := json.NewDecoder(req.Body).Decode(posted); err != nil {
				t.Error(err)
			}
			w.WriteHeader(http.StatusCreated)
			fmt.Fprint(w, `{"html_url":"https://forge.example/o/r/releases/tag/v`+version+`"}`)
		default:
			t.Errorf("unexpected request %s %s", req.Method, req.URL)
		}
	})
	api := github{base: "https://api.forge.example", token: "test-token", client: &http.Client{Transport: handlerTransport{handler}}}
	return git, api, requests, posted
}

func TestPublish(t *testing.T) {
	t.Parallel()
	git, api, requests, posted := publishFixture(t, "0.1.0", released, http.StatusNotFound)
	var out bytes.Buffer
	if err := releaseRunner(git, &out, false).publish(api, repository{"o", "r"}); err != nil {
		t.Fatal(err)
	}
	wantPost := map[string]string{
		"tag_name": "v0.1.0", "target_commitish": "merge-sha", "name": "0.1.0",
		"body": "### Added\n\n- New option ([#34]).\n\n[#34]: https://forge.example/o/r/pulls/34",
	}
	if diff := cmp.Diff(wantPost, *posted); diff != "" {
		t.Fatalf("release payload mismatch (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff([]string{"GET /repos/o/r/git/ref/tags/v0.1.0", "POST /repos/o/r/releases"}, *requests); diff != "" {
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
		{"already published", "0.1.0", released, http.StatusOK, []string{"GET /repos/o/r/git/ref/tags/v0.1.0"}, "nothing to publish: v0.1.0 already exists\n"},
		{"not released yet", "0.1.0", firstUnreleased, http.StatusNotFound, nil, "nothing to publish: CHANGELOG.md has no released [0.1.0] section\n"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			git, api, requests, posted := publishFixture(t, tc.version, tc.changelog, tc.tagStatus)
			var out bytes.Buffer
			if err := releaseRunner(git, &out, false).publish(api, repository{"o", "r"}); err != nil {
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
