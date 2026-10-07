package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"maps"
	"net/http"
	"net/http/httptest"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
)

const released = "# Changelog\n\n## [0.1.0] - 2026-09-25\n\n### Added\n\n- New option ([#34]).\n\n[0.1.0]: https://forge.example/o/r/releases/tag/v0.1.0\n\n[#34]: https://forge.example/o/r/pull/34\n"
const unreleasedOnly = "# Changelog\n"

var aFix = map[string]string{"35.md": "### Fixed\n\n- A fix.\n"}

type fakeGit struct {
	t         *testing.T
	responses map[string]string
	// failures fail every command equal to a key or starting with it and a space.
	failures   map[string]error
	blobs      map[string]string
	calls      []gitCmd
	checked    []string
	queueReads int
	checkErr   error
}

func (f *fakeGit) run(c gitCmd) (string, string, error) {
	f.calls = append(f.calls, c)
	key := strings.Join(c.args, " ")
	for prefix, err := range f.failures {
		if key == prefix || strings.HasPrefix(key, prefix+" ") {
			return "", "", err
		}
	}
	switch {
	case key == "hash-object -w --stdin":
		if f.blobs == nil {
			f.blobs = map[string]string{}
		}
		blob := fmt.Sprintf("blob%d", len(f.blobs))
		f.blobs[blob] = c.stdin
		return blob + "\n", "", nil
	case c.args[0] == "update-index":
		return "", "", nil
	}
	response, ok := f.responses[key]
	if !ok {
		f.t.Errorf("unexpected git %s", key)
		return "", "", fmt.Errorf("unexpected git %s", key)
	}
	return response, "", nil
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

// mutations lists the commands that write objects, an index or the checkout.
func (f *fakeGit) mutations() []string {
	var got []string
	for _, c := range f.calls {
		switch c.args[0] {
		case "read-tree", "hash-object", "update-index", "write-tree", "commit-tree", "checkout":
			got = append(got, strings.Join(c.args, " "))
		}
	}
	return got
}

type stagedFile struct{ Mode, Content string }

// staged returns the mode and content each update-index call stages by path, and the paths it removes.
func (f *fakeGit) staged() (map[string]stagedFile, []string) {
	files := map[string]stagedFile{}
	var removed []string
	for _, c := range f.calls {
		if c.args[0] != "update-index" {
			continue
		}
		switch c.args[1] {
		case "--cacheinfo":
			mode, entry, _ := strings.Cut(c.args[2], ",")
			blob, path, _ := strings.Cut(entry, ",")
			files[path] = stagedFile{Mode: mode, Content: f.blobs[blob]}
		case "--force-remove":
			removed = append(removed, c.args[2])
		}
	}
	return files, removed
}

func mainGit(t *testing.T, version, changelog, tags string, fragments map[string]string) *fakeGit {
	t.Helper()
	git := &fakeGit{
		t: t,
		responses: map[string]string{
			"fetch --quiet origin main":               "",
			"rev-parse --verify origin/main^{commit}": "base\n",
			"show base:version.txt":                   version + "\n",
			"show base:CHANGELOG.md":                  changelog,
			"show base:go.mod":                        "module forge.example/o/r\n\ngo 1.27\n",
			"ls-remote --tags origin refs/tags/v*":    tags,
			"rev-parse --git-path togi-release-index": "index\n",
			"read-tree base":                          "",
			"write-tree":                              "tree\n",
			"commit-tree tree -p base -F -":           "commit\n",
			"checkout --quiet --detach base":          "",
			"checkout --quiet --detach commit":        "",
		},
		failures: map[string]error{},
	}
	var listing string
	for _, name := range fragmentNames(fragments) {
		listing += "changes/" + name + "\n"
		git.responses["show base:changes/"+name] = fragments[name]
	}
	git.responses["ls-tree --name-only base changes/"] = listing + "changes/README.md\n"
	return git
}

func fragmentNames(fragments map[string]string) []string {
	return slices.Sorted(maps.Keys(fragments))
}

func releaseRunner(git *fakeGit, out *bytes.Buffer, commit bool) runner {
	return runner{
		git:    git.run,
		now:    func() time.Time { return time.Date(2026, 9, 26, 0, 0, 0, 0, time.UTC) },
		out:    out,
		commit: commit,
		requireNoHardwareWait: func() error {
			git.queueReads++
			return nil
		},
		requireGreen: func(commit string) error {
			git.checked = append(git.checked, commit)
			return git.checkErr
		},
	}
}

func TestRelease(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name, version, changelog, tags string
		fragments                      map[string]string
		next, reason, changelogOut     string
	}{
		{
			name: "bump", version: "0.1.0", changelog: released, tags: "sha\trefs/tags/v0.1.0\n", fragments: aFix,
			next: "0.1.1", reason: "No breaking changes (patch bump).",
			changelogOut: "# Changelog\n\n## [0.1.1] - 2026-09-26\n\n### Fixed\n\n- A fix ([#35]).\n\n## [0.1.0] - 2026-09-25\n\n### Added\n\n- New option ([#34]).\n\n[0.1.0]: https://forge.example/o/r/releases/tag/v0.1.0\n\n[0.1.1]: https://forge.example/o/r/releases/tag/v0.1.1\n\n[#34]: https://forge.example/o/r/pull/34\n[#35]: https://forge.example/o/r/pull/35\n",
		},
		{
			name: "bump without tags", version: "0.1.0", changelog: unreleasedOnly,
			fragments: map[string]string{"34.md": "### Added\n\n- New option.\n"},
			next:      "0.1.1", reason: "No breaking changes (patch bump).",
			changelogOut: "# Changelog\n\n## [0.1.1] - 2026-09-26\n\n### Added\n\n- New option ([#34]).\n\n[0.1.1]: https://forge.example/o/r/releases/tag/v0.1.1\n\n[#34]: https://forge.example/o/r/pull/34\n",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			git := mainGit(t, tc.version, tc.changelog, tc.tags, tc.fragments)
			var out bytes.Buffer
			if err := releaseRunner(git, &out, true).release(); err != nil {
				t.Fatal(err)
			}
			commands := git.commands()
			if commands[0] != "fetch --quiet origin main" {
				t.Fatalf("release read origin/main before fetching it: %v", commands)
			}
			files, removed := git.staged()
			want := map[string]stagedFile{"version.txt": {Mode: "100644", Content: tc.next + "\n"}, "CHANGELOG.md": {Mode: "100644", Content: tc.changelogOut}}
			if diff := cmp.Diff(want, files); diff != "" {
				t.Fatalf("committed files mismatch (-want +got):\n%s", diff)
			}
			var consumed []string
			for _, name := range fragmentNames(tc.fragments) {
				consumed = append(consumed, "changes/"+name)
			}
			if diff := cmp.Diff(consumed, removed); diff != "" {
				t.Fatalf("removed fragments mismatch (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff([]string{"Release " + tc.next + "\n\n" + tc.reason + "\n"}, git.stdin("commit-tree tree -p base -F -")); diff != "" {
				t.Fatalf("commit message mismatch (-want +got):\n%s", diff)
			}
			for _, c := range git.calls {
				switch c.args[0] {
				case "read-tree", "update-index", "write-tree":
					if !slices.Contains(c.env, "GIT_INDEX_FILE=index") {
						t.Fatalf("%s ran outside the temporary index: env %v", strings.Join(c.args, " "), c.env)
					}
				}
			}
			if last := commands[len(commands)-1]; last != "checkout --quiet --detach commit" {
				t.Fatalf("release ended on %q, not on checking out its commit", last)
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
	git := mainGit(t, "0.1.0", released, "sha\trefs/tags/v0.1.0\n", aFix)
	git.checkErr = errors.New("check run u for base concluded failure")
	var out bytes.Buffer
	err := releaseRunner(git, &out, true).release()
	if got, want := fmt.Sprint(err), "require a passing check on origin/main: check run u for base concluded failure"; got != want {
		t.Fatalf("release error = %q, want %q", got, want)
	}
	if got := git.mutations(); got != nil {
		t.Fatalf("refused release changed the repository: %v", got)
	}
}

// pullRequests renders a page of n open pull requests, which the issues endpoint lists among issues.
func pullRequests(n int) string {
	records := make([]string, n)
	for i := range records {
		records[i] = fmt.Sprintf(`{"number":%d,"title":"A pull request","pull_request":{"url":"u"}}`, 1000+i)
	}
	return "[" + strings.Join(records, ",") + "]"
}

func TestReleaseWaitsOnTheTargetMachine(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name, err string
		pages     []string
		mutations []string
		checked   []string
	}{
		{
			name:      "empty queue",
			pages:     []string{`[{"number":40,"title":"A pull request","pull_request":{"url":"u"}}]`},
			mutations: []string{"read-tree base", "hash-object -w --stdin", "update-index --cacheinfo 100644,blob0,version.txt", "hash-object -w --stdin", "update-index --cacheinfo 100644,blob1,CHANGELOG.md", "update-index --force-remove changes/35.md", "write-tree", "commit-tree tree -p base -F -", "checkout --quiet --detach commit"},
			checked:   []string{"base"},
		},
		{
			name:  "open issue",
			pages: []string{`[{"number":417,"title":"Backends: shared check","state":"open","pull_request":null},{"number":40,"title":"A pull request","state":"open","pull_request":{"url":"u"}}]`},
			err:   "refuse to release: issues wait on a run on the target machine (needs-hardware): #417 Backends: shared check",
		},
		{
			name:  "closed issue",
			pages: []string{`[{"number":417,"title":"Backends: shared check","state":"closed","pull_request":null},{"number":40,"title":"A pull request","state":"closed","pull_request":{"url":"u"}}]`},
			err:   "refuse to release: issues wait on a run on the target machine (needs-hardware): #417 Backends: shared check (closed)",
		},
		{
			name:  "open and closed issues",
			pages: []string{`[{"number":417,"title":"Backends: shared check","state":"open"},{"number":418,"title":"Hardware-layer names","state":"closed"}]`},
			err:   "refuse to release: issues wait on a run on the target machine (needs-hardware): #417 Backends: shared check; #418 Hardware-layer names (closed)",
		},
		{
			name:  "issue after a full page of pull requests",
			pages: []string{pullRequests(100), `[{"number":417,"title":"Backends: shared check","state":"closed","pull_request":null}]`},
			err:   "refuse to release: issues wait on a run on the target machine (needs-hardware): #417 Backends: shared check (closed)",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			var requests []string
			api := github{base: "https://api.forge.example", token: "test-token", client: &http.Client{Transport: handlerTransport{http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
				requests = append(requests, req.Method+" "+req.URL.RequestURI())
				page, err := strconv.Atoi(req.URL.Query().Get("page"))
				if err != nil || page < 1 || page > len(tc.pages) {
					t.Errorf("request for page %q of %d", req.URL.Query().Get("page"), len(tc.pages))
					http.NotFound(w, req)
					return
				}
				fmt.Fprint(w, tc.pages[page-1])
			})}}}
			git := mainGit(t, "0.1.0", released, "sha\trefs/tags/v0.1.0\n", aFix)
			var out bytes.Buffer
			r := releaseRunner(git, &out, true)
			r.requireNoHardwareWait = func() error { return requireNoHardwareWait(api, repository{"o", "r"}) }
			err := r.release()
			if got := fmt.Sprint(err); tc.err != "" && got != tc.err || tc.err == "" && err != nil {
				t.Fatalf("release error = %v, want %q", err, tc.err)
			}
			var want []string
			for page := range len(tc.pages) {
				want = append(want, fmt.Sprintf("GET /repos/o/r/issues?labels=needs-hardware&page=%d&per_page=100&state=all", page+1))
			}
			if diff := cmp.Diff(want, requests); diff != "" {
				t.Errorf("requests mismatch (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff(tc.mutations, git.mutations()); diff != "" {
				t.Errorf("repository changes mismatch (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff(tc.checked, git.checked); diff != "" {
				t.Errorf("green check mismatch (-want +got):\n%s", diff)
			}
		})
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
		{name: "commit", commit: true, err: "nothing to release: changes/ has no fragments"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			changelog := released
			if tc.commit {
				changelog = unreleasedOnly
			}
			git := mainGit(t, "0.1.0", changelog, "sha\trefs/tags/v0.1.0\n", nil)
			var out bytes.Buffer
			err := releaseRunner(git, &out, tc.commit).release()
			if got := fmt.Sprint(err); tc.err != "" && got != tc.err || tc.err == "" && err != nil {
				t.Fatalf("release error = %v, want %q", err, tc.err)
			}
			if got := git.mutations(); got != nil {
				t.Fatalf("empty release changed the repository: %v", got)
			}
			if got := out.String(); got != tc.output {
				t.Fatalf("output = %q", got)
			}
		})
	}
}

func TestReleaseResumesUnpublished(t *testing.T) {
	t.Parallel()
	git := mainGit(t, "0.1.0", released, "sha\trefs/tags/v0.0.9\n", nil)
	var out bytes.Buffer
	if err := releaseRunner(git, &out, true).release(); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]string{"checkout --quiet --detach base"}, git.mutations()); diff != "" {
		t.Fatalf("repository changes mismatch (-want +got):\n%s", diff)
	}
	if got, want := out.String(), "0.1.0 is released in CHANGELOG.md; would publish origin/main if its GitHub Release is missing\n"; got != want {
		t.Fatalf("output = %q", got)
	}
	if git.checked != nil {
		t.Fatalf("resume required a green check for %v", git.checked)
	}
	if git.queueReads != 0 {
		t.Fatalf("resume read the needs-hardware queue %d times", git.queueReads)
	}
}

