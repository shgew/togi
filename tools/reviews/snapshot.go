package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	neturl "net/url"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
)

const snapshotUsage = "usage: reviews snapshot [--previous <record comment URL> [--previous-base <full base SHA>]] [--cover <file or ->] <PR number>\n       reviews snapshot --cover <file or -> <snapshot directory>"

// tools holds what the subcommands reach outside the process through, so tests inject fakes.
type tools struct {
	gh     ghFunc
	git    gitFunc
	getenv func(string) string
	// tmp is the directory holding togi-review/.
	tmp    string
	stdin  io.Reader
	stdout io.Writer
	stderr io.Writer
}

// pullInfo is the head and base a snapshot freezes.
type pullInfo struct {
	Head string `json:"headRefOid"`
	Base string `json:"baseRefOid"`
	URL  string `json:"url"`
}

var pullURL = regexp.MustCompile(`^https://github\.com/([^/]+/[^/]+)/pull/\d+$`)

func viewPull(gh ghFunc, pr int) (pullInfo, error) {
	out, err := gh("pr", "view", strconv.Itoa(pr), "--json", "headRefOid,baseRefOid,url")
	if err != nil {
		return pullInfo{}, fmt.Errorf("view pull request #%d: %w", pr, err)
	}
	var p pullInfo
	if err := json.Unmarshal(out, &p); err != nil {
		return pullInfo{}, fmt.Errorf("decode pull request #%d: %w", pr, err)
	}
	if p.Head == "" || p.Base == "" {
		return pullInfo{}, fmt.Errorf("pull request #%d has no head or base commit", pr)
	}
	return p, nil
}

func (p pullInfo) repository() (string, error) {
	m := pullURL.FindStringSubmatch(p.URL)
	if m == nil {
		return "", fmt.Errorf("cannot read the repository from pull request URL %q", p.URL)
	}
	return m[1], nil
}

func runSnapshot(t tools, args []string) error {
	fs := flag.NewFlagSet("snapshot", flag.ContinueOnError)
	fs.SetOutput(t.stderr)
	previous := fs.String("previous", "", "URL of the previous review record comment, for a re-review")
	previousBase := fs.String("previous-base", "", "full SHA of the base the pull request had at the previous record's head, for a record that does not name it (version 1, or 2 without base_sha)")
	coverFile := fs.String("cover", "", "file with `just cover` output, or - for standard input")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if fs.NArg() != 1 {
		return errors.New(snapshotUsage)
	}
	var coverText string
	if *coverFile != "" {
		b, err := readInput(t.stdin, *coverFile)
		if err != nil {
			return fmt.Errorf("read cover output: %w", err)
		}
		coverText = string(b)
	}
	target := fs.Arg(0)
	pr, err := strconv.Atoi(target)
	if err != nil {
		if *coverFile == "" || *previous != "" || *previousBase != "" {
			return errors.New(snapshotUsage)
		}
		m, err := addCover(target, coverText)
		if err != nil {
			return err
		}
		return printSummary(t.stdout, target, m)
	}
	if *previousBase != "" && (*previous == "" || !previousBaseSHA.MatchString(*previousBase)) {
		return fmt.Errorf("--previous-base needs --previous and a full lower-case commit SHA\n%s", snapshotUsage)
	}
	dir, m, err := createSnapshot(t, pr, *previous, *previousBase)
	if err != nil {
		return err
	}
	if *coverFile != "" {
		if m, err = addCover(dir, coverText); err != nil {
			return err
		}
	}
	return printSummary(t.stdout, dir, m)
}

func readInput(stdin io.Reader, name string) ([]byte, error) {
	if name == "-" {
		return io.ReadAll(stdin)
	}
	return os.ReadFile(name)
}

var errHeadRecorded = errors.New("the previous record already covers this head")

