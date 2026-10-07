// Command board prints the coordination board of the current repository: who may post on it, what waits on the owner and on the target machine, untriaged issues, work in progress, ready work and overlaps between them, and the open Ruleset issue.
package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"time"
)

const issuesQuery = `query($owner: String!, $name: String!, $endCursor: String) {
  repository(owner: $owner, name: $name) {
    nameWithOwner
    issues(states: OPEN, first: 100, after: $endCursor) {
      pageInfo { hasNextPage endCursor }
      nodes {
        number title body isPinned
        labels(first: 50) { nodes { name } }
        assignees(first: 20) { nodes { login } }
        parent { number title }
        blockedBy(first: 50) { nodes { number state } }
        linkedBranches(first: 20) { nodes { ref { name repository { nameWithOwner } } } }
        subIssuesSummary { total completed }
      }
    }
  }
}`

// closedHardwareQuery lists the closed issues that still carry needs-hardware: GitHub closes an issue when a pull request linked to it merges,
// before its run on the target machine, so the label, not the open state, marks the wait (ADR 0043).
const closedHardwareQuery = `query($owner: String!, $name: String!, $endCursor: String) {
  repository(owner: $owner, name: $name) {
    issues(states: CLOSED, labels: ["needs-hardware"], first: 100, after: $endCursor) {
      pageInfo { hasNextPage endCursor }
      nodes { number title assignees(first: 20) { nodes { login } } }
    }
  }
}`

const pullsQuery = `query($owner: String!, $name: String!, $endCursor: String) {
  repository(owner: $owner, name: $name) {
    pullRequests(states: OPEN, first: 100, after: $endCursor) {
      pageInfo { hasNextPage endCursor }
      nodes { number body headRefName headRepository { nameWithOwner } }
    }
  }
}`

// ghFunc runs gh with the arguments and returns its standard output.
type ghFunc func(args ...string) ([]byte, error)

func runGH(args ...string) ([]byte, error) {
	cmd := exec.Command("gh", args...)
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		return nil, fmt.Errorf("gh %s: %w: %s", args[0], err, strings.TrimSpace(stderr.String()))
	}
	return stdout.Bytes(), nil
}

func main() {
	if err := run(os.Stdout, runGH, time.Now()); err != nil {
		fmt.Fprintln(os.Stderr, "board:", err)
		os.Exit(1)
	}
}

func run(w io.Writer, gh ghFunc, now time.Time) error {
	repo, issues, err := fetchIssues(gh)
	if err != nil {
		return err
	}
	closedHardware, err := fetchClosedHardware(gh)
	if err != nil {
		return err
	}
	pulls, err := fetchPulls(gh)
	if err != nil {
		return err
	}
	limit, err := fetchLimit(gh)
	if err != nil {
		return err
	}
	return render(w, build(repo, issues, closedHardware, pulls, limit, now))
}

// query runs a paginated GraphQL query against the current repository and returns its pages.
func query(gh ghFunc, q string) ([]byte, error) {
	return gh("api", "graphql", "--paginate", "--slurp", "-F", "owner={owner}", "-F", "name={repo}", "-f", "query="+q)
}

// fetchLimit returns the current repository's interaction limit, which GitHub answers as {} when there is none.
func fetchLimit(gh ghFunc) (interactionLimit, error) {
	out, err := gh("api", "repos/{owner}/{repo}/interaction-limits")
	if err != nil {
		return interactionLimit{}, fmt.Errorf("fetch interaction limit: %w", err)
	}
	var l struct {
		Limit     string    `json:"limit"`
		ExpiresAt time.Time `json:"expires_at"`
	}
	if err := json.Unmarshal(out, &l); err != nil {
		return interactionLimit{}, fmt.Errorf("decode interaction limit: %w", err)
	}
	return interactionLimit{Limit: l.Limit, Expires: l.ExpiresAt}, nil
}

type nodes[T any] struct {
	Nodes []T `json:"nodes"`
}

type repository struct {
	NameWithOwner string `json:"nameWithOwner"`
}

type issueNode struct {
	Number    int                           `json:"number"`
	Title     string                        `json:"title"`
	Body      string                        `json:"body"`
	IsPinned  bool                          `json:"isPinned"`
	Labels    nodes[struct{ Name string }]  `json:"labels"`
	Assignees nodes[struct{ Login string }] `json:"assignees"`
	Parent    *parent                       `json:"parent"`
	BlockedBy nodes[struct {
		Number int
		State  string
	}] `json:"blockedBy"`
	LinkedBranches nodes[struct {
		Ref *struct {
			Name       string
			Repository repository
		}
	}] `json:"linkedBranches"`
	SubIssuesSummary struct {
		Total     int
		Completed int
	} `json:"subIssuesSummary"`
}

