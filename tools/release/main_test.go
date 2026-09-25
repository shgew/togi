package main

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"reflect"
	"strings"
	"testing"
	"time"
)

const apiPrefix = "/api/v1/repos/o/r"
const unreleased = "## [Unreleased]\n\n### Added\n\n- New option ([#34]).\n\n[#34]: https://forge.example/o/r/pulls/34\n"
const released = "## [Unreleased]\n\n## [0.1.0] - 2026-09-25\n\n### Added\n\n- New option ([#34]).\n\n[0.1.0]: https://forge.example/o/r/releases/tag/v0.1.0\n\n[#34]: https://forge.example/o/r/pulls/34\n"

func releaseServer(t *testing.T, version, changelog string, extra func(http.ResponseWriter, *http.Request)) (*httptest.Server, *[]string) {
	t.Helper()
	requests := new([]string)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		*requests = append(*requests, req.Method+" "+req.URL.RequestURI())
		if got := req.Header.Get("Authorization"); got != "token test-token" {
			t.Errorf("Authorization = %q", got)
		}
		switch req.URL.Path {
		case apiPrefix:
			fmt.Fprint(w, `{"default_branch":"main"}`)
		case apiPrefix + "/raw/version.txt":
			if req.URL.Query().Get("ref") != "main" {
				t.Errorf("version ref = %q", req.URL.Query().Get("ref"))
			}
			fmt.Fprint(w, version+"\n")
		case apiPrefix + "/raw/CHANGELOG.md":
			fmt.Fprint(w, changelog)
		default:
			extra(w, req)
		}
	}))
	t.Cleanup(server.Close)
	return server, requests
}

func releaseRunner(server *httptest.Server, out *bytes.Buffer, dryRun bool) runner {
	return runner{
		api:  forgejo{base: server.URL + "/api/v1", token: "test-token", client: server.Client()},
		repo: repository{owner: "o", name: "r", webURL: "https://forge.example/o/r"},
		now:  func() time.Time { return time.Date(2026, 9, 25, 0, 0, 0, 0, time.UTC) },
		out:  out, dryRun: dryRun,
	}
}

func assertRequests(t *testing.T, got []string, suffix ...string) {
	t.Helper()
	want := []string{"GET " + apiPrefix, "GET " + apiPrefix + "/raw/version.txt?ref=main", "GET " + apiPrefix + "/raw/CHANGELOG.md?ref=main"}
	want = append(want, suffix...)
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("requests:\n%q\nwant:\n%q", got, want)
	}
}

func TestTagRelease(t *testing.T) {
	t.Parallel()
	server, requests := releaseServer(t, "0.1.0", released, func(w http.ResponseWriter, req *http.Request) {
		switch req.URL.Path {
		case apiPrefix + "/tags/v0.1.0":
			w.WriteHeader(http.StatusNotFound)
		case apiPrefix + "/pulls":
			fmt.Fprint(w, `[{"merged":false,"head":{"ref":"release-0.1.0"}},{"merged":true,"merge_commit_sha":"merge-sha","head":{"ref":"release-0.1.0"}}]`)
		case apiPrefix + "/releases":
			var post struct {
				Tag, Commit, Name, Body string
			}
			var fields map[string]string
			if err := json.NewDecoder(req.Body).Decode(&fields); err != nil {
				t.Fatal(err)
			}
			post.Tag, post.Commit, post.Name, post.Body = fields["tag_name"], fields["target_commitish"], fields["name"], fields["body"]
			if post.Tag != "v0.1.0" || post.Commit != "merge-sha" || post.Name != "0.1.0" || post.Body != "### Added\n\n- New option ([#34]).\n\n[#34]: https://forge.example/o/r/pulls/34" {
				t.Errorf("release payload = %+v", post)
			}
			w.WriteHeader(http.StatusCreated)
			fmt.Fprint(w, `{"html_url":"https://forge.example/o/r/releases/tag/v0.1.0"}`)
		default:
			t.Errorf("unexpected request %s", req.URL)
		}
	})
	var out bytes.Buffer
	if err := releaseRunner(server, &out, false).run(); err != nil {
		t.Fatal(err)
	}
	if got := out.String(); got != "https://forge.example/o/r/releases/tag/v0.1.0\n" {
		t.Fatalf("output = %q", got)
	}
	assertRequests(t, *requests, "GET "+apiPrefix+"/tags/v0.1.0", "GET "+apiPrefix+"/pulls?state=closed&limit=50&page=1", "POST "+apiPrefix+"/releases")
}