func TestReleasePreview(t *testing.T) {
	t.Parallel()
	git := mainGit(t, "0.1.0", released, "sha\trefs/tags/v0.1.0\n", aFix)
	var out bytes.Buffer
	if err := releaseRunner(git, &out, false).release(); err != nil {
		t.Fatal(err)
	}
	if got := git.mutations(); got != nil {
		t.Fatalf("preview changed the repository: %v", got)
	}
	want := "Would release from origin/main:\n\nRelease 0.1.1\n\nNo breaking changes (patch bump).\n\n### Fixed\n\n- A fix ([#35]).\n\n[#35]: https://forge.example/o/r/pull/35\n"
	if diff := cmp.Diff(want, out.String()); diff != "" {
		t.Fatalf("output mismatch (-want +got):\n%s", diff)
	}
	if git.checked != nil {
		t.Fatalf("preview required a green check for %v", git.checked)
	}
	if git.queueReads != 0 {
		t.Fatalf("preview read the needs-hardware queue %d times", git.queueReads)
	}
}

func TestWaitForGreenCheck(t *testing.T) {
	t.Parallel()
	const (
		none       = `[]`
		queued     = `[{"html_url":"u","status":"queued","conclusion":null}]`
		inProgress = `[{"html_url":"u","status":"in_progress","conclusion":null}]`
		success    = `[{"html_url":"u","status":"completed","conclusion":"success"}]`
		failure    = `[{"html_url":"u","status":"completed","conclusion":"failure"}]`
		cancelled  = `[{"html_url":"u","status":"completed","conclusion":"cancelled"}]`
	)
	for _, tc := range []struct {
		name      string
		responses []string
		err, out  string
		sleeps    []time.Duration
	}{
		{name: "success", responses: []string{success}},
		{name: "failure", responses: []string{failure}, err: "check run u for base concluded failure"},
		{name: "cancelled", responses: []string{cancelled}, err: "check run u for base concluded cancelled"},
		{
			name:      "missing then in progress then success",
			responses: []string{none, inProgress, inProgress, success},
			out:       "no check.yml run on main for base; waiting up to 1h0m0s for it to pass\ncheck run u for base is in_progress; waiting up to 59m30s for it to pass\n",
			sleeps:    []time.Duration{30 * time.Second, 30 * time.Second, 30 * time.Second},
		},
		{
			name:      "fails while waited on",
			responses: []string{queued, failure},
			err:       "check run u for base concluded failure",
			out:       "check run u for base is queued; waiting up to 1h0m0s for it to pass\n",
			sleeps:    []time.Duration{30 * time.Second},
		},
		{
			name:      "cancelled while waited on",
			responses: []string{inProgress, cancelled},
			err:       "check run u for base concluded cancelled",
			out:       "check run u for base is in_progress; waiting up to 1h0m0s for it to pass\n",
			sleeps:    []time.Duration{30 * time.Second},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			check := newFakeCheck(t, tc.responses, 0)
			var out bytes.Buffer
			err := waitForGreenCheck(check.api, repository{"o", "r"}, "base", check.now, check.sleep, &out)
			if got := fmt.Sprint(err); tc.err != "" && got != tc.err || tc.err == "" && err != nil {
				t.Fatalf("waitForGreenCheck error = %v, want %q", err, tc.err)
			}
			if diff := cmp.Diff(tc.out, out.String()); diff != "" {
				t.Errorf("output mismatch (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff(tc.sleeps, check.sleeps); diff != "" {
				t.Errorf("sleeps mismatch (-want +got):\n%s", diff)
			}
			want := slices.Repeat([]string{"GET /repos/o/r/actions/workflows/check.yml/runs?branch=main&event=push&head_sha=base&per_page=1"}, len(tc.responses))
			if diff := cmp.Diff(want, check.requests); diff != "" {
				t.Fatalf("requests mismatch (-want +got):\n%s", diff)
			}
		})
	}
}

