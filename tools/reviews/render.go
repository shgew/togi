package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
)

const (
	recordFile     = "record.md"
	recordJSONFile = "record.json"
	renderUsage    = "usage: reviews render <snapshot directory> <findings file>"
)

// recordModel is one review record. Its JSON form is the version 3 block of docs/review-record.md; markdown writes the same data for people. The unexported fields are the few things only the Markdown carries.
type recordModel struct {
	Version       int              `json:"version"`
	Repository    string           `json:"repository"`
	PullRequest   int              `json:"pull_request"`
	HeadSHA       string           `json:"head_sha"`
	BaseSHA       string           `json:"base_sha"`
	Previous      *recordPrevious  `json:"previous"`
	ExcludedFiles []recordExcluded `json:"excluded_files"`
	Files         []string         `json:"files"`
	Reviewers     []recordReviewer `json:"reviewers"`
	Findings      []recordFinding  `json:"findings"`
	Coverage      *recordCoverage  `json:"coverage"`
	Verdict       string           `json:"verdict"`

	blocker  string
	fallback string
}

type recordPrevious struct {
	URL     string `json:"url"`
	HeadSHA string `json:"head_sha"`
}

type recordExcluded struct {
	Path   string `json:"path"`
	Reason string `json:"reason"`
}

type recordReviewer struct {
	Name  string   `json:"name"`
	Model *string  `json:"model"`
	Files []string `json:"files"`
}

type recordFinding struct {
	Source   string  `json:"source"`
	Priority string  `json:"priority"`
	Finding  string  `json:"finding"`
	Location *string `json:"location"`
	URL      *string `json:"url"`
	Outcome  outcome `json:"outcome"`
}

type recordCoverage struct {
	Uncovered int    `json:"uncovered"`
	Judgment  string `json:"judgment"`
}

// findingsInput is the coordinator's judgment, the one input render takes besides the manifest.
type findingsInput struct {
	// Reviewers cover every included file; a coordinator that reviewed a small pull request lists itself.
	Reviewers []recordReviewer `json:"reviewers"`
	Findings  []recordFinding  `json:"findings"`
	// CoverageJudgment says how the uncovered ranges were judged; required when the diff changes Go code.
	CoverageJudgment string `json:"coverage_judgment"`
	// Blocked names a blocker the findings do not show, such as an unresolved review thread or an open finding from an earlier record.
	Blocked string `json:"blocked"`
}

var fullSHA = regexp.MustCompile(`^[0-9a-f]{40}$`)

func runRender(t tools, args []string) error {
	if len(args) != 2 {
		return errors.New(renderUsage)
	}
	dir := args[0]
	m, err := readManifest(dir)
	if err != nil {
		return err
	}
	raw, err := os.ReadFile(args[1])
	if err != nil {
		return fmt.Errorf("read findings: %w", err)
	}
	var in findingsInput
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.DisallowUnknownFields()
	if err := dec.Decode(&in); err != nil {
		return fmt.Errorf("decode findings %s: %w", args[1], err)
	}
	model, err := buildRecord(m, in)
	if err != nil {
		return err
	}
	block, err := model.jsonBlock()
	if err != nil {
		return err
	}
	md := model.markdown() + "\n" + recordOpen + block + recordClose + "\n"
	if err := os.WriteFile(filepath.Join(dir, recordFile), []byte(md), 0o644); err != nil {
		return fmt.Errorf("write %s: %w", recordFile, err)
	}
	if err := os.WriteFile(filepath.Join(dir, recordJSONFile), []byte(block+"\n"), 0o644); err != nil {
		return fmt.Errorf("write %s: %w", recordJSONFile, err)
	}
	_, err = fmt.Fprintf(t.stdout, "%s: %s on %s\nwrote %s and %s\n", filepath.Base(filepath.Clean(dir)), model.Verdict, model.HeadSHA, filepath.Join(dir, recordFile), filepath.Join(dir, recordJSONFile))
	return err
}