// createSnapshot freezes pull request pr and writes its snapshot directory. It refuses when the head or base moved while it ran. previousBase, when set, is the base the previous record's head had, for a record that does not name it.
func createSnapshot(t tools, pr int, previousURL, previousBase string) (string, manifest, error) {
	before, err := viewPull(t.gh, pr)
	if err != nil {
		return "", manifest{}, err
	}
	repo, err := before.repository()
	if err != nil {
		return "", manifest{}, err
	}
	// The diff goes to files, never to a terminal, so gh's guard against escape sequences, which a diff of terminal test data trips, does not apply.
	raw, err := t.gh("pr", "diff", strconv.Itoa(pr), "--color=never", "--allow-escape-sequences")
	if err != nil {
		return "", manifest{}, fmt.Errorf("fetch the diff of pull request #%d: %w", pr, err)
	}
	after, err := viewPull(t.gh, pr)
	if err != nil {
		return "", manifest{}, err
	}
	if err := movedSince(pr, before, after); err != nil {
		return "", manifest{}, err
	}
	prFiles, err := parseDiff(string(raw))
	if err != nil {
		return "", manifest{}, fmt.Errorf("read the diff of pull request #%d: %w", pr, err)
	}
	m := manifest{
		Version: manifestVersion, Repository: repo, PullRequest: pr, HeadSHA: before.Head, BaseSHA: before.Base,
		Small: isSmall(prFiles), PRFiles: len(prFiles), PRLines: totalLines(prFiles),
	}
	scope := prFiles
	var delta []byte
	if previousURL != "" {
		if err := previousRecordOf(t, repo, pr, previousURL, previousBase, &m); err != nil {
			return "", manifest{}, err
		}
		if m.Fallback == "" {
			d, err := reviewDelta(t, m)
			if err != nil {
				return "", manifest{}, err
			}
			m.Patches, m.CarryForward = d.Patches, d.carryForward() && m.Previous.Verdict == "success"
			scope = mergeByPath(d.Files)
			delta = bytes.Join(d.Texts, nil)
		}
	}
	// Everything that reaches out for the frozen head comes before the last look at the pull request.
	complete := m.Previous == nil || m.Fallback != ""
	exclusions := make([]string, len(scope))
	for i, f := range scope {
		if exclusions[i], err = exclusionOf(t, repo, m.HeadSHA, f, complete); err != nil {
			return "", manifest{}, err
		}
	}
	last, err := viewPull(t.gh, pr)
	if err != nil {
		return "", manifest{}, err
	}
	if err := movedSince(pr, before, last); err != nil {
		return "", manifest{}, err
	}
	dir := filepath.Join(t.tmp, "togi-review", fmt.Sprintf("%d-%s", pr, short(before.Head)))
	if err := os.RemoveAll(dir); err != nil {
		return "", manifest{}, fmt.Errorf("clear %s: %w", dir, err)
	}
	if err := os.MkdirAll(filepath.Join(dir, "files"), 0o755); err != nil {
		return "", manifest{}, fmt.Errorf("create %s: %w", dir, err)
	}
	if err := os.WriteFile(filepath.Join(dir, "pr.diff"), raw, 0o644); err != nil {
		return "", manifest{}, fmt.Errorf("write pr.diff: %w", err)
	}
	if m.Previous != nil && m.Fallback == "" {
		if err := os.WriteFile(filepath.Join(dir, "delta.diff"), delta, 0o644); err != nil {
			return "", manifest{}, fmt.Errorf("write delta.diff: %w", err)
		}
	}
	m.Files = []manifestFile{}
	for i, f := range scope {
		mf := manifestFile{Path: f.Path, OldPath: f.OldPath, Status: f.Status, Added: f.Added, Removed: f.Removed, Binary: f.Binary, Exclusion: exclusions[i]}
		if mf.Exclusion == "" {
			mf.HunkFile = hunkFileName(i + 1)
			if err := os.WriteFile(filepath.Join(dir, mf.HunkFile), []byte(f.Text), 0o644); err != nil {
				return "", manifest{}, fmt.Errorf("write %s: %w", mf.HunkFile, err)
			}
			m.IncludedFiles++
			m.IncludedLines += f.Added + f.Removed
		}
		m.Files = append(m.Files, mf)
	}
	m.Reviewers = reviewerCount(m.IncludedLines, m.IncludedFiles)
	return dir, m, writeManifest(dir, m)
}

