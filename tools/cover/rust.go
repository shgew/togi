package main

import (
	"bufio"
	"bytes"
	"fmt"
	"io"
	"maps"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
)

// rustHost is the target the coverage build compiled for: its triple and the cfg options `rustc --print cfg` names for it.
type rustHost struct {
	triple string
	cfg    hostCfg
}

// coverageCfg holds the option cargo llvm-cov enables beyond the target's own: cfg(coverage).
// test is not in it: unit-test builds set it, while the library builds that integration tests and dependents link do not.
var coverageCfg = []string{"coverage"}

func hostOf() (rustHost, error) {
	version, err := exec.Command("rustc", "-vV").Output()
	if err != nil {
		return rustHost{}, fmt.Errorf("cover: rustc -vV: %w", err)
	}
	printed, err := exec.Command("rustc", "--print", "cfg").Output()
	if err != nil {
		return rustHost{}, fmt.Errorf("cover: rustc --print cfg: %w", err)
	}
	return hostFrom(string(version), string(printed))
}

// hostFrom builds the host from the output of `rustc -vV` and `rustc --print cfg`.
func hostFrom(version, printed string) (rustHost, error) {
	var triple string
	for line := range strings.Lines(version) {
		if host, ok := strings.CutPrefix(strings.TrimSpace(line), "host: "); ok {
			triple = host
		}
	}
	if triple == "" {
		return rustHost{}, fmt.Errorf("cover: rustc -vV names no host")
	}
	options := slices.Clone(coverageCfg)
	for line := range strings.Lines(printed) {
		if line = strings.TrimSpace(line); line != "" {
			options = append(options, line)
		}
	}
	return rustHost{triple: triple, cfg: newHostCfg(options...)}, nil
}

// runRust reports the changed Rust lines the lcov trace of a cargo llvm-cov run leaves unreached, then the changed files with code that build left out.
func runRust(tracePath, buildDir, base string, out io.Writer) error {
	trace, err := os.Open(tracePath)
	if err != nil {
		return fmt.Errorf("cover: open lcov trace: %w", err)
	}
	defer trace.Close()
	host, err := hostOf()
	if err != nil {
		return err
	}
	root, err := os.Getwd()
	if err != nil {
		return fmt.Errorf("cover: working directory: %w", err)
	}
	return reportRust(trace, buildDir, root, base, host, out)
}

func reportRust(trace io.Reader, buildDir, root, base string, host rustHost, out io.Writer) error {
	diff, err := exec.Command("git", "diff", "-U0", "--inter-hunk-context=0", "--no-color", "--no-ext-diff", "--no-prefix", "--merge-base", base, "--", "*.rs").Output()
	if err != nil {
		return fmt.Errorf("cover: diff against %s: %w", base, err)
	}
	changed, err := parseDiff(bytes.NewReader(diff))
	if err != nil {
		return err
	}
	if len(changed) == 0 {
		_, err := fmt.Fprintf(out, "No Rust lines changed since %s.\n", base)
		return err
	}
	lines, err := parseLcov(trace, root)
	if err != nil {
		return err
	}
	built, err := builtSources(buildDir, root)
	if err != nil {
		return err
	}
	unbuilt, err := rustNotBuilt(root, changed, built, lines, host.cfg)
	if err != nil {
		return err
	}
	gaps := intersect(changed, unreached(lines))
	if len(gaps) == 0 {
		if _, err := fmt.Fprintf(out, "Every changed Rust line the trace measures ran in a test since %s.\n", base); err != nil {
			return err
		}
	}
	for _, g := range gaps {
		if _, err := fmt.Fprintln(out, g); err != nil {
			return err
		}
	}
	for _, path := range unbuilt {
		if _, err := fmt.Fprintf(out, "%s: not built for %s\n", path, host.triple); err != nil {
			return err
		}
	}
	return nil
}

