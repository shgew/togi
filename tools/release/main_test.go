package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
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
			"rev-parse --verify origin/main^{commit}": "base\n",
			"show base:version.txt":                   version + "\n",
			"show base:CHANGELOG.md":                  changelog,
			"show base:go.mod":                        "module forge.example/o/r\n\ngo 1.27\n",
			"ls-remote --tags origin refs/tags/v*":    tags,
			"rev-parse --git-path togi-release-index": "index\n",
			"hash-object -w --stdin":                  "blob\n",
			"write-tree":                              "tree\n",
			"commit-tree tree -p base -F -":           "commit\n",
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
			name: "bump without tags", version: "0.1.0", changelog: firstUnreleased,
			next: "0.1.1", reason: "No breaking changes (patch bump).",
			changelogOut: "## [Unreleased]\n\n## [0.1.1] - 2026-09-26\n\n### Added\n\n- New option ([#34]).\n\n[0.1.1]: https://forge.example/o/r/releases/tag/v0.1.1\n\n[#34]: https://forge.example/o/r/pulls/34\n",
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
				"rev-parse --git-path togi-release-index",
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
			changelog := released
			if tc.commit {
				changelog = "## [Unreleased]\n"
			}
			git := mainGit("0.1.0", changelog, "sha\trefs/tags/v0.1.0\n")
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
	if got, want := out.String(), "0.1.0 is released in CHANGELOG.md; would publish origin/main if its GitHub Release is missing\n"; got != want {
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

func publishFixture(t *testing.T, version, changelog string, releaseStatus int) (*fakeGit, github, *[]string, *map[string]string) {
	t.Helper()
	git := &fakeGit{responses: map[string]string{
		"show HEAD:version.txt":                                 version + "\n",
		"show HEAD:CHANGELOG.md":                                changelog,
		"log --first-parent -1 --format=%H HEAD -- version.txt": "merge-sha\n",
	}}
	git.responses["ls-remote --tags origin refs/tags/v"+version+" refs/tags/v"+version+"^{}"] = "merge-sha\trefs/tags/v" + version + "\n"
	requests := new([]string)
	posted := new(map[string]string)
	handler := http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		*requests = append(*requests, req.Method+" "+req.URL.RequestURI())
		if got := req.Header.Get("Authorization"); got != "Bearer test-token" {
			t.Errorf("Authorization = %q", got)
		}
		switch req.Method + " " + req.URL.Path {
		case "GET /repos/o/r/releases/tags/v" + version:
			w.WriteHeader(releaseStatus)
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
	if diff := cmp.Diff([]string{"GET /repos/o/r/releases/tags/v0.1.0", "POST /repos/o/r/releases"}, *requests); diff != "" {
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
		{"already published", "0.1.0", released, http.StatusOK, []string{"GET /repos/o/r/releases/tags/v0.1.0"}, "nothing to publish: GitHub Release v0.1.0 already exists\n"},
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

func TestRepositoryValidation(t *testing.T) {
	t.Parallel()
	for _, input := range []string{"", "owner", "o/r/extra", "./r", "o/..", "o /r", "o/"} {
		if _, err := parseRepository(input); err == nil || !strings.Contains(err.Error(), "invalid repository") {
			t.Errorf("parseRepository(%q) = %v", input, err)
		}
	}
	got, err := parseRepository("owner-name/repo.name")
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(repository{"owner-name", "repo.name"}, got, cmp.AllowUnexported(repository{})); diff != "" {
		t.Fatal(diff)
	}
}

func TestReleaseFailuresDoNotCommit(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct{ name, command, version, changelog, module, want string }{
		{name: "fetch", command: "fetch --quiet origin main", want: "fetch main: injected"},
		{name: "resolve", command: "rev-parse --verify origin/main^{commit}", want: "resolve origin/main: injected"},
		{name: "read", command: "show base:CHANGELOG.md", want: "read CHANGELOG.md from origin/main: injected"},
		{name: "version", version: "bad", want: "read version.txt: invalid semantic version \"bad\""},
		{name: "module", module: "not a module", want: "read go.mod: missing module path"},
		{name: "unreleased", changelog: "# Changelog\n", want: "read CHANGELOG.md: missing [Unreleased] section"},
		{name: "tags", command: "ls-remote --tags origin refs/tags/v*", want: "list release tags: injected"},
		{name: "overflow", version: strings.Repeat("9", 40) + ".1.0", want: "bump version: parse version"},
		{name: "duplicate", changelog: unreleased + "\n## [0.1.1] - 2026-09-25\n", want: "rewrite changelog: version [0.1.1] already exists"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			git := mainGit("0.1.0", unreleased, "")
			if tc.command != "" {
				git.failures[tc.command] = errors.New("injected")
			}
			if tc.version != "" {
				git.responses["show base:version.txt"] = tc.version
			}
			if tc.changelog != "" {
				git.responses["show base:CHANGELOG.md"] = tc.changelog
			}
			if tc.module != "" {
				git.responses["show base:go.mod"] = tc.module
			}
			var out bytes.Buffer
			err := releaseRunner(git, &out, true).release()
			if err == nil || !strings.HasPrefix(err.Error(), tc.want) {
				t.Fatalf("error = %v, want %q", err, tc.want)
			}
			if len(git.stdin("hash-object -w --stdin")) != 0 || git.checked != nil || out.Len() != 0 {
				t.Fatalf("failed release progressed: calls %v, checks %v, output %q", git.commands(), git.checked, out.String())
			}
		})
	}
}

func TestReleaseCommitFailuresStopBeforeCheckout(t *testing.T) {
	t.Parallel()
	for _, command := range []string{"rev-parse --git-path togi-release-index", "read-tree base", "hash-object -w --stdin", "update-index --cacheinfo 100644,blob,version.txt", "write-tree", "commit-tree tree -p base -F -", "checkout --quiet --detach commit"} {
		t.Run(command, func(t *testing.T) {
			git := mainGit("0.1.0", unreleased, "")
			git.failures[command] = errors.New("injected")
			var out bytes.Buffer
			err := releaseRunner(git, &out, true).release()
			if err == nil || !strings.Contains(err.Error(), "injected") {
				t.Fatalf("error = %v", err)
			}
			calls := git.commands()
			if calls[len(calls)-1] != command {
				t.Fatalf("continued after failure: %v", calls)
			}
			if command != "checkout --quiet --detach commit" && out.Len() != 0 {
				t.Fatalf("claimed release: %s", &out)
			}
		})
	}
}

func TestPublishFailuresDoNotCreateRelease(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct{ name, command, value, want string }{
		{"read", "show HEAD:version.txt", "", "read version.txt: injected"},
		{"version", "show HEAD:version.txt", "bad", "read version.txt: invalid semantic version"},
		{"history", "rev-parse --is-shallow-repository", "", "check release history: injected"},
		{"unshallow", "fetch --quiet --unshallow --no-tags origin main", "", "fetch complete release history: injected"},
		{"log", "log --first-parent -1 --format=%H HEAD -- version.txt", "", "find the commit that set version.txt to 0.1.0: injected"},
		{"empty history", "log --first-parent -1 --format=%H HEAD -- version.txt", "\n", "find the commit that set version.txt to 0.1.0: no commit changed version.txt"},
		{"tag lookup", "ls-remote --tags origin refs/tags/v0.1.0 refs/tags/v0.1.0^{}", "", "check tag v0.1.0: injected"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			git, api, requests, posted := publishFixture(t, "0.1.0", released, http.StatusNotFound)
			git.failures = map[string]error{}
			if tc.value == "" {
				git.failures[tc.command] = errors.New("injected")
			} else {
				git.responses[tc.command] = tc.value
			}
			if tc.name == "unshallow" {
				git.responses["rev-parse --is-shallow-repository"] = "true\n"
			}
			var out bytes.Buffer
			err := releaseRunner(git, &out, false).publish(api, repository{"o", "r"})
			if err == nil || !strings.HasPrefix(fmt.Sprint(err), tc.want) {
				t.Fatalf("error = %v, want %q", err, tc.want)
			}
			if *posted != nil || len(*requests) != 0 || out.Len() != 0 {
				t.Fatalf("publication progressed: %v %v %s", *requests, *posted, &out)
			}
		})
	}
}

type failureTransport struct {
	err  error
	body io.ReadCloser
}

func (f failureTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	if f.err != nil {
		return nil, f.err
	}
	return &http.Response{StatusCode: http.StatusOK, Body: f.body, Header: make(http.Header), Request: req}, nil
}

type failingBody struct{}

func (failingBody) Read([]byte) (int, error) { return 0, errors.New("broken response") }
func (failingBody) Close() error             { return nil }

func TestGitHubRequestErrors(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name, method, response, want string
		body                         any
		transport                    http.RoundTripper
		status                       int
	}{
		{name: "encode", method: http.MethodPost, body: make(chan int), want: "encode POST /test:"},
		{name: "prepare", method: "bad method", want: "prepare bad method /test:"},
		{name: "transport", method: http.MethodGet, transport: failureTransport{err: errors.New("offline")}, want: "GET /test:"},
		{name: "read", method: http.MethodGet, transport: failureTransport{body: failingBody{}}, want: "read GET /test response: broken response"},
		{name: "http", method: http.MethodGet, status: http.StatusForbidden, response: "denied", want: "GET /test: HTTP 403: denied"},
		{name: "decode", method: http.MethodGet, status: http.StatusOK, response: "not-json", want: "decode GET /test response:"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			transport := tc.transport
			if transport == nil {
				transport = handlerTransport{http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(tc.status); fmt.Fprint(w, tc.response) })}
			}
			api := github{base: "https://api.forge.example", token: "test-token", client: &http.Client{Transport: transport}}
			var result map[string]string
			_, err := api.request(tc.method, "/test", tc.body, &result)
			if err == nil || !strings.HasPrefix(err.Error(), tc.want) {
				t.Fatalf("error = %v, want %q", err, tc.want)
			}
			if result != nil {
				t.Fatalf("invalid response returned result %v", result)
			}
		})
	}
}

