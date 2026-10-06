// Command reviews prints the recorded agent review's track record over the current repository's merged pull requests: robotogi's review check on each head, findings by priority and outcome from the review records, and the time from opening to the first record.
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

const robotogiLogin = "robotogi"

// pullsQuery filters check suites by robotogi's App ID.
const pullsQuery = `query($owner: String!, $name: String!, $endCursor: String) {
  repository(owner: $owner, name: $name) {
    pullRequests(states: MERGED, first: 50, after: $endCursor) {
      pageInfo { hasNextPage endCursor }
      nodes {
        number title createdAt
        commits(last: 1) { nodes { commit {
          checkSuites(first: 10, filterBy: {appId: 5162510}) { nodes {
            checkRuns(first: 10, filterBy: {checkName: "review"}) { nodes { conclusion } }
          } }
        } } }
        comments(first: 100) {
          pageInfo { hasNextPage }
          nodes { author { login } createdAt url body }
        }
      }
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
	if err := run(os.Stdout, runGH); err != nil {
		fmt.Fprintln(os.Stderr, "reviews:", err)
		os.Exit(1)
	}
}

func run(w io.Writer, gh ghFunc) error {
	pulls, err := fetchPulls(gh)
	if err != nil {
		return err
	}
	rows, err := summarize(pulls)
	if err != nil {
		return err
	}
	return render(w, rows, total(rows))
}

type nodes[T any] struct {
	Nodes []T `json:"nodes"`
}

type pullNode struct {
	Number    int       `json:"number"`
	Title     string    `json:"title"`
	CreatedAt time.Time `json:"createdAt"`
	Commits   nodes[struct {
		Commit struct {
			CheckSuites nodes[struct {
				CheckRuns nodes[struct {
					Conclusion string `json:"conclusion"`
				}] `json:"checkRuns"`
			}] `json:"checkSuites"`
		} `json:"commit"`
	}] `json:"commits"`
	Comments struct {
		PageInfo struct {
			HasNextPage bool `json:"hasNextPage"`
		} `json:"pageInfo"`
		Nodes []struct {
			Author *struct {
				Login string `json:"login"`
			} `json:"author"`
			CreatedAt time.Time `json:"createdAt"`
			URL       string    `json:"url"`
			Body      string    `json:"body"`
		} `json:"nodes"`
	} `json:"comments"`
}

func fetchPulls(gh ghFunc) ([]pull, error) {
	out, err := gh("api", "graphql", "--paginate", "--slurp", "-F", "owner={owner}", "-F", "name={repo}", "-f", "query="+pullsQuery)
	if err != nil {
		return nil, fmt.Errorf("fetch merged pull requests: %w", err)
	}
	var pages []struct {
		Data struct {
			Repository struct {
				PullRequests nodes[pullNode] `json:"pullRequests"`
			} `json:"repository"`
		} `json:"data"`
	}
	if err := json.Unmarshal(out, &pages); err != nil {
		return nil, fmt.Errorf("decode merged pull requests: %w", err)
	}
	var pulls []pull
	for _, page := range pages {
		for _, n := range page.Data.Repository.PullRequests.Nodes {
			if n.Comments.PageInfo.HasNextPage {
				return nil, fmt.Errorf("pull request #%d has more than 100 comments", n.Number)
			}
			p := pull{Number: n.Number, Title: n.Title, Opened: n.CreatedAt, Check: headCheck(n)}
			for _, c := range n.Comments.Nodes {
				if c.Author != nil && c.Author.Login == robotogiLogin {
					p.Comments = append(p.Comments, comment{URL: c.URL, Created: c.CreatedAt, Body: c.Body})
				}
			}
			pulls = append(pulls, p)
		}
	}
	return pulls, nil
}

// headCheck returns "success" when any robotogi review check on the head succeeded, else the first one's conclusion, lower-cased.
func headCheck(n pullNode) string {
	var first string
	for _, c := range n.Commits.Nodes {
		for _, s := range c.Commit.CheckSuites.Nodes {
			for _, r := range s.CheckRuns.Nodes {
				conclusion := strings.ToLower(r.Conclusion)
				if conclusion == "" {
					conclusion = "pending"
				}
				if conclusion == "success" {
					return conclusion
				}
				if first == "" {
					first = conclusion
				}
			}
		}
	}
	return first
}
