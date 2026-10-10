package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
)

const manifestVersion = 1

// manifest is manifest.json: everything a coordinator needs from the frozen pull request, and everything render reads from it.
type manifest struct {
	Version     int    `json:"version"`
	Repository  string `json:"repository"`
	PullRequest int    `json:"pull_request"`
	HeadSHA     string `json:"head_sha"`
	BaseSHA     string `json:"base_sha"`
	// Previous is the record this one follows; null in a first record.
	Previous *previousRecord `json:"previous"`
	// Fallback says why a re-review covers the whole layer instead of a delta.
	Fallback string `json:"fallback,omitempty"`
	// CarryForward is true when every patch is unchanged: the record carries forward and the new head needs only a check.
	CarryForward bool `json:"carry_forward"`
	// Files are the files of this record's diff: the pull request's, or the delta's on a re-review.
	Files []manifestFile `json:"files"`
	// IncludedFiles and IncludedLines are F and L of the sizing rule, over the included files.
	IncludedFiles int `json:"included_files"`
	IncludedLines int `json:"included_lines"`
	Reviewers     int `json:"reviewers"`
	// Small applies the small-pull-request rule to the pull request's whole diff.
	Small   bool          `json:"small"`
	PRFiles int           `json:"pr_files"`
	PRLines int           `json:"pr_lines"`
	Patches []patchStatus `json:"patches,omitempty"`
	// Coverage is null until `just cover` output arrives.
	Coverage *coverage `json:"coverage"`
}

type previousRecord struct {
	URL     string `json:"url"`
	HeadSHA string `json:"head_sha"`
	BaseSHA string `json:"base_sha,omitempty"`
}

type manifestFile struct {
	Path    string `json:"path"`
	OldPath string `json:"old_path,omitempty"`
	Status  string `json:"status"`
	Added   int    `json:"added"`
	Removed int    `json:"removed"`
	Binary  bool   `json:"binary,omitempty"`
	// Exclusion is the reason the review skips the file; empty for an included file.
	Exclusion string `json:"exclusion,omitempty"`
	// HunkFile is the included file's hunk file, relative to the snapshot directory.
	HunkFile string `json:"hunk_file,omitempty"`
}

// patchStatus classifies one patch of a re-review's range-diff.
type patchStatus struct {
	Old     string `json:"old,omitempty"`
	New     string `json:"new,omitempty"`
	Subject string `json:"subject"`
	// Status is unchanged, changed, added or removed.
	Status string `json:"status"`
	// ContextOnly marks an unchanged patch that range-diff showed as changed because only its context lines differ.
	ContextOnly bool `json:"context_only,omitempty"`
}

type coverage struct {
	// Reported counts the ranges in the input, before dropping those outside the hunks.
	Reported int          `json:"reported"`
	Ranges   []coverRange `json:"ranges"`
	NotBuilt []notBuilt   `json:"not_built"`
}

type coverRange struct {
	Path  string `json:"path"`
	First int    `json:"first"`
	Last  int    `json:"last"`
}

type notBuilt struct {
	Path   string `json:"path"`
	Target string `json:"target"`
}

// included returns the files the review covers.
func (m manifest) included() []manifestFile {
	var out []manifestFile
	for _, f := range m.Files {
		if f.Exclusion == "" {
			out = append(out, f)
		}
	}
	return out
}

func (m manifest) changesGo() bool {
	return slices.ContainsFunc(m.included(), func(f manifestFile) bool { return strings.HasSuffix(f.Path, ".go") })
}

// reviewerCount applies the sizing rule of .omp/commands/review-pr.md to the included files: l lines added plus removed over f files.
func reviewerCount(l, f int) int {
	switch {
	case f == 0:
		return 0
	case l < 100 || f <= 2:
		return 1
	case l < 500:
		return min(2, f)
	case l < 2000:
		return min(4, ceilDiv(f, 3))
	case l < 5000:
		return min(8, ceilDiv(f, 2))
	}
	return min(16, f)
}

func ceilDiv(a, b int) int { return (a + b - 1) / b }

var smallPaths = []string{"internal/smu", "internal/trial", "internal/session", "internal/journal", "nix"}

// isSmall applies the small-pull-request rule to the whole diff: at most 2 files and under 100 changed lines, every file counted, none under a protected path.
func isSmall(files []fileDiff) bool {
	lines := 0
	for _, f := range files {
		lines += f.Added + f.Removed
		for _, p := range append([]string{f.Path}, f.OldPath) {
			if slices.ContainsFunc(smallPaths, func(dir string) bool { return strings.HasPrefix(p, dir+"/") }) {
				return false
			}
		}
	}
	return len(files) <= 2 && lines < 100
}

func totalLines(files []fileDiff) int {
	n := 0
	for _, f := range files {
		n += f.Added + f.Removed
	}
	return n
}

func hunkFileName(index int) string { return fmt.Sprintf("files/%03d.diff", index) }

func readManifest(dir string) (manifest, error) {
	b, err := os.ReadFile(filepath.Join(dir, "manifest.json"))
	if err != nil {
		return manifest{}, fmt.Errorf("read manifest: %w", err)
	}
	var m manifest
	if err := json.Unmarshal(b, &m); err != nil {
		return manifest{}, fmt.Errorf("decode manifest in %s: %w", dir, err)
	}
	if m.Version != manifestVersion {
		return manifest{}, fmt.Errorf("manifest in %s has version %d; this tool reads version %d", dir, m.Version, manifestVersion)
	}
	return m, nil
}

func writeManifest(dir string, m manifest) error {
	b, err := json.MarshalIndent(m, "", "  ")
	if err != nil {
		return fmt.Errorf("encode manifest: %w", err)
	}
	if err := os.WriteFile(filepath.Join(dir, "manifest.json"), append(b, '\n'), 0o644); err != nil {
		return fmt.Errorf("write manifest: %w", err)
	}
	return nil
}