type pullNode struct {
	Number         int         `json:"number"`
	Body           string      `json:"body"`
	HeadRefName    string      `json:"headRefName"`
	HeadRepository *repository `json:"headRepository"`
}

// fetchIssues returns the owner/name of the current repository and its open issues.
func fetchIssues(gh ghFunc) (string, []issue, error) {
	out, err := query(gh, issuesQuery)
	if err != nil {
		return "", nil, fmt.Errorf("fetch issues: %w", err)
	}
	var pages []struct {
		Data struct {
			Repository struct {
				repository
				Issues nodes[issueNode] `json:"issues"`
			} `json:"repository"`
		} `json:"data"`
	}
	if err := json.Unmarshal(out, &pages); err != nil {
		return "", nil, fmt.Errorf("decode issues: %w", err)
	}
	var repo string
	var issues []issue
	for _, page := range pages {
		repo = page.Data.Repository.NameWithOwner
		for _, n := range page.Data.Repository.Issues.Nodes {
			i := issue{Number: n.Number, Title: n.Title, Body: n.Body, Pinned: n.IsPinned, Parent: n.Parent,
				SubIssues: n.SubIssuesSummary.Total, SubClosed: n.SubIssuesSummary.Completed}
			for _, l := range n.Labels.Nodes {
				i.Labels = append(i.Labels, l.Name)
			}
			for _, a := range n.Assignees.Nodes {
				i.Assignees = append(i.Assignees, a.Login)
			}
			for _, b := range n.BlockedBy.Nodes {
				if b.State == "OPEN" {
					i.OpenBlockers = append(i.OpenBlockers, b.Number)
				}
			}
			for _, b := range n.LinkedBranches.Nodes {
				if b.Ref != nil {
					i.Branches = append(i.Branches, branch{Repo: b.Ref.Repository.NameWithOwner, Name: b.Ref.Name})
				}
			}
			issues = append(issues, i)
		}
	}
	return repo, issues, nil
}

// fetchClosedHardware returns the closed issues labeled needs-hardware.
func fetchClosedHardware(gh ghFunc) ([]issue, error) {
	out, err := query(gh, closedHardwareQuery)
	if err != nil {
		return nil, fmt.Errorf("fetch closed needs-hardware issues: %w", err)
	}
	var pages []struct {
		Data struct {
			Repository struct {
				Issues nodes[struct {
					Number    int                           `json:"number"`
					Title     string                        `json:"title"`
					Assignees nodes[struct{ Login string }] `json:"assignees"`
				}] `json:"issues"`
			} `json:"repository"`
		} `json:"data"`
	}
	if err := json.Unmarshal(out, &pages); err != nil {
		return nil, fmt.Errorf("decode closed needs-hardware issues: %w", err)
	}
	var issues []issue
	for _, page := range pages {
		for _, n := range page.Data.Repository.Issues.Nodes {
			i := issue{Number: n.Number, Title: n.Title, Closed: true, Labels: []string{"needs-hardware"}}
			for _, a := range n.Assignees.Nodes {
				i.Assignees = append(i.Assignees, a.Login)
			}
			issues = append(issues, i)
		}
	}
	return issues, nil
}

func fetchPulls(gh ghFunc) ([]pull, error) {
	out, err := query(gh, pullsQuery)
	if err != nil {
		return nil, fmt.Errorf("fetch pull requests: %w", err)
	}
	var pages []struct {
		Data struct {
			Repository struct {
				PullRequests nodes[pullNode] `json:"pullRequests"`
			} `json:"repository"`
		} `json:"data"`
	}
	if err := json.Unmarshal(out, &pages); err != nil {
		return nil, fmt.Errorf("decode pull requests: %w", err)
	}
	var pulls []pull
	for _, page := range pages {
		for _, n := range page.Data.Repository.PullRequests.Nodes {
			p := pull{Number: n.Number, Body: n.Body, Head: branch{Name: n.HeadRefName}}
			if n.HeadRepository != nil {
				p.Head.Repo = n.HeadRepository.NameWithOwner
			}
			pulls = append(pulls, p)
		}
	}
	return pulls, nil
}
