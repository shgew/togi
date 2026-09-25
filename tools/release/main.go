// Command release prepares and publishes shycler releases through the Forgejo API.
package main

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"regexp"
	"strings"
	"time"
)

type repository struct {
	owner, name, webURL string
}

var repoPart = regexp.MustCompile(`^[A-Za-z0-9_.-]+$`)

func parseRepository(name, webURL string) (repository, error) {
	owner, repo, ok := strings.Cut(name, "/")
	if !ok || !repoPart.MatchString(owner) || !repoPart.MatchString(repo) || owner == "." || owner == ".." || repo == "." || repo == ".." {
		return repository{}, fmt.Errorf("invalid repository %q (expected owner/name)", name)
	}
	u, err := url.Parse(webURL)
	if err != nil || u.Scheme != "https" || u.Host == "" || u.User != nil || u.Path != "" || u.RawQuery != "" || u.Fragment != "" {
		return repository{}, fmt.Errorf("invalid Forgejo URL %q (expected https://host)", webURL)
	}
	return repository{owner, repo, strings.TrimRight(webURL, "/") + "/" + name}, nil
}

func parseRemote(remote string) (repository, string, error) {
	u, err := url.Parse(strings.TrimSpace(remote))
	if err != nil || (u.Scheme != "https" && u.Scheme != "ssh") || u.Host == "" || u.RawQuery != "" || u.Fragment != "" || (u.Scheme == "ssh" && (u.User == nil || u.User.Username() != "git")) || (u.Scheme == "https" && u.User != nil) {
		return repository{}, "", fmt.Errorf("unsupported remote URL %q", remote)
	}
	name := strings.TrimPrefix(u.Path, "/")
	name = strings.TrimSuffix(name, ".git")
	webURL := "https://" + u.Host
	r, err := parseRepository(name, webURL)
	if err != nil {
		return repository{}, "", fmt.Errorf("remote URL %q: %w", remote, err)
	}
	return r, webURL, nil
}

func remoteOrigin() (repository, string, error) {
	output, err := exec.Command("git", "config", "--get", "remote.origin.url").Output()
	if err != nil {
		return repository{}, "", fmt.Errorf("read remote.origin.url with git config: %w", err)
	}
	return parseRemote(string(output))
}

type forgejo struct {
	base, token string
	client      *http.Client
}

func (f forgejo) request(method, path string, body any, result any, allowed ...int) (int, error) {
	var input io.Reader
	if body != nil {
		encoded, err := json.Marshal(body)
		if err != nil {
			return 0, fmt.Errorf("encode %s %s: %w", method, path, err)
		}
		input = bytes.NewReader(encoded)
	}
	req, err := http.NewRequest(method, strings.TrimRight(f.base, "/")+path, input)
	if err != nil {
		return 0, fmt.Errorf("prepare %s %s: %w", method, path, err)
	}
	req.Header.Set("Authorization", "token "+f.token)
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := f.client.Do(req)
	if err != nil {
		return 0, fmt.Errorf("%s %s: %w", method, path, err)
	}
	defer resp.Body.Close()
	data, err := io.ReadAll(resp.Body)
	if err != nil {
		return 0, fmt.Errorf("read %s %s response: %w", method, path, err)
	}
	for _, status := range allowed {
		if resp.StatusCode == status {
			return status, nil
		}
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return resp.StatusCode, fmt.Errorf("%s %s: HTTP %d: %s", method, path, resp.StatusCode, strings.TrimSpace(string(data)))
	}
	if result != nil {
		if err := json.Unmarshal(data, result); err != nil {
			return 0, fmt.Errorf("decode %s %s response: %w", method, path, err)
		}
	}
	return resp.StatusCode, nil
}

func (f forgejo) repoPath(r repository) string {
	return "/repos/" + url.PathEscape(r.owner) + "/" + url.PathEscape(r.name)
}

type pull struct {
	HTMLURL        string `json:"html_url"`
	Merged         bool   `json:"merged"`
	MergeCommitSHA string `json:"merge_commit_sha"`
	Head           struct {
		Ref string `json:"ref"`
	} `json:"head"`
}

func (f forgejo) pulls(r repository, state string) ([]pull, error) {
	var all []pull
	for page := 1; ; page++ {
		var batch []pull
		path := fmt.Sprintf("%s/pulls?state=%s&limit=50&page=%d", f.repoPath(r), state, page)
		if _, err := f.request(http.MethodGet, path, nil, &batch); err != nil {
			return nil, err
		}
		all = append(all, batch...)
		if len(batch) < 50 {
			return all, nil
		}
	}
}

func (f forgejo) hasTags(r repository) (bool, error) {
	for page := 1; ; page++ {
		var batch []struct {
			Name string `json:"name"`
		}
		path := fmt.Sprintf("%s/tags?limit=50&page=%d", f.repoPath(r), page)
		if _, err := f.request(http.MethodGet, path, nil, &batch); err != nil {
			return false, err
		}
		for _, tag := range batch {
			if strings.HasPrefix(tag.Name, "v") {
				return true, nil
			}
		}
		if len(batch) < 50 {
			return false, nil
		}
	}
}

