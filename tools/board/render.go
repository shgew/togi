package main

import (
	"fmt"
	"io"
	"strings"
)

func render(w io.Writer, b board) error {
	p := &printer{w: w}
	p.section("Waiting on you (needs-decision)", len(b.Waiting))
	for _, i := range b.Waiting {
		p.line("  %s%s", head(i), assigned(i))
	}
	p.section("Untriaged (needs-triage)", len(b.Untriaged))
	for _, i := range b.Untriaged {
		p.line("  %s", head(i))
	}
	p.section("In progress", len(b.InProgress))
	for _, i := range b.InProgress {
		p.line("  %s%s", head(i.issue), assigned(i.issue))
		if i.Parent != nil {
			p.line("    block: #%d %s", i.Parent.Number, i.Parent.Title)
		}
		p.line("    branches: %s", list(i.Work))
		p.line("    pull requests: %s", list(refs(i.Pulls)))
		p.line("    touches: %s", list(i.Touches))
	}
	p.section("Ready", len(b.Ready))
	priority := ""
	for _, g := range b.Ready {
		if g.Priority != priority {
			priority = g.Priority
			p.line("  %s", priority)
		}
		if g.Parent != nil {
			p.line("    #%d %s", g.Parent.Number, g.Parent.Title)
		} else {
			p.line("    no block")
		}
		for _, i := range g.Issues {
			p.line("      %s", head(i.issue))
			p.line("        touches: %s", list(i.Touches))
		}
	}
	p.section("Overlaps", len(b.Overlaps))
	for _, o := range b.Overlaps {
		p.line("  #%d overlaps in-progress #%d on %s", o.Ready.Number, o.Progress.Number, strings.Join(o.Paths, ", "))
	}
	p.section("Ruleset", len(b.Rulesets))
	for _, i := range b.Rulesets {
		pinned := "pinned"
		if !i.Pinned {
			pinned = "not pinned"
		}
		p.line("  %s (%s, %d of %d sub-issues closed)", head(i), pinned, i.SubClosed, i.SubIssues)
	}
	return p.err
}

type printer struct {
	w       io.Writer
	err     error
	started bool
}

func (p *printer) line(format string, args ...any) {
	if p.err == nil {
		_, p.err = fmt.Fprintf(p.w, format+"\n", args...)
	}
}

func (p *printer) section(title string, n int) {
	if p.started {
		p.line("")
	}
	p.started = true
	p.line("%s", title)
	if n == 0 {
		p.line("  none")
	}
}

func head(i issue) string { return fmt.Sprintf("#%d %s", i.Number, i.Title) }

func assigned(i issue) string {
	if len(i.Assignees) == 0 {
		return ""
	}
	return " (@" + strings.Join(i.Assignees, ", @") + ")"
}

func refs(numbers []int) []string {
	out := make([]string, len(numbers))
	for k, n := range numbers {
		out[k] = fmt.Sprintf("#%d", n)
	}
	return out
}

func list(items []string) string {
	if len(items) == 0 {
		return "none"
	}
	return strings.Join(items, ", ")
}