func TestGitHubFailuresReachOperator(t *testing.T) {
	t.Parallel()
	api := github{base: "https://api.forge.example", token: "test-token", client: &http.Client{Transport: handlerTransport{http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusForbidden)
		fmt.Fprint(w, "denied")
	})}}}
	if err := requireGreenCheck(api, repository{"o", "r"}, "base"); err == nil || !strings.Contains(err.Error(), "look up the check.yml run for base:") || !strings.Contains(err.Error(), "HTTP 403: denied") {
		t.Fatalf("green check error = %v", err)
	}
	for _, status := range []int{http.StatusForbidden, http.StatusNotFound} {
		t.Run(fmt.Sprint(status), func(t *testing.T) {
			git, api, _, posted := publishFixture(t, "0.1.0", released, status)
			if status == http.StatusNotFound {
				api.client = &http.Client{Transport: handlerTransport{http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
					if r.Method == http.MethodGet {
						w.WriteHeader(http.StatusNotFound)
						fmt.Fprint(w, "{}")
						return
					}
					w.WriteHeader(http.StatusForbidden)
					fmt.Fprint(w, "denied")
				})}}
			}
			var out bytes.Buffer
			err := releaseRunner(git, &out, false).publish(api, repository{"o", "r"})
			prefix := "check release v0.1.0:"
			if status == http.StatusNotFound {
				prefix = "create release v0.1.0:"
			}
			if err == nil || !strings.HasPrefix(err.Error(), prefix) || !strings.Contains(err.Error(), "HTTP 403:") {
				t.Fatalf("publication error = %v", err)
			}
			if *posted != nil || out.Len() != 0 {
				t.Fatalf("claimed publication: %v %s", *posted, &out)
			}
		})
	}
}