// movedSince refuses a snapshot whose pull request head or base is no longer the frozen one.
func movedSince(pr int, before, after pullInfo) error {
	if after.Head != before.Head || after.Base != before.Base {
		return fmt.Errorf("pull request #%d moved while the snapshot ran (head %s, base %s before; head %s, base %s after): run snapshot again", pr, before.Head, before.Base, after.Head, after.Base)
	}
	return nil
}

// exclusionOf names why the review skips the file, or "" when it is included. Paths and kinds exclude by themselves. Whether a file is generated is decided on its contents, never on marker lines a patch shows: a marker in context or outside the hunks counts, one after the package clause or inside a string does not, and one an earlier patch of a delta added and a later one dropped does not either. complete says the sections hold each file's whole change, as the pull request's diff does: an added or deleted file's section then is its whole content and needs no read. Any other file is read at the frozen head; one the head lacks has no contents to judge and stays in the review.
func exclusionOf(t tools, repo, head string, f fileDiff, complete bool) (string, error) {
	if reason := fixedExclusion(f); reason != "" {
		return reason, nil
	}
	content := ""
	if complete && (f.Status == "added" || f.Status == "deleted") {
		content = wholeContent(f)
	} else {
		var err error
		if content, _, err = fileAt(t.gh, repo, head, f.Path); err != nil {
			return "", err
		}
	}
	if generatedContent(content) {
		return "generated", nil
	}
	return "", nil
}

// fileAt reads a file at a commit through the GitHub API, so the reviewing host needs no checkout of it. A file the commit lacks is not an error.
func fileAt(gh ghFunc, repo, ref, name string) (string, bool, error) {
	segments := strings.Split(name, "/")
	for i, s := range segments {
		segments[i] = neturl.PathEscape(s)
	}
	out, err := gh("api", "-H", "Accept: application/vnd.github.raw+json", "repos/"+repo+"/contents/"+strings.Join(segments, "/")+"?ref="+ref)
	if err != nil {
		if strings.Contains(err.Error(), "HTTP 404") {
			return "", false, nil
		}
		return "", false, fmt.Errorf("read %s at %s: %w", name, short(ref), err)
	}
	return string(out), true, nil
}

func short(sha string) string { return sha[:min(8, len(sha))] }

var commentURL = regexp.MustCompile(`^https://github\.com/([^/]+/[^/]+)/(?:pull|issues)/(\d+)#issuecomment-(\d+)$`)

// previousBaseSHA is what --previous-base accepts: a complete lower-case commit SHA, SHA-1 or SHA-256.
var previousBaseSHA = regexp.MustCompile(`^(?:[0-9a-f]{40}|[0-9a-f]{64})$`)

// previousRecordOf reads the previous record comment and sets m.Previous. A record that lacks its base takes previousBase when the caller recovered one; without either it cannot be diffed and m.Fallback says the whole layer is reviewed.
func previousRecordOf(t tools, repo string, pr int, url, previousBase string, m *manifest) error {
	match := commentURL.FindStringSubmatch(url)
	if match == nil {
		return fmt.Errorf("previous %q is not a pull request comment URL", url)
	}
	if match[1] != repo || match[2] != strconv.Itoa(pr) {
		return fmt.Errorf("previous record %s is not on pull request #%d of %s", url, pr, repo)
	}
	out, err := t.gh("api", "repos/"+repo+"/issues/comments/"+match[3])
	if err != nil {
		return fmt.Errorf("fetch the previous record: %w", err)
	}
	var c struct {
		Body string `json:"body"`
		User struct {
			Login string `json:"login"`
		} `json:"user"`
	}
	if err := json.Unmarshal(out, &c); err != nil {
		return fmt.Errorf("decode the previous record comment: %w", err)
	}
	if c.User.Login != robotogiBotLogin {
		return fmt.Errorf("previous record %s was posted by %q, not %s", url, c.User.Login, robotogiBotLogin)
	}
	r, err := parseRecord(c.Body)
	if err != nil {
		return fmt.Errorf("read the previous record %s: %w", url, err)
	}
	if r.HeadSHA == "" {
		return fmt.Errorf("previous record %s names no head_sha", url)
	}
	if r.HeadSHA == m.HeadSHA {
		return fmt.Errorf("%w: %s", errHeadRecorded, url)
	}
	base := r.BaseSHA
	switch {
	case base == "":
		base = previousBase
	case previousBase != "" && previousBase != base:
		return fmt.Errorf("previous record %s names base %s, not the supplied --previous-base %s", url, base, previousBase)
	}
	m.Previous = &previousRecord{URL: url, HeadSHA: r.HeadSHA, BaseSHA: base, Verdict: r.Verdict}
	if base == "" {
		m.Fallback = fmt.Sprintf("the previous record is version %d and does not name the base the pull request had at its head, so no minimal delta can be established", r.Version)
	}
	return nil
}

