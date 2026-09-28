// Command release builds togi's release commit and publishes the release, from the release workflow.
package main

import (
	"bytes"
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

const (
	remote        = "origin"
	defaultBranch = "main"
	checkWorkflow = "check.yml"
)

var repoPart = regexp.MustCompile(`^[A-Za-z0-9_.-]+$`)
var modulePath = regexp.MustCompile(`(?m)^module[ \t]+(\S+)[ \t]*$`)

type gitCmd struct {
	args  []string
	env   []string
	stdin string
}

type gitFunc func(gitCmd) (stdout, stderr string, err error)

func runGit(c gitCmd) (string, string, error) {
	cmd := exec.Command("git", c.args...)
	cmd.Env = append(os.Environ(), c.env...)
	cmd.Stdin = strings.NewReader(c.stdin)
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	err := cmd.Run()
	if err != nil {
		err = fmt.Errorf("git %s: %w: %s", strings.Join(c.args, " "), err, strings.TrimSpace(stderr.String()))
	}
	return stdout.String(), stderr.String(), err
}

type repository struct {
	owner, name string
}

func parseRepository(name string) (repository, error) {
	owner, repo, ok := strings.Cut(name, "/")
	if !ok || !repoPart.MatchString(owner) || !repoPart.MatchString(repo) || owner == "." || owner == ".." || repo == "." || repo == ".." {
		return repository{}, fmt.Errorf("invalid repository %q (expected owner/name)", name)
	}
	return repository{owner, repo}, nil
}

type github struct {
	base, token string
	client      *http.Client
}

func (g github) request(method, path string, body any, result any, allowed ...int) (int, error) {
	var input io.Reader
	if body != nil {
		encoded, err := json.Marshal(body)
		if err != nil {
			return 0, fmt.Errorf("encode %s %s: %w", method, path, err)
		}
		input = bytes.NewReader(encoded)
	}
	req, err := http.NewRequest(method, strings.TrimRight(g.base, "/")+path, input)
	if err != nil {
		return 0, fmt.Errorf("prepare %s %s: %w", method, path, err)
	}
	req.Header.Set("Authorization", "Bearer "+g.token)
	req.Header.Set("Accept", "application/vnd.github+json")
	req.Header.Set("X-GitHub-Api-Version", "2022-11-28")
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := g.client.Do(req)
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

type runner struct {
	git          gitFunc
	now          func() time.Time
	out          io.Writer
	commit       bool
	requireGreen func(commit string) error
}

func (r runner) output(args ...string) (string, error) {
	stdout, _, err := r.git(gitCmd{args: args})
	return stdout, err
}

func (r runner) release() error {
	if _, err := r.output("fetch", "--quiet", remote, defaultBranch); err != nil {
		return fmt.Errorf("fetch %s: %w", defaultBranch, err)
	}
	base := remote + "/" + defaultBranch
	baseCommit, err := r.output("rev-parse", "--verify", base+"^{commit}")
	if err != nil {
		return fmt.Errorf("resolve %s: %w", base, err)
	}
	baseCommit = strings.TrimSpace(baseCommit)
	files := map[string]string{}
	for _, path := range []string{"version.txt", "CHANGELOG.md", "go.mod"} {
		text, err := r.output("show", baseCommit+":"+path)
		if err != nil {
			return fmt.Errorf("read %s from %s: %w", path, base, err)
		}
		files[path] = text
	}
	version := strings.TrimSpace(files["version.txt"])
	if !versionPattern.MatchString(version) {
		return fmt.Errorf("read version.txt: invalid semantic version %q", version)
	}
	module := modulePath.FindStringSubmatch(files["go.mod"])
	if module == nil {
		return errors.New("read go.mod: missing module path")
	}
	changelog := files["CHANGELOG.md"]
	s, ok := sectionNamed(changelog, "Unreleased")
	if !ok {
		return errors.New("read CHANGELOG.md: missing [Unreleased] section")
	}
	tags, err := r.output("ls-remote", "--tags", remote, "refs/tags/v*")
	if err != nil {
		return fmt.Errorf("list release tags: %w", err)
	}
	current, released := sectionNamed(changelog, version)
	if len(entries(s.body)) == 0 {
		if released && current.date != "" && !hasTag(tags, "v"+version) {
			fmt.Fprintf(r.out, "%s is released in CHANGELOG.md but not yet published; publishing %s as it is\n", version, base)
			return r.checkout(baseCommit)
		}
		if r.commit {
			return errors.New("nothing to release: [Unreleased] in CHANGELOG.md is empty")
		}
		fmt.Fprintln(r.out, "nothing to release")
		return nil
	}
	next, reason := version, "First release; version.txt sets the initial version."
	if released || strings.TrimSpace(tags) != "" {
		if next, reason, err = bump(version, s.body); err != nil {
			return fmt.Errorf("bump version: %w", err)
		}
	}
	updated, err := rewriteChangelog(changelog, next, "https://"+module[1], r.now())
	if err != nil {
		return fmt.Errorf("rewrite changelog: %w", err)
	}
	message := "Release " + next + "\n\n" + reason + "\n"
	if !r.commit {
		notes, _ := sectionNamed(updated, next)
		fmt.Fprintf(r.out, "Would release from %s:\n\n%s\n%s\n", base, message, releaseNotes(updated, notes))
		return nil
	}
	if err := r.requireGreen(baseCommit); err != nil {
		return fmt.Errorf("require a passing check on %s: %w", base, err)
	}
	commit, err := r.commitFiles(baseCommit, message, map[string]string{"version.txt": next + "\n", "CHANGELOG.md": updated})
	if err != nil {
		return fmt.Errorf("commit release %s: %w", next, err)
	}
	fmt.Fprintf(r.out, "Release %s committed as %s on top of %s\n", next, commit, base)
	return r.checkout(commit)
}

func hasTag(lsRemote, tag string) bool {
	for line := range strings.SplitSeq(lsRemote, "\n") {
		if _, ref, ok := strings.Cut(line, "\t"); ok && ref == "refs/tags/"+tag {
			return true
		}
	}
	return false
}

func (r runner) checkout(commit string) error {
	if _, err := r.output("checkout", "--quiet", "--detach", commit); err != nil {
		return fmt.Errorf("check out %s: %w", commit, err)
	}
	return nil
}

func (r runner) commitFiles(parent, message string, files map[string]string) (string, error) {
	indexPath, err := r.output("rev-parse", "--git-path", "togi-release-index")
	if err != nil {
		return "", fmt.Errorf("locate temporary index: %w", err)
	}
	indexPath = strings.TrimSpace(indexPath)
	defer os.Remove(indexPath)
	env := []string{"GIT_INDEX_FILE=" + indexPath}
	indexed := func(stdin string, args ...string) (string, error) {
		stdout, _, err := r.git(gitCmd{args: args, env: env, stdin: stdin})
		return strings.TrimSpace(stdout), err
	}
	if _, err := indexed("", "read-tree", parent); err != nil {
		return "", fmt.Errorf("read parent tree: %w", err)
	}
	for _, path := range []string{"version.txt", "CHANGELOG.md"} {
		blob, err := indexed(files[path], "hash-object", "-w", "--stdin")
		if err != nil {
			return "", fmt.Errorf("store %s: %w", path, err)
		}
		if _, err := indexed("", "update-index", "--cacheinfo", "100644,"+blob+","+path); err != nil {
			return "", fmt.Errorf("stage %s: %w", path, err)
		}
	}
	tree, err := indexed("", "write-tree")
	if err != nil {
		return "", fmt.Errorf("write tree: %w", err)
	}
	commit, err := indexed(message, "commit-tree", tree, "-p", parent, "-F", "-")
	if err != nil {
		return "", fmt.Errorf("create commit: %w", err)
	}
	return commit, nil
}

func (r runner) publish(api github, repo repository) error {
	var files [2]string
	for i, path := range []string{"version.txt", "CHANGELOG.md"} {
		text, err := r.output("show", "HEAD:"+path)
		if err != nil {
			return fmt.Errorf("read %s: %w", path, err)
		}
		files[i] = text
	}
	version, changelog := strings.TrimSpace(files[0]), files[1]
	if !versionPattern.MatchString(version) {
		return fmt.Errorf("read version.txt: invalid semantic version %q", version)
	}
	s, released := sectionNamed(changelog, version)
	if !released || s.date == "" {
		fmt.Fprintf(r.out, "nothing to publish: CHANGELOG.md has no released [%s] section\n", version)
		return nil
	}
	prefix := "/repos/" + url.PathEscape(repo.owner) + "/" + url.PathEscape(repo.name)
	tag := "v" + version
	status, err := api.request(http.MethodGet, prefix+"/git/ref/tags/"+url.PathEscape(tag), nil, nil, http.StatusNotFound)
	if err != nil {
		return fmt.Errorf("check tag %s: %w", tag, err)
	}
	if status != http.StatusNotFound {
		fmt.Fprintf(r.out, "nothing to publish: %s already exists\n", tag)
		return nil
	}
	commit, err := r.output("log", "--first-parent", "-1", "--format=%H", "HEAD", "--", "version.txt")
	if err != nil {
		return fmt.Errorf("find the commit that set version.txt to %s: %w", version, err)
	}
	commit = strings.TrimSpace(commit)
	if commit == "" {
		return fmt.Errorf("find the commit that set version.txt to %s: no commit changed version.txt", version)
	}
	var result struct {
		HTMLURL string `json:"html_url"`
	}
	body := map[string]any{"tag_name": tag, "target_commitish": commit, "name": version, "body": releaseNotes(changelog, s)}
	if _, err := api.request(http.MethodPost, prefix+"/releases", body, &result); err != nil {
		return fmt.Errorf("create release %s: %w", tag, err)
	}
	fmt.Fprintln(r.out, result.HTMLURL)
	return nil
}

func requireGreenCheck(api github, repo repository, commit string) error {
	path := "/repos/" + url.PathEscape(repo.owner) + "/" + url.PathEscape(repo.name) + "/actions/workflows/" + checkWorkflow + "/runs"
	query := url.Values{"branch": {defaultBranch}, "event": {"push"}, "head_sha": {commit}, "per_page": {"1"}}
	var result struct {
		WorkflowRuns []struct {
			HTMLURL    string `json:"html_url"`
			Status     string `json:"status"`
			Conclusion string `json:"conclusion"`
		} `json:"workflow_runs"`
	}
	if _, err := api.request(http.MethodGet, path+"?"+query.Encode(), nil, &result); err != nil {
		return fmt.Errorf("look up the %s run for %s: %w", checkWorkflow, commit, err)
	}
	if len(result.WorkflowRuns) == 0 {
		return fmt.Errorf("no %s run on %s for %s", checkWorkflow, defaultBranch, commit)
	}
	run := result.WorkflowRuns[0]
	if run.Status != "completed" {
		return fmt.Errorf("check run %s for %s is %s; run just release again once it passes", run.HTMLURL, commit, run.Status)
	}
	if run.Conclusion != "success" {
		return fmt.Errorf("check run %s for %s concluded %s", run.HTMLURL, commit, run.Conclusion)
	}
	return nil
}

func main() {
	var commit, publish bool
	flag.BoolVar(&commit, "commit", false, "commit the next release on top of origin/main and check it out, once the check workflow passed on origin/main (release workflow; reads GITHUB_API_URL, GITHUB_REPOSITORY and GITHUB_TOKEN)")
	flag.BoolVar(&publish, "publish", false, "publish the release version.txt names at HEAD, if not yet published (release workflow; reads GITHUB_API_URL, GITHUB_REPOSITORY and GITHUB_TOKEN)")
	flag.Usage = func() {
		fmt.Fprintln(flag.CommandLine.Output(), "Usage: release [-commit | -publish]\n\nWithout flags, prints the release the release workflow would make from origin/main.")
		flag.PrintDefaults()
	}
	flag.Parse()
	if flag.NArg() != 0 || commit && publish {
		flag.Usage()
		os.Exit(2)
	}
	r := runner{git: runGit, now: time.Now, out: os.Stdout, commit: commit}
	if err := run(r, publish); err != nil {
		fmt.Fprintln(os.Stderr, "release:", err)
		os.Exit(1)
	}
}

func run(r runner, publish bool) error {
	if !r.commit && !publish {
		return r.release()
	}
	api, repo, err := githubFromEnv()
	if err != nil {
		return err
	}
	if publish {
		return r.publish(api, repo)
	}
	r.requireGreen = func(commit string) error { return requireGreenCheck(api, repo, commit) }
	return r.release()
}

func githubFromEnv() (github, repository, error) {
	env := map[string]string{}
	for _, key := range []string{"GITHUB_API_URL", "GITHUB_REPOSITORY", "GITHUB_TOKEN"} {
		if env[key] = os.Getenv(key); env[key] == "" {
			return github{}, repository{}, fmt.Errorf("%s is unset", key)
		}
	}
	repo, err := parseRepository(env["GITHUB_REPOSITORY"])
	if err != nil {
		return github{}, repository{}, err
	}
	return github{base: env["GITHUB_API_URL"], token: env["GITHUB_TOKEN"], client: http.DefaultClient}, repo, nil
}
