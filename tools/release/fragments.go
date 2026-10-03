package main

import (
	"cmp"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strconv"
	"strings"
)

const (
	fragmentDir    = "changes"
	fragmentReadme = "README.md"
)

var fragmentName = regexp.MustCompile(`^([1-9][0-9]*)\.md$`)

var fragmentSections = []string{"Added", "Changed", "Removed", "Fixed"}

type fragmentEntry struct {
	pr            int
	section, text string
}

func (e fragmentEntry) breaking() bool {
	return strings.HasPrefix(e.text, "**BREAKING**")
}

func parseFragment(name, content string) ([]fragmentEntry, error) {
	m := fragmentName.FindStringSubmatch(name)
	if m == nil {
		return nil, fmt.Errorf("%s: name must be the pull request number, as in 123.md", name)
	}
	pr, err := strconv.Atoi(m[1])
	if err != nil {
		return nil, fmt.Errorf("%s: parse pull request number: %w", name, err)
	}
	var result []fragmentEntry
	section, sectionEntries := "", 0
	closeSection := func(line int) error {
		if section != "" && sectionEntries == 0 {
			return fmt.Errorf("%s:%d: ### %s has no entries", name, line, section)
		}
		return nil
	}
	lines := strings.Split(content, "\n")
	for i, line := range lines {
		n := i + 1
		switch {
		case strings.TrimSpace(line) == "":
		case strings.HasPrefix(line, "### "):
			if err := closeSection(n); err != nil {
				return nil, err
			}
			section, sectionEntries = strings.TrimPrefix(line, "### "), 0
			if !slices.Contains(fragmentSections, section) {
				return nil, fmt.Errorf("%s:%d: section %q is not one of %s", name, n, section, strings.Join(fragmentSections, ", "))
			}
		case strings.HasPrefix(line, "- "):
			text := strings.TrimSpace(strings.TrimPrefix(line, "- "))
			switch {
			case section == "":
				return nil, fmt.Errorf("%s:%d: entry before any ### section heading", name, n)
			case !strings.HasSuffix(text, ".") || text == ".":
				return nil, fmt.Errorf("%s:%d: entry must end with a period", name, n)
			case strings.Contains(text, "[#"):
				return nil, fmt.Errorf("%s:%d: entry must not link a pull request; the release adds ([#%d])", name, n, pr)
			}
			result = append(result, fragmentEntry{pr: pr, section: section, text: text})
			sectionEntries++
		default:
			return nil, fmt.Errorf("%s:%d: expected a ### section heading or a \"- \" entry on one line", name, n)
		}
	}
	if err := closeSection(len(lines)); err != nil {
		return nil, err
	}
	if len(result) == 0 {
		return nil, fmt.Errorf("%s: no entries", name)
	}
	return result, nil
}

func parseFragments(files map[string]string) ([]fragmentEntry, error) {
	var names []string
	for name := range files {
		if name != fragmentReadme {
			names = append(names, name)
		}
	}
	slices.Sort(names)
	var result []fragmentEntry
	for _, name := range names {
		parsed, err := parseFragment(name, files[name])
		if err != nil {
			return nil, err
		}
		result = append(result, parsed...)
	}
	return result, nil
}

func readFragmentDir(dir string) (map[string]string, error) {
	list, err := os.ReadDir(dir)
	if errors.Is(err, fs.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("list %s: %w", dir, err)
	}
	files := make(map[string]string, len(list))
	for _, entry := range list {
		if entry.IsDir() {
			return nil, fmt.Errorf("%s: unexpected directory", entry.Name())
		}
		data, err := os.ReadFile(filepath.Join(dir, entry.Name()))
		if err != nil {
			return nil, fmt.Errorf("read %s: %w", entry.Name(), err)
		}
		files[entry.Name()] = string(data)
	}
	return files, nil
}

func assemble(fragments []fragmentEntry) (body string, prs []int) {
	var b strings.Builder
	for _, section := range fragmentSections {
		var lines []fragmentEntry
		for _, e := range fragments {
			if e.section == section {
				lines = append(lines, e)
			}
		}
		if len(lines) == 0 {
			continue
		}
		slices.SortStableFunc(lines, func(a, b fragmentEntry) int {
			if a.breaking() != b.breaking() {
				if a.breaking() {
					return -1
				}
				return 1
			}
			return cmp.Compare(a.pr, b.pr)
		})
		if b.Len() > 0 {
			b.WriteString("\n")
		}
		b.WriteString("### " + section + "\n\n")
		for _, e := range lines {
			fmt.Fprintf(&b, "- %s ([#%d]).\n", strings.TrimSuffix(e.text, "."), e.pr)
		}
	}
	for _, e := range fragments {
		prs = append(prs, e.pr)
	}
	slices.Sort(prs)
	return strings.TrimSuffix(b.String(), "\n"), slices.Compact(prs)
}
