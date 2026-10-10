package main

import (
	"errors"
	"fmt"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

// gitCalls returns the calls of git that start with the prefix.
func gitCalls(git *fakeRunner, prefix string) []string {
	var out []string
	for _, c := range git.joined() {
		if strings.HasPrefix(c, prefix) {
			out = append(out, c)
		}
	}
	return out
}

// TestSnapshotRemovedPatchInheritedWithLaterLowerEdits: the old range had A (x = 1); the new base contains A, as itself or rewritten at another position, and then a lower commit C edits x further. Comparing A with the new base shows x changing, which read by lines looks like the layer dropping A. Provenance comes first: A is inherited, so nothing of it is the layer's removal. A rewrite is proven by a base commit with exactly A's changed lines; the log's first candidate differs only in whitespace and proves nothing.
func TestSnapshotRemovedPatchInheritedWithLaterLowerEdits(t *testing.T) {
	t.Parallel()
	rangeDiff := "1:  aaa1111 < -:  ------- A, in the base\n2:  aaa2222 = 1:  bbb2222 D, another file\n"
	const file = "diff --git a/x.go b/x.go\n--- a/x.go\n+++ b/x.go\n"
	// What reading by lines would see: C changed x on top of A, in the base and the head alike.
	patches := map[string]string{
		"aaa1111 aaa1111^": file + "@@ -5,3 +5,3 @@\n ctx\n-x = 1\n+x = 0\n tail\n",
		"aaa1111^ aaa1111": file + "@@ -5,3 +5,3 @@\n ctx\n-x = 0\n+x = 1\n tail\n",
		"aaa1111 -- x.go":  file + "@@ -6 +6 @@\n-x = 1\n+x = 2\n",
	}
	rewritten := "\x00\n\n" + file + "@@ -5,3 +5,3 @@\n ctx\n-x = 0\n+x =1\n tail\n" +
		"\x00\n\n" + file + "@@ -9,3 +9,3 @@\n other\n-x = 0\n+x = 1\n end\n"
	for name, r := range map[string]reReview{
		"ancestor":  {ancestors: map[string]bool{"aaa1111": true}},
		"rewritten": {logs: map[string]string{"aaa1111": rewritten}},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			r.body, r.rangeDiff, r.patches = previousBody(3, oldBaseSHA), rangeDiff, patches
			tl, git := r.tools(t)
			if err := runSnapshot(tl, []string{"--previous", previousURL, "700"}); err != nil {
				t.Fatal(err)
			}
			dir := snapshotDir(tl)
			m, err := readManifest(dir)
			if err != nil {
				t.Fatal(err)
			}
			wantPatches := []patchStatus{
				{Old: "aaa1111", Subject: "A, in the base", Status: "removed"},
				{Old: "aaa2222", New: "bbb2222", Subject: "D, another file", Status: "unchanged"},
			}
			if diff := cmp.Diff(wantPatches, m.Patches); diff != "" {
				t.Errorf("patches (-want +got):\n%s", diff)
			}
			if len(m.Files) != 0 || m.CarryForward {
				t.Errorf("files = %+v, carry %t; want no removal assigned to this layer", m.Files, m.CarryForward)
			}
			if got := readFile(t, filepath.Join(dir, "delta.diff")); got != "" {
				t.Errorf("delta.diff = %q, want empty", got)
			}
			if calls := gitCalls(git, "git diff"); slices.ContainsFunc(calls, func(c string) bool { return strings.Contains(c, " -- ") }) {
				t.Errorf("git traced the lines of an inherited commit: %q", calls)
			}
			if got, want := gitCalls(git, "git merge-base"), []string{"git merge-base --is-ancestor aaa1111 " + baseSHA}; !slices.Equal(got, want) {
				t.Errorf("ancestry checks = %q, want %q", got, want)
			}
			wantLog := name == "rewritten"
			if got := len(gitCalls(git, "git log")) == 1; got != wantLog {
				t.Errorf("rewrite check ran = %t, want %t: it is only asked when the commit is not an ancestor", got, wantLog)
			}
		})
	}
}