func TestWaitForGreenCheckTimesOut(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct{ name, runs, err string }{
		{"missing", `[]`, "no check.yml run on main for base after waiting 1h0m0s"},
		{"queued", `[{"html_url":"u","status":"queued","conclusion":null}]`, "check run u for base is queued after waiting 1h0m0s"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			// Each lookup takes 7s: 97 rounds of lookup and 30s sleep reach 59m49s, the 98th lookup leaves 4s for the last sleep, and the 99th ends the wait.
			check := newFakeCheck(t, []string{tc.runs}, 7*time.Second)
			err := waitForGreenCheck(check.api, repository{"o", "r"}, "base", check.now, check.sleep, io.Discard)
			if got := fmt.Sprint(err); got != tc.err {
				t.Fatalf("waitForGreenCheck error = %v, want %q", err, tc.err)
			}
			if got, want := len(check.requests), 99; got != want {
				t.Errorf("lookups = %d, want %d", got, want)
			}
			if got, want := check.sleeps[len(check.sleeps)-1], 4*time.Second; got != want {
				t.Errorf("last sleep = %s, want %s", got, want)
			}
			if got, want := check.elapsed(), checkTimeout+7*time.Second; got != want {
				t.Errorf("waited %s, want %s", got, want)
			}
		})
	}
}