type runner struct {
	api    forgejo
	repo   repository
	now    func() time.Time
	out    io.Writer
	dryRun bool
}

func (r runner) run() error {
	prefix := r.api.repoPath(r.repo)
	var info struct {
		DefaultBranch string `json:"default_branch"`
	}
	if _, err := r.api.request(http.MethodGet, prefix, nil, &info); err != nil {
		return fmt.Errorf("read repository: %w", err)
	}
	if info.DefaultBranch == "" {
		return errors.New("read repository: missing default_branch")
	}
	branch := url.QueryEscape(info.DefaultBranch)
	var rawVersion, changelog string
	for _, file := range []struct {
		path string
		into *string
	}{{"version.txt", &rawVersion}, {"CHANGELOG.md", &changelog}} {
		path := prefix + "/raw/" + file.path + "?ref=" + branch
		var data string
		if err := r.raw(path, &data); err != nil {
			return fmt.Errorf("read %s: %w", file.path, err)
		}
		*file.into = data
	}
	version := strings.TrimSpace(rawVersion)
	if !versionPattern.MatchString(version) {
		return fmt.Errorf("read version.txt: invalid semantic version %q", version)
	}
	if s, exists := sectionNamed(changelog, version); exists && s.date != "" {
		path := prefix + "/tags/v" + url.PathEscape(version)
		status, err := r.api.request(http.MethodGet, path, nil, nil, http.StatusNotFound)
		if err != nil {
			return fmt.Errorf("check release tag: %w", err)
		}
		if status == http.StatusNotFound {
			return r.tag(version, s, changelog)
		}
	}
	return r.releasePullRequest(info.DefaultBranch, version, changelog)
}

func (r runner) raw(path string, data *string) error {
	req, err := http.NewRequest(http.MethodGet, strings.TrimRight(r.api.base, "/")+path, nil)
	if err != nil {
		return fmt.Errorf("prepare GET %s: %w", path, err)
	}
	req.Header.Set("Authorization", "token "+r.api.token)
	resp, err := r.api.client.Do(req)
	if err != nil {
		return fmt.Errorf("GET %s: %w", path, err)
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return fmt.Errorf("read GET %s response: %w", path, err)
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("GET %s: HTTP %d: %s", path, resp.StatusCode, strings.TrimSpace(string(body)))
	}
	*data = string(body)
	return nil
}

func (r runner) tag(version string, s section, changelog string) error {
	pulls, err := r.api.pulls(r.repo, "closed")
	if err != nil {
		return fmt.Errorf("tag release: find merged release pull request: %w", err)
	}
	for _, p := range pulls {
		if p.Head.Ref != "release-"+version || !p.Merged {
			continue
		}
		if p.MergeCommitSHA == "" {
			return fmt.Errorf("tag release: merged release-%s pull request has no merge_commit_sha", version)
		}
		if r.dryRun {
			fmt.Fprintf(r.out, "Would publish release v%s at merge commit %s\n", version, p.MergeCommitSHA)
			return nil
		}
		var result struct {
			HTMLURL string `json:"html_url"`
		}
		body := map[string]any{
			"tag_name": "v" + version, "target_commitish": p.MergeCommitSHA,
			"name": version, "body": releaseNotes(changelog, s),
		}
		if _, err := r.api.request(http.MethodPost, r.api.repoPath(r.repo)+"/releases", body, &result); err != nil {
			return fmt.Errorf("tag release: create release: %w", err)
		}
		fmt.Fprintln(r.out, result.HTMLURL)
		return nil
	}
	return fmt.Errorf("tag release: no merged pull request with head release-%s", version)
}