func TestTagDryRun(t *testing.T) {
	t.Parallel()
	server, requests := releaseServer(t, "0.1.0", released, func(w http.ResponseWriter, req *http.Request) {
		switch req.URL.Path {
		case apiPrefix + "/tags/v0.1.0":
			w.WriteHeader(http.StatusNotFound)
		case apiPrefix + "/pulls":
			fmt.Fprint(w, `[{"merged":true,"merge_commit_sha":"merge-sha","head":{"ref":"release-0.1.0"}}]`)
		default:
			t.Errorf("dry run changed repository: %s %s", req.Method, req.URL)
		}
	})
	var out bytes.Buffer
	if err := releaseRunner(server, &out, true).run(); err != nil {
		t.Fatal(err)
	}
	if out.String() != "Would publish release v0.1.0 at merge commit merge-sha\n" {
		t.Fatalf("output = %q", out.String())
	}
	assertRequests(t, *requests, "GET "+apiPrefix+"/tags/v0.1.0", "GET "+apiPrefix+"/pulls?state=closed&limit=50&page=1")
}

func TestTagRequiresMergedReleasePullRequest(t *testing.T) {
	t.Parallel()
	server, requests := releaseServer(t, "0.1.0", released, func(w http.ResponseWriter, req *http.Request) {
		switch req.URL.Path {
		case apiPrefix + "/tags/v0.1.0":
			w.WriteHeader(http.StatusNotFound)
		case apiPrefix + "/pulls":
			fmt.Fprint(w, `[{"merged":false,"head":{"ref":"release-0.1.0"}}]`)
		default:
			t.Errorf("unexpected write or request: %s %s", req.Method, req.URL)
		}
	})
	var out bytes.Buffer
	err := releaseRunner(server, &out, false).run()
	if err == nil || !strings.Contains(err.Error(), "no merged pull request with head release-0.1.0") {
		t.Fatalf("error = %v", err)
	}
	assertRequests(t, *requests, "GET "+apiPrefix+"/tags/v0.1.0", "GET "+apiPrefix+"/pulls?state=closed&limit=50&page=1")
}

func TestReleasePullRequest(t *testing.T) {
	t.Parallel()
	changelog := "## [Unreleased]\n\n### Added\n\n- New option ([#34]).\n\n## [0.1.0] - 2026-09-01\n\n### Fixed\n\n- Old fix.\n\n[#34]: https://forge.example/o/r/pulls/34\n"
	server, requests := releaseServer(t, "0.1.0", changelog, func(w http.ResponseWriter, req *http.Request) {
		switch {
		case req.URL.Path == apiPrefix+"/tags/v0.1.0":
			fmt.Fprint(w, `{"name":"v0.1.0"}`)
		case req.URL.Path == apiPrefix+"/pulls" && req.Method == http.MethodGet:
			fmt.Fprint(w, `[]`)
		case req.URL.Path == apiPrefix+"/tags":
			fmt.Fprint(w, `[{"name":"v0.1.0"}]`)
		case strings.HasPrefix(req.URL.Path, apiPrefix+"/contents/"):
			fmt.Fprint(w, `{"sha":"blob-sha"}`)
		case req.URL.Path == apiPrefix+"/contents":
			var payload struct {
				Branch    string `json:"branch"`
				NewBranch string `json:"new_branch"`
				Message   string `json:"message"`
				Files     []struct {
					Operation, Path, SHA, Content string
				}
			}
			if err := json.NewDecoder(req.Body).Decode(&payload); err != nil {
				t.Fatal(err)
			}
			if payload.Branch != "main" || payload.NewBranch != "release-0.1.1" || payload.Message != "Release 0.1.1" || len(payload.Files) != 2 {
				t.Errorf("commit payload = %+v", payload)
			}
			for _, file := range payload.Files {
				data, err := base64.StdEncoding.DecodeString(file.Content)
				if err != nil || file.SHA != "blob-sha" || file.Operation != "update" {
					t.Errorf("file payload = %+v, %v", file, err)
				}
				switch file.Path {
				case "version.txt":
					if string(data) != "0.1.1\n" {
						t.Errorf("version = %q", data)
					}
				case "CHANGELOG.md":
					if !strings.Contains(string(data), "## [0.1.1] - 2026-09-25") || !strings.Contains(string(data), "[0.1.1]: https://forge.example/o/r/releases/tag/v0.1.1") {
						t.Errorf("changelog = %q", data)
					}
				default:
					t.Errorf("unknown path %q", file.Path)
				}
			}
			w.WriteHeader(http.StatusCreated)
			fmt.Fprint(w, `{}`)
		case req.URL.Path == apiPrefix+"/pulls" && req.Method == http.MethodPost:
			var payload map[string]string
			if err := json.NewDecoder(req.Body).Decode(&payload); err != nil {
				t.Fatal(err)
			}
			if payload["title"] != "Release 0.1.1" || payload["head"] != "release-0.1.1" || payload["base"] != "main" || !strings.Contains(payload["body"], "patch bump") {
				t.Errorf("PR payload = %q", payload)
			}
			w.WriteHeader(http.StatusCreated)
			fmt.Fprint(w, `{"html_url":"https://forge.example/o/r/pulls/35"}`)
		default:
			t.Errorf("unexpected request %s", req.URL)
		}
	})
	var out bytes.Buffer
	if err := releaseRunner(server, &out, false).run(); err != nil {
		t.Fatal(err)
	}
	if got := out.String(); got != "https://forge.example/o/r/pulls/35\n" {
		t.Fatalf("output = %q", got)
	}
	assertRequests(t, *requests, "GET "+apiPrefix+"/tags/v0.1.0", "GET "+apiPrefix+"/pulls?state=open&limit=50&page=1", "GET "+apiPrefix+"/tags?limit=50&page=1", "GET "+apiPrefix+"/contents/CHANGELOG.md?ref=main", "GET "+apiPrefix+"/contents/version.txt?ref=main", "POST "+apiPrefix+"/contents", "POST "+apiPrefix+"/pulls")
}