// TestSnapshotRemovedPatchWhitespaceRewriteIsNoProof: the old range had A (x = "a b"); the new base has a commit that differs from A only in whitespace (x = "ab"), so patch IDs, as git cherry compares them, match. The values differ, so A is not inherited: its loss is traced by lines and its reversal is the layer's removal.
func TestSnapshotRemovedPatchWhitespaceRewriteIsNoProof(t *testing.T) {
	t.Parallel()
	rangeDiff := "1:  aaa1111 < -:  ------- A, x = \"a b\"\n"
	const file = "diff --git a/x.go b/x.go\n--- a/x.go\n+++ b/x.go\n"
	patches := map[string]string{
		"aaa1111 aaa1111^": file + "@@ -5,3 +5,3 @@\n ctx\n-x = \"a b\"\n+x = 0\n tail\n",
		"aaa1111^ aaa1111": file + "@@ -5,3 +5,3 @@\n ctx\n-x = 0\n+x = \"a b\"\n tail\n",
		"aaa1111 -- x.go":  file + "@@ -6 +6 @@\n-x = \"a b\"\n+x = \"ab\"\n",
	}
	logs := map[string]string{"aaa1111": "\x00\n\n" + file + "@@ -5,3 +5,3 @@\n ctx\n-x = 0\n+x = \"ab\"\n tail\n"}
	tl, git := reReview{body: previousBody(3, oldBaseSHA), rangeDiff: rangeDiff, patches: patches, logs: logs}.tools(t)
	if err := runSnapshot(tl, []string{"--previous", previousURL, "700"}); err != nil {
		t.Fatal(err)
	}
	if len(gitCalls(git, "git log")) != 1 {
		t.Fatalf("git calls = %q, want one rewrite check", git.joined())
	}
	dir := snapshotDir(tl)
	m, err := readManifest(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(m.Files) != 1 || m.Files[0].Path != "x.go" {
		t.Errorf("files = %+v, want the traced removal of x.go", m.Files)
	}
	if got := readFile(t, filepath.Join(dir, "delta.diff")); !strings.Contains(got, "\n-x = \"a b\"\n") {
		t.Errorf("delta.diff = %q, want the reversal of x = \"a b\"", got)
	}
}

// TestSnapshotRemovedPatchWithoutProvenanceIsTracedByLines: a commit that is neither an ancestor of the new base nor in it by patch falls back to the line trace, after both checks said no.
func TestSnapshotRemovedPatchWithoutProvenanceIsTracedByLines(t *testing.T) {
	t.Parallel()
	rangeDiff := "1:  aaa1111 < -:  ------- dropped\n"
	patches := map[string]string{
		"aaa1111 aaa1111^": onePatch("d.go", "ctx", "gone"),
		"aaa1111 -- d.go":  "diff --git a/d.go b/d.go\n--- a/d.go\n+++ b/d.go\n@@ -6 +6 @@\n-old\n+gone\n",
	}
	tl, git := reReview{body: previousBody(3, oldBaseSHA), rangeDiff: rangeDiff, patches: patches}.tools(t)
	if err := runSnapshot(tl, []string{"--previous", previousURL, "700"}); err != nil {
		t.Fatal(err)
	}
	var kinds []string
	for _, c := range git.joined()[2:] {
		kinds = append(kinds, strings.Fields(c)[1])
	}
	// fetch and range-diff come first.
	if diff := cmp.Diff([]string{"merge-base", "diff", "log", "diff", "diff"}, kinds); diff != "" {
		t.Errorf("git calls after range-diff (-want +got):\n%s", diff)
	}
	m, err := readManifest(snapshotDir(tl))
	if err != nil {
		t.Fatal(err)
	}
	if len(m.Files) != 1 || m.Files[0].Path != "d.go" {
		t.Errorf("files = %+v, want the traced removal of d.go", m.Files)
	}
}

func TestSnapshotRemovedPatchProvenanceFailure(t *testing.T) {
	t.Parallel()
	for name, err := range map[string]error{
		"unknown commit":  errors.New("git merge-base: fatal: Not a valid commit name"),
		"exit status 128": fmt.Errorf("git merge-base: %w", exitStatus(128)),
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			tl, git := reReview{
				body: previousBody(3, oldBaseSHA), rangeDiff: "1:  aaa1111 < -:  ------- dropped\n",
				ancestryErr: map[string]error{"aaa1111": err},
			}.tools(t)
			got := runSnapshot(tl, []string{"--previous", previousURL, "700"})
			if got == nil || !strings.Contains(got.Error(), "merge-base --is-ancestor aaa1111") || !errors.Is(got, err) {
				t.Fatalf("error = %v, want the ancestry failure surfaced", got)
			}
			if calls := gitCalls(git, "git diff"); len(calls) != 0 {
				t.Errorf("a failed ancestry check was taken for a no: %q", calls)
			}
		})
	}
}