// reviewDelta fetches the commits of both ranges and classifies the layer's patches between them.
func reviewDelta(t tools, m manifest) (deltaResult, error) {
	p := m.Previous
	if _, err := t.git("fetch", "--quiet", "origin", p.BaseSHA, p.HeadSHA, m.BaseSHA, m.HeadSHA); err != nil {
		return deltaResult{}, fmt.Errorf("fetch the commits of both ranges: %w", err)
	}
	return classifyRange(t.git, p.BaseSHA, p.HeadSHA, m.BaseSHA, m.HeadSHA)
}

// printSummary prints what a coordinator needs to start: where the snapshot is, what it froze and how to size the review.
func printSummary(w io.Writer, dir string, m manifest) error {
	var b strings.Builder
	fmt.Fprintf(&b, "snapshot %s\n", dir)
	fmt.Fprintf(&b, "pull request #%d in %s\nhead %s\nbase %s\n", m.PullRequest, m.Repository, m.HeadSHA, m.BaseSHA)
	if m.Previous != nil {
		fmt.Fprintf(&b, "previous record %s on %s\n", m.Previous.URL, m.Previous.HeadSHA)
		if m.Fallback != "" {
			fmt.Fprintf(&b, "fallback: %s\n", m.Fallback)
		} else {
			counts := map[string]int{}
			context := 0
			for _, p := range m.Patches {
				counts[p.Status]++
				if p.ContextOnly {
					context++
				}
			}
			fmt.Fprintf(&b, "patches: %d unchanged (%d with only context changed), %d changed, %d added, %d removed\n", counts["unchanged"], context, counts["changed"], counts["added"], counts["removed"])
			switch {
			case m.CarryForward:
				b.WriteString("every patch is unchanged: carry the record forward with a check on the new head\n")
			case counts["changed"]+counts["added"]+counts["removed"] == 0:
				verdict := m.Previous.Verdict
				if verdict == "" {
					verdict = "not recorded"
				}
				fmt.Fprintf(&b, "every patch is unchanged, but the previous record's verdict is %s, not success: it is not carried forward and the new head needs its own record\n", verdict)
			}
		}
	}
	fmt.Fprintf(&b, "files: %d, included %d, excluded %d\n", len(m.Files), m.IncludedFiles, len(m.Files)-m.IncludedFiles)
	for _, f := range m.Files {
		switch {
		case f.Exclusion != "":
			fmt.Fprintf(&b, "  excluded %s +%d -%d: %s\n", f.Path, f.Added, f.Removed, f.Exclusion)
		default:
			fmt.Fprintf(&b, "  %s %s +%d -%d\n", f.HunkFile, f.Path, f.Added, f.Removed)
		}
	}
	fmt.Fprintf(&b, "L %d, F %d: %d reviewers\n", m.IncludedLines, m.IncludedFiles, m.Reviewers)
	fmt.Fprintf(&b, "small pull request: %t (whole diff %d files, %d lines)\n", m.Small, m.PRFiles, m.PRLines)
	switch {
	case m.Coverage != nil:
		fmt.Fprintf(&b, "uncovered: %d of %d reported ranges inside the hunks, %d files not built\n", len(m.Coverage.Ranges), m.Coverage.Reported, len(m.Coverage.NotBuilt))
		for _, r := range m.Coverage.Ranges {
			fmt.Fprintf(&b, "  %s:%d-%d\n", r.Path, r.First, r.Last)
		}
	case m.changesGo():
		b.WriteString("uncovered: not recorded yet; run `just cover <base>` and pass it to `snapshot --cover`\n")
	}
	_, err := io.WriteString(w, b.String())
	return err
}