func TestOpenReleasePullRequest(t *testing.T) {
	t.Parallel()
	server, requests := releaseServer(t, "0.1.0", unreleased, func(w http.ResponseWriter, req *http.Request) {
		if req.URL.Path == apiPrefix+"/pulls" {
			fmt.Fprint(w, `[{"head":{"ref":"topic"}},{"head":{"ref":"release-0.2.0"},"html_url":"https://forge.example/o/r/pulls/35"}]`)
		} else {
			t.Errorf("unexpected request %s", req.URL)
		}
	})
	var out bytes.Buffer
	if err := releaseRunner(server, &out, false).run(); err != nil {
		t.Fatal(err)
	}
	if out.String() != "https://forge.example/o/r/pulls/35\n" {
		t.Fatalf("output = %q", out.String())
	}
	assertRequests(t, *requests, "GET "+apiPrefix+"/pulls?state=open&limit=50&page=1")
}

func TestNothingToRelease(t *testing.T) {
	t.Parallel()
	server, requests := releaseServer(t, "0.1.0", "## [Unreleased]\n\n### Added\n\n## [0.1.0] - 2026-09-01\n\n- Previous.\n", func(w http.ResponseWriter, req *http.Request) {
		switch req.URL.Path {
		case apiPrefix + "/tags/v0.1.0":
			fmt.Fprint(w, `{}`)
		case apiPrefix + "/pulls":
			fmt.Fprint(w, `[]`)
		default:
			t.Errorf("unexpected request %s", req.URL)
		}
	})
	var out bytes.Buffer
	if err := releaseRunner(server, &out, false).run(); err != nil {
		t.Fatal(err)
	}
	if out.String() != "nothing to release\n" {
		t.Fatalf("output = %q", out.String())
	}
	assertRequests(t, *requests, "GET "+apiPrefix+"/tags/v0.1.0", "GET "+apiPrefix+"/pulls?state=open&limit=50&page=1")
}

func TestFirstRelease(t *testing.T) {
	t.Parallel()
	server, requests := releaseServer(t, "0.1.0", unreleased, func(w http.ResponseWriter, req *http.Request) {
		switch {
		case req.URL.Path == apiPrefix+"/pulls" && req.Method == http.MethodGet:
			fmt.Fprint(w, `[]`)
		case req.URL.Path == apiPrefix+"/tags":
			fmt.Fprint(w, `[]`)
		case strings.HasPrefix(req.URL.Path, apiPrefix+"/contents/"):
			fmt.Fprint(w, `{"sha":"blob-sha"}`)
		case req.URL.Path == apiPrefix+"/contents":
			var payload struct {
				NewBranch string `json:"new_branch"`
				Files     []struct {
					Path, Content string
				}
			}
			if err := json.NewDecoder(req.Body).Decode(&payload); err != nil {
				t.Error(err)
				return
			}
			if payload.NewBranch != "release-0.1.0" || len(payload.Files) != 2 {
				t.Errorf("commit payload = %+v", payload)
			}
			for _, file := range payload.Files {
				if file.Path == "version.txt" {
					decoded, err := base64.StdEncoding.DecodeString(file.Content)
					if err != nil || string(decoded) != "0.1.0\n" {
						t.Errorf("initial version = %q, %v", decoded, err)
					}
				}
			}
			w.WriteHeader(http.StatusCreated)
			fmt.Fprint(w, `{}`)
		case req.URL.Path == apiPrefix+"/pulls" && req.Method == http.MethodPost:
			var payload map[string]string
			if err := json.NewDecoder(req.Body).Decode(&payload); err != nil {
				t.Error(err)
				return
			}
			if payload["title"] != "Release 0.1.0" || !strings.Contains(payload["body"], "First release") {
				t.Errorf("first release PR = %q", payload)
			}
			w.WriteHeader(http.StatusCreated)
			fmt.Fprint(w, `{"html_url":"https://forge.example/o/r/pulls/35"}`)
		default:
			t.Errorf("unexpected request %s %s", req.Method, req.URL)
		}
	})
	var out bytes.Buffer
	if err := releaseRunner(server, &out, false).run(); err != nil {
		t.Fatal(err)
	}
	if out.String() != "https://forge.example/o/r/pulls/35\n" {
		t.Fatalf("output = %q", out.String())
	}
	assertRequests(t, *requests, "GET "+apiPrefix+"/pulls?state=open&limit=50&page=1", "GET "+apiPrefix+"/tags?limit=50&page=1", "GET "+apiPrefix+"/contents/CHANGELOG.md?ref=main", "GET "+apiPrefix+"/contents/version.txt?ref=main", "POST "+apiPrefix+"/contents", "POST "+apiPrefix+"/pulls")
}