// buildRecord checks the coordinator's findings against the manifest and joins them into one record.
func buildRecord(m manifest, in findingsInput) (recordModel, error) {
	if m.CarryForward {
		return recordModel{}, errors.New("every patch is unchanged: carry the record forward with publish; there is no record to render")
	}
	r := recordModel{
		Version: 3, Repository: m.Repository, PullRequest: m.PullRequest, HeadSHA: m.HeadSHA, BaseSHA: m.BaseSHA,
		ExcludedFiles: []recordExcluded{}, Files: []string{}, Reviewers: []recordReviewer{}, Findings: []recordFinding{},
		fallback: m.Fallback,
	}
	if m.Previous != nil {
		r.Previous = &recordPrevious{URL: m.Previous.URL, HeadSHA: m.Previous.HeadSHA}
	}
	for _, f := range m.Files {
		if f.Exclusion != "" {
			r.ExcludedFiles = append(r.ExcludedFiles, recordExcluded{Path: f.Path, Reason: f.Exclusion})
			continue
		}
		r.Files = append(r.Files, f.Path)
	}
	if err := checkReviewers(r.Files, in.Reviewers); err != nil {
		return recordModel{}, err
	}
	for _, rv := range in.Reviewers {
		if rv.Files == nil {
			rv.Files = []string{}
		}
		r.Reviewers = append(r.Reviewers, rv)
	}
	var open []string
	for _, f := range in.Findings {
		if err := checkFinding(f); err != nil {
			return recordModel{}, err
		}
		if f.Outcome.Status == "deferred" && (f.Priority == "P0" || f.Priority == "P1") && !slices.Contains(open, f.Priority) {
			open = append(open, f.Priority)
		}
	}
	r.Findings = append(r.Findings, in.Findings...)
	switch {
	case m.changesGo():
		if m.Coverage == nil {
			return recordModel{}, errors.New("the diff changes Go code but the snapshot has no coverage: run `just cover <base>` and pass it to `snapshot --cover`")
		}
		if strings.TrimSpace(in.CoverageJudgment) == "" {
			return recordModel{}, errors.New("coverage_judgment is required: the diff changes Go code")
		}
		r.Coverage = &recordCoverage{Uncovered: len(m.Coverage.Ranges), Judgment: in.CoverageJudgment}
	case in.CoverageJudgment != "":
		return recordModel{}, errors.New("coverage_judgment given, but the diff changes no Go code")
	}
	slices.Sort(open)
	var reasons []string
	if len(open) > 0 {
		reasons = append(reasons, strings.Join(open, ", ")+" open")
	}
	if b := strings.TrimSuffix(strings.TrimSpace(in.Blocked), "."); b != "" {
		reasons = append(reasons, b)
	}
	r.Verdict, r.blocker = "success", strings.Join(reasons, "; ")
	if r.blocker != "" {
		r.Verdict = "blocked"
	}
	return r, nil
}

func checkReviewers(included []string, reviewers []recordReviewer) error {
	if len(included) == 0 {
		if len(reviewers) > 0 {
			return errors.New("no file is included, so the record has no reviewers")
		}
		return nil
	}
	if len(reviewers) == 0 {
		return errors.New("reviewers are required: the record covers files")
	}
	covered := map[string]bool{}
	for _, rv := range reviewers {
		if rv.Name == "" {
			return errors.New("a reviewer has no name")
		}
		for _, f := range rv.Files {
			if !slices.Contains(included, f) {
				return fmt.Errorf("reviewer %s covers %s, which the snapshot does not include", rv.Name, f)
			}
			covered[f] = true
		}
	}
	for _, f := range included {
		if !covered[f] {
			return fmt.Errorf("no reviewer covers %s", f)
		}
	}
	return nil
}

func checkFinding(f recordFinding) error {
	if f.Source == "" || f.Finding == "" {
		return errors.New("a finding needs a source and text")
	}
	if !slices.Contains(priorities[:], f.Priority) {
		return fmt.Errorf("finding from %s: unknown priority %q", f.Source, f.Priority)
	}
	o := f.Outcome
	switch {
	case o.Status == "fixed" && fullSHA.MatchString(o.SHA) && o.Reason == "" && o.Issue == 0:
	case o.Status == "rejected" && o.Reason != "" && o.SHA == "" && o.Issue == 0:
	case o.Status == "deferred" && o.Issue > 0 && o.SHA == "" && o.Reason == "":
	default:
		return fmt.Errorf("finding from %s: outcome must be fixed with a full sha, rejected with a reason, or deferred with an issue number", f.Source)
	}
	return nil
}