func TestLostEffectsOperations(t *testing.T) {
	t.Parallel()
	parse := func(in string) []fileDiff {
		files, err := parseDiff(in)
		if err != nil {
			t.Fatal(err)
		}
		return files
	}
	// The reversal of a patch that created f with the sole line G.
	const created = "diff --git a/f b/f\ndeleted file mode 100644\nindex 1111111..0000000\n--- a/f\n+++ /dev/null\n@@ -1 +0,0 @@\n-G = 1\n"
	// Since: f exists, emptied.
	const emptied = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +0,0 @@\n-G = 1\n"
	// Since: f is gone.
	const removed = "diff --git a/f b/f\ndeleted file mode 100644\nindex 2222222..0000000\n--- a/f\n+++ /dev/null\n@@ -1 +0,0 @@\n-G = 1\n"
	// The reversal of a patch that renamed a to b and added G.
	const renamed = "diff --git a/b b/a\nsimilarity index 90%\nrename from b\nrename to a\nindex 1111111..2222222 100644\n--- a/b\n+++ b/a\n@@ -5,3 +5,2 @@\n ctx\n-G = 1\n tail\n"
	// Since, with the rename kept: b without G.
	const lostG = "diff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -6 +5,0 @@\n-G = 1\n"
	// Since, with the rename undone: b is gone, a is back.
	bGone := "diff --git a/b b/b\ndeleted file mode 100644\nindex 2222222..0000000\n--- a/b\n+++ /dev/null\n@@ -1,10 +0,0 @@\n" + strings.Repeat("-line\n", 10)
	aBack := "diff --git a/a b/a\nnew file mode 100644\nindex 0000000..3333333\n--- /dev/null\n+++ b/a\n@@ -0,0 +1,2 @@\n+x\n+y\n"
	// The reversal of a patch that created a binary file, and since: it changed.
	const binary = "diff --git a/p b/p\ndeleted file mode 100644\nindex 1111111..0000000\nBinary files a/p and /dev/null differ\n"
	const binaryChanged = "diff --git a/p b/p\nindex 1111111..2222222 100644\nBinary files a/p and b/p differ\n"
	const contentOnly = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +0,0 @@\n-G = 1\n"
	tests := []struct {
		name                         string
		f                            string
		base, head                   string
		wantText                     string // "" when nothing is lost
		wantPath, wantOld, wantState string
	}{
		{"created file kept but emptied", created, emptied, emptied, contentOnly, "f", "", "modified"},
		{"created file kept in one layer, emptied in both", created, removed, emptied, contentOnly, "f", "", "modified"},
		{"created file gone in both", created, removed, removed, created, "f", "", "deleted"},
		{"created file kept whole", created, "", "", "", "", "", ""},
		{"rename kept, G lost", renamed, lostG, lostG, "diff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -5,3 +5,2 @@\n ctx\n-G = 1\n tail\n", "b", "", "modified"},
		{"rename undone", renamed, bGone + aBack, bGone + aBack, renamed, "a", "b", "renamed"},
		{"rename undone only in the base", renamed, bGone + aBack, lostG, "diff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -5,3 +5,2 @@\n ctx\n-G = 1\n tail\n", "b", "", "modified"},
		{"rename kept, nothing lost", renamed, "", "", "", "", "", ""},
		{"binary created and changed since", binary, binaryChanged, binaryChanged, "diff --git a/p b/p\nBinary files a/p and b/p differ\n", "p", "", "modified"},
	}
	for _, tt := range tests {
		got, ok := lostEffects(parse(tt.f)[0], parse(tt.base), parse(tt.head))
		if ok != (tt.wantText != "") {
			t.Errorf("%s: lost = %t, want %t (text %q)", tt.name, ok, tt.wantText != "", got.Text)
			continue
		}
		if ok && (got.Text != tt.wantText || got.Path != tt.wantPath || got.OldPath != tt.wantOld || got.Status != tt.wantState) {
			t.Errorf("%s: got %q at path %q (from %q, %s); want %q at %q (from %q, %s)", tt.name, got.Text, got.Path, got.OldPath, got.Status, tt.wantText, tt.wantPath, tt.wantOld, tt.wantState)
		}
	}
}