func TestFirstReleaseDryRun(t *testing.T) {
	t.Parallel()
	server, requests := releaseServer(t, "0.1.0", unreleased, func(w http.ResponseWriter, req *http.Request) {
		switch req.URL.Path {
		case apiPrefix + "/pulls", apiPrefix + "/tags":
			fmt.Fprint(w, `[]`)
		default:
			t.Errorf("dry run changed repository: %s %s", req.Method, req.URL)
		}
	})
	var out bytes.Buffer
	if err := releaseRunner(server, &out, true).run(); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out.String(), "Would open release pull request for 0.1.0 on release-0.1.0") {
		t.Fatalf("output = %q", out.String())
	}
	assertRequests(t, *requests, "GET "+apiPrefix+"/pulls?state=open&limit=50&page=1", "GET "+apiPrefix+"/tags?limit=50&page=1")
}

func TestRecoverCommittedBranch(t *testing.T) {
	t.Parallel()
	updated, err := rewriteChangelog(unreleased, "0.1.0", "https://forge.example/o/r", time.Date(2026, 9, 25, 0, 0, 0, 0, time.UTC))
	if err != nil {
		t.Fatal(err)
	}
	server, requests := releaseServer(t, "0.1.0", unreleased, func(w http.ResponseWriter, req *http.Request) {
		switch {
		case req.URL.Path == apiPrefix+"/pulls" && req.Method == http.MethodGet:
			fmt.Fprint(w, `[]`)
		case req.URL.Path == apiPrefix+"/tags":
			fmt.Fprint(w, `[]`)
		case strings.HasPrefix(req.URL.Path, apiPrefix+"/contents/"):
			if req.URL.Query().Get("ref") == "main" {
				fmt.Fprint(w, `{"sha":"blob-sha"}`)
				return
			}
			var text string
			if strings.HasSuffix(req.URL.Path, "CHANGELOG.md") {
				text = updated
			} else {
				text = "0.1.0\n"
			}
			fmt.Fprintf(w, `{"content":%q}`, base64.StdEncoding.EncodeToString([]byte(text)))
		case req.URL.Path == apiPrefix+"/contents":
			w.WriteHeader(http.StatusConflict)
			fmt.Fprint(w, `{"message":"branch already exists"}`)
		case req.URL.Path == apiPrefix+"/pulls" && req.Method == http.MethodPost:
			w.WriteHeader(http.StatusCreated)
			fmt.Fprint(w, `{"html_url":"https://forge.example/o/r/pulls/35"}`)
		default:
			t.Errorf("unexpected request %s %s", req.Method, req.URL)
		}
	})
	var out bytes.Buffer
	if err := releaseRunner(server, &out, false).run(); err != nil {
		t.Fatal(err)
	}
	if out.String() != "https://forge.example/o/r/pulls/35\n" {
		t.Fatalf("output = %q", out.String())
	}
	assertRequests(t, *requests, "GET "+apiPrefix+"/pulls?state=open&limit=50&page=1", "GET "+apiPrefix+"/tags?limit=50&page=1", "GET "+apiPrefix+"/contents/CHANGELOG.md?ref=main", "GET "+apiPrefix+"/contents/version.txt?ref=main", "POST "+apiPrefix+"/contents", "GET "+apiPrefix+"/contents/CHANGELOG.md?ref=release-0.1.0", "GET "+apiPrefix+"/contents/version.txt?ref=release-0.1.0", "POST "+apiPrefix+"/pulls")
}