func (r runner) releasePullRequest(branch, version, changelog string) error {
	pulls, err := r.api.pulls(r.repo, "open")
	if err != nil {
		return fmt.Errorf("release pull request: find open pull requests: %w", err)
	}
	for _, p := range pulls {
		if strings.HasPrefix(p.Head.Ref, "release-") {
			fmt.Fprintln(r.out, p.HTMLURL)
			return nil
		}
	}
	s, ok := sectionNamed(changelog, "Unreleased")
	if !ok {
		return errors.New("release pull request: missing [Unreleased] section")
	}
	if len(entries(s.body)) == 0 {
		fmt.Fprintln(r.out, "nothing to release")
		return nil
	}
	hasTags, err := r.api.hasTags(r.repo)
	if err != nil {
		return fmt.Errorf("release pull request: list tags: %w", err)
	}
	next, reason := version, "First release; version.txt sets the initial version."
	if hasTags {
		next, reason, err = bump(version, s.body)
		if err != nil {
			return fmt.Errorf("release pull request: bump version: %w", err)
		}
	} else if _, exists := sectionNamed(changelog, version); exists {
		next, reason, err = bump(version, s.body)
		if err != nil {
			return fmt.Errorf("release pull request: bump version: %w", err)
		}
	}
	updated, err := rewriteChangelog(changelog, next, r.repo.webURL, r.now())
	if err != nil {
		return fmt.Errorf("release pull request: rewrite changelog: %w", err)
	}
	if r.dryRun {
		fmt.Fprintf(r.out, "Would open release pull request for %s on release-%s: %s\n", next, next, reason)
		return nil
	}
	prefix := r.api.repoPath(r.repo)
	files := make([]map[string]string, 0, 2)
	for _, file := range []struct{ path, text string }{{"CHANGELOG.md", updated}, {"version.txt", next + "\n"}} {
		var contents struct {
			SHA string `json:"sha"`
		}
		path := prefix + "/contents/" + file.path + "?ref=" + url.QueryEscape(branch)
		if _, err := r.api.request(http.MethodGet, path, nil, &contents); err != nil {
			return fmt.Errorf("release pull request: read %s SHA: %w", file.path, err)
		}
		if contents.SHA == "" {
			return fmt.Errorf("release pull request: missing %s SHA", file.path)
		}
		files = append(files, map[string]string{"operation": "update", "path": file.path, "content": base64.StdEncoding.EncodeToString([]byte(file.text)), "sha": contents.SHA})
	}
	newBranch := "release-" + next
	commit := map[string]any{"branch": branch, "new_branch": newBranch, "message": "Release " + next, "files": files}
	if status, err := r.api.request(http.MethodPost, prefix+"/contents", commit, nil); err != nil {
		if status != http.StatusConflict && status != http.StatusUnprocessableEntity {
			return fmt.Errorf("release pull request: commit release files: %w", err)
		}
		matched, checkErr := r.branchMatches(newBranch, files)
		if checkErr != nil {
			return fmt.Errorf("release pull request: commit release files: %w (check existing branch: %v)", err, checkErr)
		}
		if !matched {
			return fmt.Errorf("release pull request: commit release files: %w (existing branch has different contents)", err)
		}
	}
	var created struct {
		HTMLURL string `json:"html_url"`
	}
	pr := map[string]string{"title": "Release " + next, "head": newBranch, "base": branch, "body": "Release " + next + "\n\n" + reason}
	if _, err := r.api.request(http.MethodPost, prefix+"/pulls", pr, &created); err != nil {
		return fmt.Errorf("release pull request: open pull request: %w", err)
	}
	fmt.Fprintln(r.out, created.HTMLURL)
	return nil
}

func (r runner) branchMatches(branch string, files []map[string]string) (bool, error) {
	for _, file := range files {
		path := r.api.repoPath(r.repo) + "/contents/" + file["path"] + "?ref=" + url.QueryEscape(branch)
		var contents struct {
			Content string `json:"content"`
		}
		if _, err := r.api.request(http.MethodGet, path, nil, &contents); err != nil {
			return false, err
		}
		got, err := base64.StdEncoding.DecodeString(contents.Content)
		if err != nil {
			return false, fmt.Errorf("decode existing %s: %w", file["path"], err)
		}
		want, _ := base64.StdEncoding.DecodeString(file["content"])
		if !bytes.Equal(got, want) {
			return false, nil
		}
	}
	return true, nil
}

func main() {
	var repoFlag, urlFlag string
	var dryRun bool
	flag.StringVar(&repoFlag, "repo", "", "Forgejo repository (owner/name; defaults to remote.origin.url)")
	flag.StringVar(&urlFlag, "url", "", "Forgejo server (https://host; defaults to remote.origin.url)")
	flag.BoolVar(&dryRun, "dry-run", false, "report the due step without making changes")
	flag.Parse()
	if flag.NArg() != 0 {
		fmt.Fprintln(os.Stderr, "release: unexpected positional arguments")
		os.Exit(2)
	}
	if err := runCLI(repoFlag, urlFlag, dryRun); err != nil {
		fmt.Fprintln(os.Stderr, "release:", err)
		os.Exit(1)
	}
}

func runCLI(repoFlag, urlFlag string, dryRun bool) error {
	token := os.Getenv("FORGEJO_TOKEN")
	if token == "" {
		return errors.New("FORGEJO_TOKEN is unset")
	}
	if repoFlag == "" || urlFlag == "" {
		origin, webURL, err := remoteOrigin()
		if err != nil {
			return err
		}
		if repoFlag == "" {
			repoFlag = origin.owner + "/" + origin.name
		}
		if urlFlag == "" {
			urlFlag = webURL
		}
	}
	repo, err := parseRepository(repoFlag, urlFlag)
	if err != nil {
		return err
	}
	return runner{
		api:  forgejo{base: urlFlag + "/api/v1", token: token, client: http.DefaultClient},
		repo: repo, now: time.Now, out: os.Stdout, dryRun: dryRun,
	}.run()
}
