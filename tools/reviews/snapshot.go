package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
)

const snapshotUsage = "usage: reviews snapshot [--previous <record comment URL>] [--cover <file or ->] <PR number>\n       reviews snapshot --cover <file or -> <snapshot directory>"

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
		if *coverFile == "" || *previous != "" {
			return errors.New(snapshotUsage)
		}
		m, err := addCover(target, coverText)
		if err != nil {
			return err
		}
		return printSummary(t.stdout, target, m)
	}
	dir, m, err := createSnapshot(t, pr, *previous)
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

// createSnapshot freezes pull request pr and writes its snapshot directory. It refuses when the head or base moved while it ran.
func createSnapshot(t tools, pr int, previousURL string) (string, manifest, error) {
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
	if after.Head != before.Head || after.Base != before.Base {
		return "", manifest{}, fmt.Errorf("pull request #%d moved while the snapshot ran (head %s, base %s before; head %s, base %s after): run snapshot again", pr, before.Head, before.Base, after.Head, after.Base)
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
		if err := previousRecordOf(t, repo, pr, previousURL, &m); err != nil {
			return "", manifest{}, err
		}
		if m.Fallback == "" {
			d, err := reviewDelta(t, m)
			if err != nil {
				return "", manifest{}, err
			}
			m.Patches, m.CarryForward = d.Patches, d.carryForward()
			scope = mergeByPath(d.Files)
			delta = bytes.Join(d.Texts, nil)
		}
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
		mf := manifestFile{Path: f.Path, OldPath: f.OldPath, Status: f.Status, Added: f.Added, Removed: f.Removed, Binary: f.Binary, Exclusion: excludedReason(f)}
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

func short(sha string) string { return sha[:min(8, len(sha))] }

var commentURL = regexp.MustCompile(`^https://github\.com/([^/]+/[^/]+)/(?:pull|issues)/(\d+)#issuecomment-(\d+)$`)

// previousRecordOf reads the previous record comment and sets m.Previous. A record that lacks its base cannot be diffed: m.Fallback then says the whole layer is reviewed.
func previousRecordOf(t tools, repo string, pr int, url string, m *manifest) error {
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
	if !strings.HasPrefix(c.User.Login, robotogiLogin) {
		return fmt.Errorf("previous record %s was posted by %q, not %s", url, c.User.Login, robotogiLogin)
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
	m.Previous = &previousRecord{URL: url, HeadSHA: r.HeadSHA, BaseSHA: r.BaseSHA}
	if r.BaseSHA == "" {
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
			if m.CarryForward {
				b.WriteString("every patch is unchanged: carry the record forward with a check on the new head\n")
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
