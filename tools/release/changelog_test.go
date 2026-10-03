package main

import (
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
)

func TestBump(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name, version, body, want, reason string
	}{
		{"pre-one-added", "0.1.0", "### Added\n\n- A feature", "0.1.1", "patch"},
		{"pre-one-breaking", "0.1.9", "### Changed\n\n- **BREAKING** migration", "0.2.0", "migration"},
		{"post-one-added", "1.2.3", "### Added\n\n- A feature", "1.3.0", "minor"},
		{"post-one-fixed", "1.2.3", "### Fixed\n\n- Repair", "1.2.4", "patch"},
		{"post-one-breaking", "1.2.3", "### Fixed\n\n- **BREAKING** state reset", "2.0.0", "state reset"},
		{"leading-zero", "01.2.3", "- Item", "", "invalid"},
		{"overflow", strings.Repeat("9", 40) + ".2.3", "- Item", "", "parse version"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			got, reason, err := bump(tc.version, tc.body)
			if tc.want == "" {
				if err == nil || !strings.Contains(err.Error(), tc.reason) {
					t.Fatalf("bump error = %v, want %q", err, tc.reason)
				}
				return
			}
			if err != nil || got != tc.want || !strings.Contains(reason, tc.reason) {
				t.Fatalf("bump = %q, %q, %v; want %q, reason containing %q", got, reason, err, tc.want, tc.reason)
			}
		})
	}
}

func TestChangelogRewriteAndNotes(t *testing.T) {
	t.Parallel()
	const before = "# Changelog\n\n## [0.1.0] - 2026-09-01\n\n### Fixed\n\n- Old behavior ([#2]).\n\n[0.1.0]: https://forge.example/o/r/releases/tag/v0.1.0\n\n[#2]: https://forge.example/o/r/pull/2\n[#40]: https://forge.example/o/r/pull/40\n"
	updated, err := rewriteChangelog(before, "0.1.1", "### Added\n\n- New option ([#34]).\n- Old link ([#40]).", []int{34, 40}, "https://forge.example/o/r/", time.Date(2026, 9, 25, 16, 0, 0, 0, time.FixedZone("ahead", 3600)))
	if err != nil {
		t.Fatal(err)
	}
	want := "# Changelog\n\n## [0.1.1] - 2026-09-25\n\n### Added\n\n- New option ([#34]).\n- Old link ([#40]).\n\n## [0.1.0] - 2026-09-01\n\n### Fixed\n\n- Old behavior ([#2]).\n\n[0.1.0]: https://forge.example/o/r/releases/tag/v0.1.0\n\n[0.1.1]: https://forge.example/o/r/releases/tag/v0.1.1\n\n[#2]: https://forge.example/o/r/pull/2\n[#34]: https://forge.example/o/r/pull/34\n[#40]: https://forge.example/o/r/pull/40\n"
	if diff := cmp.Diff(want, updated); diff != "" {
		t.Fatalf("rewrite mismatch (-want +got):\n%s", diff)
	}
	s, ok := sectionNamed(updated, "0.1.1")
	if !ok || s.date != "2026-09-25" || len(entries(s.body)) != 2 {
		t.Fatalf("released section = %+v, found %v", s, ok)
	}
	notes := releaseNotes(updated, s)
	if !strings.Contains(notes, "[#34]: https://forge.example/o/r/pull/34") || strings.Contains(notes, "[#2]:") || strings.Contains(notes, "[0.1.1]:") {
		t.Fatalf("wrong release notes: %s", notes)
	}
}

func TestReleaseSections(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name, changelog, version, wantBody, wantNotes string
	}{
		{
			"references", "## [1.2.0] - 2026-09-24\n\n- Change ([#8]) and ([#3]).\n\n[#3]: https://forge.example/pulls/3\n[#8]: https://forge.example/pulls/8\n[#9]: https://forge.example/pulls/9\n",
			"1.2.0", "- Change ([#8]) and ([#3]).",
			"- Change ([#8]) and ([#3]).\n\n[#3]: https://forge.example/pulls/3\n\n[#8]: https://forge.example/pulls/8",
		},
		{
			"no-references", "## [1.2.1] - 2026-09-25\n\n### Fixed\n\n- Repair.\n",
			"1.2.1", "### Fixed\n\n- Repair.", "### Fixed\n\n- Repair.",
		},
		{
			"named-links",
			"## [1.2.2] - 2026-09-25\n\n- See [the guide][guide] and [source].\n\n[guide]: https://forge.example/guide\n[source]: https://forge.example/source\n[unused]: https://forge.example/unused\n",
			"1.2.2", "- See [the guide][guide] and [source].",
			"- See [the guide][guide] and [source].\n\n[guide]: https://forge.example/guide\n\n[source]: https://forge.example/source",
		},
		{
			"inline-link-is-not-reference",
			"## [1.2.3] - 2026-09-25\n\n- See [guide](https://forge.example/guide).\n\n[guide]: https://forge.example/unrelated\n",
			"1.2.3", "- See [guide](https://forge.example/guide).",
			"- See [guide](https://forge.example/guide).",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			s, ok := sectionNamed(tc.changelog, tc.version)
			if !ok || strings.TrimSpace(s.body) != tc.wantBody {
				t.Fatalf("section = %+v, found %v", s, ok)
			}
			if got := releaseNotes(tc.changelog, s); got != tc.wantNotes {
				t.Fatalf("notes = %q, want %q", got, tc.wantNotes)
			}
		})
	}
}

func TestRewriteFirstRelease(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name, input, want string
	}{
		{
			"header only", "# Changelog\n",
			"# Changelog\n\n## [0.1.0] - 2026-09-25\n\n- New ([#4]).\n\n[0.1.0]: https://forge.example/o/r/releases/tag/v0.1.0\n\n[#4]: https://forge.example/o/r/pull/4\n",
		},
		{
			"existing definitions", "# Changelog\n\n[#1]: https://forge.example/o/r/issues/1\n",
			"# Changelog\n\n## [0.1.0] - 2026-09-25\n\n- New ([#4]).\n\n[0.1.0]: https://forge.example/o/r/releases/tag/v0.1.0\n\n[#1]: https://forge.example/o/r/issues/1\n[#4]: https://forge.example/o/r/pull/4\n",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			got, err := rewriteChangelog(tc.input, "0.1.0", "- New ([#4]).", []int{4}, "https://forge.example/o/r", time.Date(2026, 9, 25, 0, 0, 0, 0, time.UTC))
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Fatalf("rewrite mismatch (-want +got):\n%s", diff)
			}
		})
	}
}

func TestRewriteRefusesExistingVersion(t *testing.T) {
	t.Parallel()
	got, err := rewriteChangelog("# Changelog\n\n## [1.2.3] - 2026-09-25\n", "1.2.3", "- Change.", nil, "https://forge.example/o/r", time.Date(2026, 9, 25, 0, 0, 0, 0, time.UTC))
	if err == nil || err.Error() != "version [1.2.3] already exists" || got != "" {
		t.Fatalf("rewrite = %q, %v; want empty output and an existing-version error", got, err)
	}
}
