package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

const publishUsage = "usage: just as-bot go run ./tools/reviews publish <snapshot directory>"

// robotogiBotLogin is the exact REST login of the robotogi App's comments; a prefix would also match ordinary users such as robotogi-helper.
const robotogiBotLogin = robotogiLogin + "[bot]"

type checkPayload struct {
	Name       string      `json:"name"`
	HeadSHA    string      `json:"head_sha"`
	Status     string      `json:"status"`
	Conclusion string      `json:"conclusion"`
	Output     checkOutput `json:"output"`
}

type checkOutput struct {
	Title   string `json:"title"`
	Summary string `json:"summary"`
}

func runPublish(t tools, args []string) error {
	if len(args) != 1 {
		return errors.New(publishUsage)
	}
	dir := args[0]
	m, err := readManifest(dir)
	if err != nil {
		return err
	}
	if t.getenv("GH_TOKEN") == "" {
		return errors.New("GH_TOKEN is not set: run publish under `just as-bot`, so one robotogi App token posts both the record and the check")
	}
	if err := requireHead(t, m); err != nil {
		return err
	}
	check := checkPayload{Name: "review", HeadSHA: m.HeadSHA, Status: "completed", Conclusion: "success"}
	var recordURL string
	switch {
	case m.CarryForward:
		if m.Previous == nil || m.Previous.Verdict != "success" {
			return errors.New("the snapshot carries forward a previous record that is not recorded as a success: render and publish a new record")
		}
		recordURL = m.Previous.URL
		check.Output = checkOutput{Title: "Review carried forward", Summary: carrySummary(m)}
	default:
		md, err := os.ReadFile(filepath.Join(dir, recordFile))
		if err != nil {
			return fmt.Errorf("read %s: run render first: %w", recordFile, err)
		}
		rec, err := parseRecord(string(md))
		if err != nil {
			return fmt.Errorf("read %s: %w", recordFile, err)
		}
		if rec.HeadSHA != m.HeadSHA {
			return fmt.Errorf("%s reviews %s, not the snapshot's head %s: run render again", recordFile, rec.HeadSHA, m.HeadSHA)
		}
		existing, err := existingRecord(t, m)
		if err != nil {
			return err
		}
		recordURL = existing.URL
		if existing.URL != "" && normalizeRecord(existing.Body) != normalizeRecord(string(md)) {
			return fmt.Errorf("robotogi's record %s on %s differs from %s: it is not updated and no check is posted; post the changed review on a new head", existing.URL, m.HeadSHA, recordFile)
		}
		if recordURL == "" {
			if recordURL, err = postRecord(t, m, filepath.Join(dir, recordFile)); err != nil {
				return err
			}
		}
		if rec.Verdict != "success" {
			fmt.Fprintf(t.stdout, "record %s\nthe verdict is %s: no review check posted\n", recordURL, rec.Verdict)
			return nil
		}
		check.Output = checkOutput{Title: "Review recorded", Summary: "Review record: " + recordURL}
	}
	checkURL, err := postCheck(t, m, dir, check)
	if err != nil {
		return err
	}
	fmt.Fprintf(t.stdout, "record %s\ncheck %s\n", recordURL, checkURL)
	if err := requireHead(t, m); err != nil {
		fmt.Fprintf(t.stderr, "warning: published, but %v; the new head needs its own review\n", err)
	}
	return nil
}

// requireHead refuses when the pull request's head is no longer the snapshot's.
func requireHead(t tools, m manifest) error {
	p, err := viewPull(t.gh, m.PullRequest)
	if err != nil {
		return err
	}
	if p.Head != m.HeadSHA {
		return fmt.Errorf("pull request #%d head is %s, not the snapshot's %s", m.PullRequest, p.Head, m.HeadSHA)
	}
	return nil
}

