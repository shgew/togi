package main

import (
	"cmp"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strconv"
	"strings"
)

var (
	coverRangeLine    = regexp.MustCompile(`^(.+):(\d+)(?:-(\d+))?$`)
	coverNotBuiltLine = regexp.MustCompile(`^(.+): not built for (.+)$`)
)

// parseCover reads `just cover` output: `path:first-last` and `path:line` ranges, then `path: not built for <platform>` lines. Its summary sentences are ignored.
func parseCover(text string) (ranges []coverRange, unbuilt []notBuilt, err error) {
	for line := range strings.Lines(text) {
		line = strings.TrimSpace(line)
		if m := coverNotBuiltLine.FindStringSubmatch(line); m != nil {
			unbuilt = append(unbuilt, notBuilt{Path: m[1], Target: m[2]})
			continue
		}
		m := coverRangeLine.FindStringSubmatch(line)
		if m == nil {
			continue
		}
		first, _ := strconv.Atoi(m[2])
		last := first
		if m[3] != "" {
			last, _ = strconv.Atoi(m[3])
		}
		if last < first {
			return nil, nil, fmt.Errorf("cover range %q ends before it starts", line)
		}
		ranges = append(ranges, coverRange{Path: m[1], First: first, Last: last})
	}
	return ranges, unbuilt, nil
}

// intersectCover keeps the parts of the reported ranges that lie inside the hunks of their file, and the not-built files the review covers. A range outside every hunk belongs to a patch the record does not cover and is dropped.
func intersectCover(ranges []coverRange, unbuilt []notBuilt, hunks map[string][]span) coverage {
	c := coverage{Reported: len(ranges), Ranges: []coverRange{}, NotBuilt: []notBuilt{}}
	for _, r := range ranges {
		for _, h := range hunks[r.Path] {
			first, last := max(r.First, h.First), min(r.Last, h.Last)
			if first <= last {
				c.Ranges = append(c.Ranges, coverRange{r.Path, first, last})
			}
		}
	}
	for _, u := range unbuilt {
		if _, ok := hunks[u.Path]; ok {
			c.NotBuilt = append(c.NotBuilt, u)
		}
	}
	slices.SortFunc(c.Ranges, func(a, b coverRange) int {
		return cmp.Or(strings.Compare(a.Path, b.Path), cmp.Compare(a.First, b.First))
	})
	c.Ranges = slices.Compact(c.Ranges)
	return c
}

// hunkSpans reads the changed new-side lines of every included file from the snapshot's hunk files.
func hunkSpans(dir string, m manifest) (map[string][]span, error) {
	hunks := map[string][]span{}
	for _, f := range m.included() {
		b, err := os.ReadFile(filepath.Join(dir, f.HunkFile))
		if err != nil {
			return nil, fmt.Errorf("read hunk file for %s: %w", f.Path, err)
		}
		files, err := parseDiff(string(b))
		if err != nil {
			return nil, fmt.Errorf("parse hunk file for %s: %w", f.Path, err)
		}
		hunks[f.Path] = nil
		for _, s := range files {
			hunks[f.Path] = append(hunks[f.Path], s.Changed...)
		}
	}
	return hunks, nil
}

// addCover records the uncovered ranges inside the snapshot's hunks in its manifest, replacing earlier coverage.
func addCover(dir, text string) (manifest, error) {
	m, err := readManifest(dir)
	if err != nil {
		return manifest{}, err
	}
	ranges, unbuilt, err := parseCover(text)
	if err != nil {
		return manifest{}, err
	}
	hunks, err := hunkSpans(dir, m)
	if err != nil {
		return manifest{}, err
	}
	c := intersectCover(ranges, unbuilt, hunks)
	m.Coverage = &c
	return m, writeManifest(dir, m)
}
