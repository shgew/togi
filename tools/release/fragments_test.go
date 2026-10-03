package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestParseFragment(t *testing.T) {
	t.Parallel()
	got, err := parseFragment("42.md", "### Changed\n\n- **BREAKING** Reset the core.\n- Faster.\n\n### Fixed\n\n- A crash.\n")
	if err != nil {
		t.Fatal(err)
	}
	want := []fragmentEntry{
		{pr: 42, section: "Changed", text: "**BREAKING** Reset the core."},
		{pr: 42, section: "Changed", text: "Faster."},
		{pr: 42, section: "Fixed", text: "A crash."},
	}
	if diff := cmp.Diff(want, got, cmp.AllowUnexported(fragmentEntry{})); diff != "" {
		t.Fatalf("entries mismatch (-want +got):\n%s", diff)
	}
}

func TestParseFragmentRejects(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct{ name, file, content, want string }{
		{"slug name", "fast-state.md", "### Fixed\n\n- A crash.\n", "fast-state.md: name must be the pull request number"},
		{"leading zero", "042.md", "### Fixed\n\n- A crash.\n", "042.md: name must be"},
		{"other extension", "42.txt", "### Fixed\n\n- A crash.\n", "42.txt: name must be"},
		{"empty", "42.md", "\n", "42.md: no entries"},
		{"unknown section", "42.md", "### Security\n\n- A hole.\n", `42.md:1: section "Security" is not one of Added, Changed, Removed, Fixed`},
		{"before heading", "42.md", "- A crash.\n", "42.md:1: entry before any ### section heading"},
		{"no period", "42.md", "### Fixed\n\n- A crash\n", "42.md:3: entry must end with a period"},
		{"bare period", "42.md", "### Fixed\n\n- .\n", "42.md:3: entry must end with a period"},
		{"own link", "42.md", "### Fixed\n\n- A crash ([#42]).\n", "42.md:3: entry must not link a pull request; the release adds ([#42])"},
		{"empty section", "42.md", "### Added\n\n### Fixed\n\n- A crash.\n", "42.md:3: ### Added has no entries"},
		{"empty last section", "42.md", "### Fixed\n\n- A crash.\n\n### Added\n", "42.md:6: ### Added has no entries"},
		{"continuation", "42.md", "### Fixed\n\n- A crash\n  on resume.\n", "42.md:3: entry must end with a period"},
		{"prose", "42.md", "### Fixed\n\nA crash.\n", "42.md:3: expected a ### section heading or a \"- \" entry on one line"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			_, err := parseFragment(tc.file, tc.content)
			if err == nil || !strings.HasPrefix(err.Error(), tc.want) {
				t.Fatalf("error = %v, want prefix %q", err, tc.want)
			}
		})
	}
}

func TestAssemble(t *testing.T) {
	t.Parallel()
	fragments, err := parseFragments(map[string]string{
		"README.md": "Not a fragment.",
		"9.md":      "### Fixed\n\n- Ninth fix.\n\n### Added\n\n- Ninth feature.\n",
		"12.md":     "### Changed\n\n- Twelfth change.\n- **BREAKING** Twelfth reset.\n",
		"10.md":     "### Fixed\n\n- **BREAKING** Tenth fix.\n\n### Removed\n\n- Tenth removal.\n",
		"3.md":      "### Changed\n\n- Third change.\n",
	})
	if err != nil {
		t.Fatal(err)
	}
	body, prs := assemble(fragments)
	want := "### Added\n\n- Ninth feature ([#9]).\n\n" +
		"### Changed\n\n- **BREAKING** Twelfth reset ([#12]).\n- Third change ([#3]).\n- Twelfth change ([#12]).\n\n" +
		"### Removed\n\n- Tenth removal ([#10]).\n\n" +
		"### Fixed\n\n- **BREAKING** Tenth fix ([#10]).\n- Ninth fix ([#9])."
	if diff := cmp.Diff(want, body); diff != "" {
		t.Fatalf("body mismatch (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff([]int{3, 9, 10, 12}, prs); diff != "" {
		t.Fatalf("pull requests mismatch (-want +got):\n%s", diff)
	}
	if next, _, err := bump("0.8.0", body); err != nil || next != "0.9.0" {
		t.Fatalf("bump = %q, %v; want 0.9.0 from the assembled breaking entries", next, err)
	}
}

func TestCheckFragments(t *testing.T) {
	t.Parallel()
	write := func(t *testing.T, dir, name, content string) {
		t.Helper()
		if err := os.WriteFile(filepath.Join(dir, name), []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	t.Run("valid", func(t *testing.T) {
		t.Parallel()
		dir := t.TempDir()
		write(t, dir, "README.md", "# Changes\n\nFree-form instructions.\n")
		write(t, dir, "7.md", "### Added\n\n- A feature.\n")
		if err := checkFragments(dir); err != nil {
			t.Fatal(err)
		}
	})
	t.Run("missing directory", func(t *testing.T) {
		t.Parallel()
		if err := checkFragments(filepath.Join(t.TempDir(), "changes")); err != nil {
			t.Fatal(err)
		}
	})
	t.Run("invalid fragment", func(t *testing.T) {
		t.Parallel()
		dir := t.TempDir()
		write(t, dir, "7.md", "### Added\n\n- A feature\n")
		if err := checkFragments(dir); err == nil || !strings.Contains(err.Error(), "7.md:3: entry must end with a period") {
			t.Fatalf("error = %v", err)
		}
	})
	t.Run("subdirectory", func(t *testing.T) {
		t.Parallel()
		dir := t.TempDir()
		if err := os.Mkdir(filepath.Join(dir, "7.md"), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := checkFragments(dir); err == nil || !strings.Contains(err.Error(), "7.md: unexpected directory") {
			t.Fatalf("error = %v", err)
		}
	})
}
