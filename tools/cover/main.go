// cover prints the lines changed since a base that no test reached, from a coverage profile and git diff.
package main

import (
	"bufio"
	"bytes"
	"flag"
	"fmt"
	"io"
	"maps"
	"os"
	"os/exec"
	"slices"
	"strconv"
	"strings"
)

type lineRange struct{ first, last int }

type gap struct {
	path string
	lineRange
}

func (g gap) String() string {
	if g.first == g.last {
		return fmt.Sprintf("%s:%d", g.path, g.first)
	}
	return fmt.Sprintf("%s:%d-%d", g.path, g.first, g.last)
}

func main() {
	if err := run(os.Args[1:], os.Stdout, os.Stderr); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(args []string, out, errOut io.Writer) error {
	flags := flag.NewFlagSet("cover", flag.ContinueOnError)
	flags.SetOutput(errOut)
	profilePath := flags.String("profile", "", "coverage profile written by go test -coverprofile")
	base := flags.String("base", "", "revision whose merge base with HEAD the working tree is compared against")
	if err := flags.Parse(args); err != nil {
		return err
	}
	if *profilePath == "" || *base == "" || flags.NArg() != 0 {
		return fmt.Errorf("cover: supply --profile and --base and no positional arguments")
	}
	module, err := modulePath("go.mod")
	if err != nil {
		return err
	}
	profile, err := os.Open(*profilePath)
	if err != nil {
		return fmt.Errorf("cover: open profile: %w", err)
	}
	defer profile.Close()
	diff, err := exec.Command("git", "diff", "-U0", "--inter-hunk-context=0", "--no-color", "--no-ext-diff", "--no-prefix", "--merge-base", *base, "--", "*.go").Output()
	if err != nil {
		return fmt.Errorf("cover: diff against %s: %w", *base, err)
	}
	changed, err := parseDiff(bytes.NewReader(diff))
	if err != nil {
		return err
	}
	if len(changed) == 0 {
		_, err := fmt.Fprintf(out, "No Go lines changed since %s.\n", *base)
		return err
	}
	uncovered, err := parseProfile(profile, module)
	if err != nil {
		return err
	}
	gaps := intersect(changed, uncovered)
	if len(gaps) == 0 {
		_, err := fmt.Fprintf(out, "Every changed Go line the profile measures ran in a test since %s.\n", *base)
		return err
	}
	for _, g := range gaps {
		if _, err := fmt.Fprintln(out, g); err != nil {
			return err
		}
	}
	return nil
}

func modulePath(goMod string) (string, error) {
	data, err := os.ReadFile(goMod)
	if err != nil {
		return "", fmt.Errorf("cover: read module path: %w", err)
	}
	for line := range strings.Lines(string(data)) {
		if path, ok := strings.CutPrefix(strings.TrimSpace(line), "module "); ok {
			return strings.TrimSpace(path), nil
		}
	}
	return "", fmt.Errorf("cover: %s declares no module", goMod)
}

// parseDiff returns the new-side lines each file gained or changed in a git diff -U0 --no-prefix.
func parseDiff(r io.Reader) (map[string][]lineRange, error) {
	changed := map[string][]lineRange{}
	var path string
	header := false
	s := bufio.NewScanner(r)
	s.Buffer(nil, 16<<20)
	for s.Scan() {
		line := s.Text()
		switch {
		case strings.HasPrefix(line, "diff --git "):
			header, path = true, ""
		case header && strings.HasPrefix(line, "+++ "):
			path = strings.TrimPrefix(line, "+++ ")
			if path == "/dev/null" {
				path = ""
			}
		case strings.HasPrefix(line, "@@ "):
			header = false
			if path == "" {
				continue
			}
			lines, err := parseHunk(line)
			if err != nil {
				return nil, err
			}
			if lines.last >= lines.first {
				changed[path] = append(changed[path], lines)
			}
		}
	}
	if err := s.Err(); err != nil {
		return nil, fmt.Errorf("cover: read diff: %w", err)
	}
	return changed, nil
}

func parseHunk(line string) (lineRange, error) {
	fields := strings.Fields(line)
	if len(fields) < 3 || !strings.HasPrefix(fields[2], "+") {
		return lineRange{}, fmt.Errorf("cover: malformed hunk header %q", line)
	}
	start, count, hasCount := strings.Cut(fields[2][1:], ",")
	first, err := strconv.Atoi(start)
	if err != nil {
		return lineRange{}, fmt.Errorf("cover: malformed hunk header %q: %w", line, err)
	}
	n := 1
	if hasCount {
		if n, err = strconv.Atoi(count); err != nil {
			return lineRange{}, fmt.Errorf("cover: malformed hunk header %q: %w", line, err)
		}
	}
	return lineRange{first, first + n - 1}, nil
}

// parseProfile returns the lines of each block no test reached, by repository-relative path.
// A block listed more than once counts as reached when any listing reached it.
func parseProfile(r io.Reader, module string) (map[string][]lineRange, error) {
	type block struct {
		path  string
		lines lineRange
		span  string
	}
	reached := map[block]bool{}
	s := bufio.NewScanner(r)
	if !s.Scan() || !strings.HasPrefix(s.Text(), "mode: ") {
		return nil, fmt.Errorf("cover: profile does not start with a mode line")
	}
	for s.Scan() {
		line := s.Text()
		fields := strings.Fields(line)
		if len(fields) != 3 {
			return nil, fmt.Errorf("cover: malformed profile line %q", line)
		}
		sep := strings.LastIndexByte(fields[0], ':')
		if sep < 0 {
			return nil, fmt.Errorf("cover: malformed profile line %q", line)
		}
		path, ok := strings.CutPrefix(fields[0][:sep], module+"/")
		if !ok {
			return nil, fmt.Errorf("cover: profile names %s outside module %s", fields[0][:sep], module)
		}
		span := fields[0][sep+1:]
		lines, err := parseSpan(span)
		if err != nil {
			return nil, fmt.Errorf("cover: malformed profile line %q: %w", line, err)
		}
		count, err := strconv.ParseUint(fields[2], 10, 64)
		if err != nil {
			return nil, fmt.Errorf("cover: malformed profile line %q: %w", line, err)
		}
		b := block{path, lines, span}
		reached[b] = reached[b] || count > 0
	}
	if err := s.Err(); err != nil {
		return nil, fmt.Errorf("cover: read profile: %w", err)
	}
	uncovered := map[string][]lineRange{}
	for b, ok := range reached {
		if !ok {
			uncovered[b.path] = append(uncovered[b.path], b.lines)
		}
	}
	return uncovered, nil
}

func parseSpan(span string) (lineRange, error) {
	start, end, ok := strings.Cut(span, ",")
	if !ok {
		return lineRange{}, fmt.Errorf("no end position in %q", span)
	}
	first, err := strconv.Atoi(strings.SplitN(start, ".", 2)[0])
	if err != nil {
		return lineRange{}, err
	}
	last, err := strconv.Atoi(strings.SplitN(end, ".", 2)[0])
	if err != nil {
		return lineRange{}, err
	}
	return lineRange{first, last}, nil
}

// intersect returns the changed lines inside uncovered blocks, merged into ranges and sorted by path and line.
func intersect(changed, uncovered map[string][]lineRange) []gap {
	var gaps []gap
	for _, path := range slices.Sorted(maps.Keys(changed)) {
		var hits []lineRange
		for _, block := range uncovered[path] {
			for _, c := range changed[path] {
				if first, last := max(block.first, c.first), min(block.last, c.last); first <= last {
					hits = append(hits, lineRange{first, last})
				}
			}
		}
		slices.SortFunc(hits, func(a, b lineRange) int { return a.first - b.first })
		for _, h := range hits {
			if n := len(gaps); n > 0 && gaps[n-1].path == path && h.first <= gaps[n-1].last+1 {
				gaps[n-1].last = max(gaps[n-1].last, h.last)
				continue
			}
			gaps = append(gaps, gap{path, h})
		}
	}
	return gaps
}
