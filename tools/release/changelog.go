package main

import (
	"fmt"
	"regexp"
	"strconv"
	"strings"
	"time"
)

var sectionHeading = regexp.MustCompile(`(?m)^## \[([^]\n]+)\](?: - (\d{4}-\d{2}-\d{2}))?[ \t]*$`)
var versionPattern = regexp.MustCompile(`^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$`)
var reference = regexp.MustCompile(`\[([^]\n]+)\](?:\[([^]\n]*)\])?`)
var definition = regexp.MustCompile(`(?m)^\[#([0-9]+)\]: .+$`)
var linkDefinition = regexp.MustCompile(`(?m)^\[[^]\n]+\]: \S.*$`)

type section struct {
	name, date, body string
	start, end       int
}

func sections(changelog string) []section {
	matches := sectionHeading.FindAllStringSubmatchIndex(changelog, -1)
	result := make([]section, 0, len(matches))
	for i, m := range matches {
		end := len(changelog)
		if i+1 < len(matches) {
			end = matches[i+1][0]
		}
		if i+1 == len(matches) {
			if at := linkDefinition.FindStringIndex(changelog[m[1]:end]); at != nil {
				end = m[1] + at[0]
			}
		}
		date := ""
		if m[4] >= 0 {
			date = changelog[m[4]:m[5]]
		}
		result = append(result, section{
			name: changelog[m[2]:m[3]], date: date,
			body: changelog[m[1]:end], start: m[0], end: end,
		})
	}
	return result
}

func sectionNamed(changelog, name string) (section, bool) {
	for _, s := range sections(changelog) {
		if s.name == name {
			return s, true
		}
	}
	return section{}, false
}

func entries(body string) []string {
	var lines []string
	for _, line := range strings.Split(body, "\n") {
		if strings.HasPrefix(line, "- ") {
			lines = append(lines, line)
		}
	}
	return lines
}

func bump(version, body string) (string, string, error) {
	matches := versionPattern.FindStringSubmatch(version)
	if matches == nil {
		return "", "", fmt.Errorf("invalid semantic version %q", version)
	}
	var numbers [3]int
	for i := range numbers {
		n, err := strconv.Atoi(matches[i+1])
		if err != nil {
			return "", "", fmt.Errorf("parse version %q: %w", version, err)
		}
		numbers[i] = n
	}
	var breaking []string
	for _, line := range entries(body) {
		if strings.HasPrefix(line, "- **BREAKING**") {
			breaking = append(breaking, line)
		}
	}
	if len(breaking) > 0 {
		if numbers[0] == 0 {
			numbers[1]++
			numbers[2] = 0
		} else {
			numbers[0]++
			numbers[1], numbers[2] = 0, 0
		}
		return fmt.Sprintf("%d.%d.%d", numbers[0], numbers[1], numbers[2]), "Breaking changes:\n" + strings.Join(breaking, "\n"), nil
	}
	if numbers[0] > 0 && addedEntries(body) {
		numbers[1]++
		numbers[2] = 0
		return fmt.Sprintf("%d.%d.%d", numbers[0], numbers[1], numbers[2]), "Added entries since the previous release (minor bump).", nil
	}
	numbers[2]++
	return fmt.Sprintf("%d.%d.%d", numbers[0], numbers[1], numbers[2]), "No breaking changes (patch bump).", nil
}

func addedEntries(body string) bool {
	inAdded := false
	for _, line := range strings.Split(body, "\n") {
		if strings.HasPrefix(line, "### ") {
			inAdded = line == "### Added"
		} else if inAdded && strings.HasPrefix(line, "- ") {
			return true
		}
	}
	return false
}

func releaseNotes(changelog string, s section) string {
	body := strings.TrimSpace(s.body)
	used := make(map[string]bool)
	for _, match := range reference.FindAllStringSubmatchIndex(body, -1) {
		if match[4] < 0 && match[1] < len(body) && body[match[1]] == '(' {
			continue
		}
		label := body[match[2]:match[3]]
		if match[4] >= 0 && match[4] != match[5] {
			label = body[match[4]:match[5]]
		}
		used[strings.ToLower(strings.Join(strings.Fields(label), " "))] = true
	}
	for _, match := range linkDefinition.FindAllStringSubmatchIndex(changelog, -1) {
		line := changelog[match[0]:match[1]]
		label := line[1:strings.IndexByte(line, ']')]
		key := strings.ToLower(strings.Join(strings.Fields(label), " "))
		if used[key] {
			body += "\n\n" + line
			delete(used, key)
		}
	}
	return body
}

func rewriteChangelog(changelog, version, webURL string, today time.Time) (string, error) {
	s, ok := sectionNamed(changelog, "Unreleased")
	if !ok {
		return "", fmt.Errorf("missing [Unreleased] section")
	}
	if _, exists := sectionNamed(changelog, version); exists {
		return "", fmt.Errorf("version [%s] already exists", version)
	}
	body := strings.TrimSpace(s.body)
	replacement := "## [Unreleased]\n\n## [" + version + "] - " + today.UTC().Format("2006-01-02") + "\n\n" + body + "\n\n"
	updated := changelog[:s.start] + replacement + changelog[s.end:]
	link := "[" + version + "]: " + strings.TrimRight(webURL, "/") + "/releases/tag/v" + version + "\n"
	if at := definition.FindStringIndex(updated); at != nil {
		updated = updated[:at[0]] + link + "\n" + updated[at[0]:]
	} else {
		updated = strings.TrimRight(updated, "\n") + "\n\n" + link
	}
	return updated, nil
}