// parseLcov returns, by repository-relative path, whether a test reached each line the trace measures.
// A line listed more than once counts as reached when any listing reached it. Files outside root are left out.
func parseLcov(r io.Reader, root string) (map[string]map[int]bool, error) {
	files := map[string]map[int]bool{}
	var current map[int]bool
	s := bufio.NewScanner(r)
	s.Buffer(nil, 16<<20)
	for s.Scan() {
		line := s.Text()
		switch {
		case strings.HasPrefix(line, "SF:"):
			current = nil
			path, err := filepath.Rel(root, strings.TrimPrefix(line, "SF:"))
			if err != nil || !filepath.IsLocal(path) {
				continue
			}
			path = filepath.ToSlash(path)
			if files[path] == nil {
				files[path] = map[int]bool{}
			}
			current = files[path]
		case strings.HasPrefix(line, "DA:"):
			if current == nil {
				continue
			}
			fields := strings.Split(strings.TrimPrefix(line, "DA:"), ",")
			if len(fields) < 2 {
				return nil, fmt.Errorf("cover: malformed lcov line %q", line)
			}
			n, err := strconv.Atoi(fields[0])
			if err != nil {
				return nil, fmt.Errorf("cover: malformed lcov line %q: %w", line, err)
			}
			count, err := strconv.ParseUint(fields[1], 10, 64)
			if err != nil {
				return nil, fmt.Errorf("cover: malformed lcov line %q: %w", line, err)
			}
			current[n] = current[n] || count > 0
		case line == "end_of_record":
			current = nil
		}
	}
	if err := s.Err(); err != nil {
		return nil, fmt.Errorf("cover: read lcov trace: %w", err)
	}
	return files, nil
}

// unreached joins each file's unreached lines into ranges; lines the trace does not measure, such as blank lines and comments, do not split a range.
func unreached(files map[string]map[int]bool) map[string][]lineRange {
	out := map[string][]lineRange{}
	for path, lines := range files {
		var open *lineRange
		for _, n := range slices.Sorted(maps.Keys(lines)) {
			switch {
			case lines[n]:
				open = nil
			case open != nil:
				open.last = n
			default:
				out[path] = append(out[path], lineRange{n, n})
				open = &out[path][len(out[path])-1]
			}
		}
	}
	return out
}

// builtSources returns the repository-relative sources the dep-info files under a build profile directory name: every file a build into it compiled.
// Crates and their tests write dep-info into deps, build scripts into build/<package>-<hash>.
// Each source also has a line of its own ending in a colon, which is what is read; rustc escapes a space in its path as a backslash and a space.
func builtSources(buildDir, root string) (map[string]bool, error) {
	var infos []string
	for _, pattern := range []string{"deps/*.d", "build/*/*.d"} {
		found, err := filepath.Glob(filepath.Join(buildDir, pattern))
		if err != nil {
			return nil, fmt.Errorf("cover: list dep-info files: %w", err)
		}
		infos = append(infos, found...)
	}
	if len(infos) == 0 {
		return nil, fmt.Errorf("cover: %s holds no dep-info files in deps or build/*", buildDir)
	}
	built := map[string]bool{}
	for _, info := range infos {
		data, err := os.ReadFile(info)
		if err != nil {
			return nil, fmt.Errorf("cover: read dep-info: %w", err)
		}
		for line := range strings.Lines(string(data)) {
			source, ok := strings.CutSuffix(strings.TrimRight(line, "\n"), ":")
			if !ok {
				continue
			}
			source = strings.ReplaceAll(source, `\ `, " ")
			if filepath.IsAbs(source) {
				if source, err = filepath.Rel(root, source); err != nil {
					continue
				}
			}
			if filepath.IsLocal(source) {
				built[filepath.ToSlash(source)] = true
			}
		}
	}
	return built, nil
}

// rustNotBuilt returns the changed files, sorted, that the coverage build leaves out in part or whole: a file no dep-info names,
// or one with a changed line inside an item whose cfg predicate is false or unknown for the host and none of whose lines the trace measures.
// A measured line inside the item shows some build compiled it, so the item is not claimed; this covers cfg(not(test)) and cfg(feature = "...") code with executable lines.
// measured holds the lines the trace lists per file, reached or not.
func rustNotBuilt(root string, changed map[string][]lineRange, built map[string]bool, measured map[string]map[int]bool, cfg hostCfg) ([]string, error) {
	var paths []string
	for _, path := range slices.Sorted(maps.Keys(changed)) {
		if !built[path] {
			paths = append(paths, path)
			continue
		}
		src, err := os.ReadFile(filepath.Join(root, path))
		if err != nil {
			return nil, fmt.Errorf("cover: read %s: %w", path, err)
		}
		if slices.ContainsFunc(cfgOut(src, cfg), func(item lineRange) bool {
			for n := range measured[path] {
				if item.first <= n && n <= item.last {
					return false
				}
			}
			return slices.ContainsFunc(changed[path], func(c lineRange) bool { return max(c.first, item.first) <= min(c.last, item.last) })
		}) {
			paths = append(paths, path)
		}
	}
	return paths, nil
}