// fakeCheck serves the check runs in order, repeating the last, on a fake clock that each lookup advances by lookup.
type fakeCheck struct {
	api      github
	start    time.Time
	clock    time.Time
	requests []string
	sleeps   []time.Duration
}

func newFakeCheck(t *testing.T, responses []string, lookup time.Duration) *fakeCheck {
	t.Helper()
	start := time.Date(2026, 10, 6, 12, 0, 0, 0, time.UTC)
	check := &fakeCheck{start: start, clock: start}
	handler := http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		runs := responses[min(len(check.requests), len(responses)-1)]
		check.requests = append(check.requests, req.Method+" "+req.URL.RequestURI())
		check.clock = check.clock.Add(lookup)
		fmt.Fprint(w, `{"total_count":1,"workflow_runs":`+runs+`}`)
	})
	check.api = github{base: "https://api.forge.example", token: "test-token", client: &http.Client{Transport: handlerTransport{handler}}}
	return check
}

func (c *fakeCheck) now() time.Time { return c.clock }

func (c *fakeCheck) sleep(d time.Duration) {
	c.sleeps = append(c.sleeps, d)
	c.clock = c.clock.Add(d)
}

func (c *fakeCheck) elapsed() time.Duration { return c.clock.Sub(c.start) }

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
	git := &fakeGit{t: t, responses: map[string]string{
		"show HEAD:version.txt":                                 version + "\n",
		"show HEAD:CHANGELOG.md":                                changelog,
		"rev-parse --is-shallow-repository":                     "false\n",
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
		"body": "### Added\n\n- New option ([#34]).\n\n[#34]: https://forge.example/o/r/pull/34",
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
		{"not released yet", "0.1.0", unreleasedOnly, http.StatusNotFound, nil, "nothing to publish: CHANGELOG.md has no released [0.1.0] section\n"},
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
	for _, tc := range []struct{ name, command, version, changelog, module, fragment, want string }{
		{name: "fetch", command: "fetch --quiet origin main", want: "fetch main: injected"},
		{name: "resolve", command: "rev-parse --verify origin/main^{commit}", want: "resolve origin/main: injected"},
		{name: "read", command: "show base:CHANGELOG.md", want: "read CHANGELOG.md from origin/main: injected"},
		{name: "version", version: "bad", want: "read version.txt: invalid semantic version \"bad\""},
		{name: "module", module: "not a module", want: "read go.mod: missing module path"},
		{name: "list fragments", command: "ls-tree --name-only base changes/", want: "read changes/ from origin/main: list: injected"},
		{name: "read fragment", command: "show base:changes/35.md", want: "read changes/ from origin/main: read 35.md: injected"},
		{name: "invalid fragment", fragment: "- A fix.\n", want: "read changes/: 35.md:1: entry before any ### section heading"},
		{name: "tags", command: "ls-remote --tags origin refs/tags/v*", want: "list release tags: injected"},
		{name: "overflow", version: strings.Repeat("9", 40) + ".1.0", want: "bump version: parse version"},
		{name: "duplicate", changelog: released + "\n## [0.1.1] - 2026-09-25\n", want: "rewrite changelog: version [0.1.1] already exists"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			git := mainGit(t, "0.1.0", released, "", aFix)
			if tc.fragment != "" {
				git.responses["show base:changes/35.md"] = tc.fragment
			}
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
			if got := git.mutations(); got != nil || git.checked != nil || out.Len() != 0 {
				t.Fatalf("failed release progressed: changes %v, checks %v, output %q", got, git.checked, out.String())
			}
		})
	}
}

func TestReleaseCommitFailuresStopBeforeCheckout(t *testing.T) {
	t.Parallel()
	for _, command := range []string{"rev-parse --git-path togi-release-index", "read-tree base", "hash-object -w --stdin", "update-index --cacheinfo", "update-index --force-remove", "write-tree", "commit-tree tree -p base -F -", "checkout --quiet --detach commit"} {
		t.Run(command, func(t *testing.T) {
			git := mainGit(t, "0.1.0", released, "", aFix)
			git.failures[command] = errors.New("injected")
			var out bytes.Buffer
			err := releaseRunner(git, &out, true).release()
			if err == nil || !strings.Contains(err.Error(), "injected") {
				t.Fatalf("error = %v", err)
			}
			calls := git.commands()
			if last := calls[len(calls)-1]; last != command && !strings.HasPrefix(last, command+" ") {
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
	noSleep := func(time.Duration) { t.Error("waited after a failed lookup") }
	epoch := func() time.Time { return time.Time{} }
	if err := waitForGreenCheck(api, repository{"o", "r"}, "base", epoch, noSleep, io.Discard); err == nil || !strings.Contains(err.Error(), "look up the check.yml run for base:") || !strings.Contains(err.Error(), "HTTP 403: denied") {
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