// jsonBlock is the record's compact one-line JSON; the encoder escapes <, > and & so finding text cannot end the HTML comment around it.
func (r recordModel) jsonBlock() (string, error) {
	b, err := json.Marshal(r)
	if err != nil {
		return "", fmt.Errorf("encode record: %w", err)
	}
	return string(b), nil
}

func (r recordModel) markdown() string {
	var b strings.Builder
	b.WriteString("## Review record\n\n")
	if r.blocker != "" {
		fmt.Fprintf(&b, "**%s** on %s: %s.\n\n", r.Verdict, r.HeadSHA, r.blocker)
	} else {
		fmt.Fprintf(&b, "**%s** on %s.\n\n", r.Verdict, r.HeadSHA)
	}
	if len(r.Findings) == 0 {
		b.WriteString("No findings.\n")
	} else {
		b.WriteString("| Source | Priority | Finding | Outcome |\n|---|---|---|---|\n")
		for _, f := range r.Findings {
			source := cell(f.Source)
			if f.URL != nil {
				source = "[" + source + "](" + *f.URL + ")"
			}
			text := cell(f.Finding)
			if f.Location != nil {
				text += " (`" + cell(*f.Location) + "`)"
			}
			fmt.Fprintf(&b, "| %s | %s | %s | %s |\n", source, f.Priority, text, cell(f.Outcome.text()))
		}
	}
	b.WriteString("\n<details>\n<summary>Review data</summary>\n\n")
	fmt.Fprintf(&b, "- Base: %s\n", r.BaseSHA)
	if r.Previous != nil {
		fmt.Fprintf(&b, "- Previous: %s, range %s..%s\n", r.Previous.URL, r.Previous.HeadSHA, r.HeadSHA)
	}
	if r.fallback != "" {
		fmt.Fprintf(&b, "- Fallback: the whole layer is covered because %s.\n", strings.TrimSuffix(r.fallback, "."))
	}
	fmt.Fprintf(&b, "- Files: %s\n", codeList(r.Files))
	if len(r.ExcludedFiles) > 0 {
		parts := make([]string, len(r.ExcludedFiles))
		for i, e := range r.ExcludedFiles {
			parts[i] = "`" + e.Path + "` (" + e.Reason + ")"
		}
		fmt.Fprintf(&b, "- Excluded: %s\n", strings.Join(parts, ", "))
	}
	parts := make([]string, len(r.Reviewers))
	for i, rv := range r.Reviewers {
		parts[i] = rv.Name
		if rv.Model != nil {
			parts[i] += " (" + *rv.Model + ")"
		}
		parts[i] += ": " + codeList(rv.Files)
	}
	fmt.Fprintf(&b, "- Reviewers: %s\n", orNone(strings.Join(parts, "; ")))
	if c := r.Coverage; c != nil {
		noun := "ranges"
		if c.Uncovered == 1 {
			noun = "range"
		}
		fmt.Fprintf(&b, "- Coverage: %d uncovered %s. %s\n", c.Uncovered, noun, c.Judgment)
	}
	b.WriteString("\n</details>\n")
	return b.String()
}

func (o outcome) text() string {
	switch o.Status {
	case "fixed":
		return "fixed in " + o.SHA
	case "rejected":
		return "rejected: " + o.Reason
	}
	return fmt.Sprintf("deferred: #%d", o.Issue)
}

// cell makes text safe for one table cell.
func cell(s string) string {
	return strings.ReplaceAll(strings.Join(strings.Fields(s), " "), "|", `\|`)
}

func codeList(items []string) string {
	parts := make([]string, len(items))
	for i, s := range items {
		parts[i] = "`" + s + "`"
	}
	return orNone(strings.Join(parts, ", "))
}

func orNone(s string) string {
	if s == "" {
		return "none"
	}
	return s
}