func carrySummary(m manifest) string {
	p := m.Previous
	context := 0
	for _, s := range m.Patches {
		if s.ContextOnly {
			context++
		}
	}
	oldBase := p.BaseSHA
	return fmt.Sprintf("Review record: %s carried forward. git range-diff %s..%s %s..%s pairs all %d patches unchanged, %d of them with only context lines changed.",
		p.URL, oldBase, p.HeadSHA, m.BaseSHA, m.HeadSHA, len(m.Patches), context)
}

// existingComment is a record comment already on the pull request.
type existingComment struct {
	URL  string
	Body string
}

// normalizeRecord makes a comment body comparable with the rendered file: GitHub may store CRLF and trim trailing whitespace.
func normalizeRecord(s string) string {
	return strings.TrimSpace(strings.ReplaceAll(s, "\r\n", "\n"))
}

// existingRecord returns robotogi's last record comment for the snapshot's head, or the zero value when there is none.
func existingRecord(t tools, m manifest) (existingComment, error) {
	out, err := t.gh("api", "--paginate", "--slurp", "repos/"+m.Repository+"/issues/"+strconv.Itoa(m.PullRequest)+"/comments")
	if err != nil {
		return existingComment{}, fmt.Errorf("list comments of pull request #%d: %w", m.PullRequest, err)
	}
	var pages [][]struct {
		URL  string `json:"html_url"`
		Body string `json:"body"`
		User struct {
			Login string `json:"login"`
		} `json:"user"`
	}
	if err := json.Unmarshal(out, &pages); err != nil {
		return existingComment{}, fmt.Errorf("decode comments of pull request #%d: %w", m.PullRequest, err)
	}
	var found existingComment
	for _, page := range pages {
		for _, c := range page {
			if c.User.Login != robotogiBotLogin {
				continue
			}
			if r, err := parseRecord(c.Body); err == nil && r.HeadSHA == m.HeadSHA {
				found = existingComment{URL: c.URL, Body: c.Body}
			}
		}
	}
	return found, nil
}

func postRecord(t tools, m manifest, file string) (string, error) {
	out, err := t.gh("api", "--method", "POST", "repos/"+m.Repository+"/issues/"+strconv.Itoa(m.PullRequest)+"/comments", "-F", "body=@"+file)
	if err != nil {
		return "", fmt.Errorf("post the record: %w", err)
	}
	var c struct {
		URL string `json:"html_url"`
	}
	if err := json.Unmarshal(out, &c); err != nil || c.URL == "" {
		return "", fmt.Errorf("the posted record's response names no URL: %s", strings.TrimSpace(string(out)))
	}
	return c.URL, nil
}

// postCheck posts the review check and verifies the returned run: robotogi's, on the reviewed head, linking the record.
func postCheck(t tools, m manifest, dir string, check checkPayload) (string, error) {
	b, err := json.Marshal(check)
	if err != nil {
		return "", fmt.Errorf("encode check: %w", err)
	}
	file := filepath.Join(dir, "check.json")
	if err := os.WriteFile(file, b, 0o644); err != nil {
		return "", fmt.Errorf("write check.json: %w", err)
	}
	out, err := t.gh("api", "--method", "POST", "repos/"+m.Repository+"/check-runs", "--input", file)
	if err != nil {
		return "", fmt.Errorf("post the review check: %w", err)
	}
	var run struct {
		URL     string `json:"html_url"`
		HeadSHA string `json:"head_sha"`
		App     struct {
			Slug string `json:"slug"`
		} `json:"app"`
		Output checkOutput `json:"output"`
	}
	if err := json.Unmarshal(out, &run); err != nil {
		return "", fmt.Errorf("decode the review check: %w", err)
	}
	switch {
	case run.App.Slug != robotogiLogin:
		return "", fmt.Errorf("the review check at %s belongs to app %q, not %s", run.URL, run.App.Slug, robotogiLogin)
	case run.HeadSHA != m.HeadSHA:
		return "", fmt.Errorf("the review check at %s is on %s, not the reviewed %s", run.URL, run.HeadSHA, m.HeadSHA)
	case run.Output.Summary != check.Output.Summary:
		return "", fmt.Errorf("the review check at %s does not carry the record link", run.URL)
	}
	return run.URL, nil
}