// TestSnapshotRemovedPatchCreatedFileKeptEmpty: the removed patch created the file with one line, the layers keep the file and drop the line. The delta is the content change of a file that still exists, not its deletion.
func TestSnapshotRemovedPatchCreatedFileKeptEmpty(t *testing.T) {
	t.Parallel()
	rangeDiff := "1:  aaa1111 < -:  ------- adds f.go\n"
	patches := map[string]string{
		"aaa1111 aaa1111^": "diff --git a/f.go b/f.go\ndeleted file mode 100644\nindex 1111111..0000000\n--- a/f.go\n+++ /dev/null\n@@ -1 +0,0 @@\n-G = 1\n",
		"aaa1111 -- f.go":  "diff --git a/f.go b/f.go\n--- a/f.go\n+++ b/f.go\n@@ -1 +0,0 @@\n-G = 1\n",
	}
	tl, _ := reReviewTools(t, previousBody(3, oldBaseSHA), rangeDiff, patches)
	if err := runSnapshot(tl, []string{"--previous", previousURL, "700"}); err != nil {
		t.Fatal(err)
	}
	dir := snapshotDir(tl)
	m, err := readManifest(dir)
	if err != nil {
		t.Fatal(err)
	}
	want := "diff --git a/f.go b/f.go\n--- a/f.go\n+++ b/f.go\n@@ -1 +0,0 @@\n-G = 1\n"
	if len(m.Files) != 1 || m.Files[0].Path != "f.go" || m.Files[0].OldPath != "" || m.Files[0].Status != "modified" || m.Files[0].Removed != 1 || m.Files[0].Added != 0 {
		t.Errorf("files = %+v, want f.go modified, one line removed", m.Files)
	}
	if got := readFile(t, filepath.Join(dir, "delta.diff")); got != want {
		t.Errorf("delta.diff = %q, want %q", got, want)
	}
	if got := readFile(t, filepath.Join(dir, m.Files[0].HunkFile)); got != want {
		t.Errorf("hunk file = %q, want %q", got, want)
	}
}

// TestSnapshotRemovedPatchRenameKept: the removed patch renamed a.go to b.go and added G. The layers keep the rename and drop G, so the delta removes G from b.go and does not rename it back.
func TestSnapshotRemovedPatchRenameKept(t *testing.T) {
	t.Parallel()
	rangeDiff := "1:  aaa1111 < -:  ------- renames a.go and adds G\n"
	lostG := "diff --git a/b.go b/b.go\n--- a/b.go\n+++ b/b.go\n@@ -6 +5,0 @@\n-G = 1\n"
	patches := map[string]string{
		"aaa1111 aaa1111^":          "diff --git a/b.go b/a.go\nsimilarity index 90%\nrename from b.go\nrename to a.go\nindex 1111111..2222222 100644\n--- a/b.go\n+++ b/a.go\n@@ -5,3 +5,2 @@\n ctx\n-G = 1\n tail\n",
		"aaa1111 base -- b.go a.go": lostG,
		"aaa1111 head -- b.go a.go": lostG,
	}
	tl, git := reReviewTools(t, previousBody(3, oldBaseSHA), rangeDiff, patches)
	if err := runSnapshot(tl, []string{"--previous", previousURL, "700"}); err != nil {
		t.Fatal(err)
	}
	dir := snapshotDir(tl)
	m, err := readManifest(dir)
	if err != nil {
		t.Fatal(err)
	}
	want := "diff --git a/b.go b/b.go\n--- a/b.go\n+++ b/b.go\n@@ -5,3 +5,2 @@\n ctx\n-G = 1\n tail\n"
	if len(m.Files) != 1 || m.Files[0].Path != "b.go" || m.Files[0].OldPath != "" || m.Files[0].Status != "modified" || m.Files[0].Removed != 1 {
		t.Errorf("files = %+v, want b.go modified with only G removed, no rename", m.Files)
	}
	if got := readFile(t, filepath.Join(dir, "delta.diff")); got != want {
		t.Errorf("delta.diff = %q, want %q", got, want)
	}
	if got := readFile(t, filepath.Join(dir, m.Files[0].HunkFile)); got != want {
		t.Errorf("hunk file = %q, want %q", got, want)
	}
	// Both names of the renamed file are traced.
	wantCall := "git diff --no-color --no-ext-diff --no-renames -U0 aaa1111 " + headSHA + " -- :(literal)b.go :(literal)a.go"
	if !slices.Contains(git.joined(), wantCall) {
		t.Errorf("git calls %q lack %q", git.joined(), wantCall)
	}
}
