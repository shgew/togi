package main

import (
	"go/build"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestChangedLinesNoTestReached(t *testing.T) {
	const module = "example.com/m"
	tests := []struct {
		name    string
		profile string
		diff    string
		want    []string
	}{
		{
			name: "uncovered block outside the hunks",
			profile: `mode: set
example.com/m/a.go:3.2,5.3 2 0
example.com/m/a.go:10.2,12.3 2 1
`,
			diff: `diff --git a.go a.go
--- a.go
+++ a.go
@@ -10,0 +11 @@ func f() {
+	y := 2
`,
		},
		{
			name: "only the changed lines of a partly changed block",
			profile: `mode: set
example.com/m/a.go:3.2,20.3 5 0
`,
			diff: `diff --git a.go a.go
--- a.go
+++ a.go
@@ -1,2 +1,4 @@
+package a
@@ -9 +11,2 @@ func f() {
+	x := 1
+	y := 2
@@ -30,0 +33 @@ func g() {
+	z := 3
`,
			want: []string{"a.go:3-4", "a.go:11-12"},
		},
		{
			name: "block reached by another listing",
			profile: `mode: atomic
example.com/m/a.go:3.2,5.3 2 0
example.com/m/a.go:3.2,5.3 2 4
`,
			diff: `diff --git a.go a.go
--- a.go
+++ a.go
@@ -3 +3 @@
+	x := 1
`,
		},
		{
			name: "adjacent blocks merge and files sort",
			profile: `mode: set
example.com/m/b.go:3.2,4.10 1 0
example.com/m/b.go:4.10,6.3 1 0
example.com/m/b.go:8.2,8.20 1 0
example.com/m/a.go:1.1,2.2 1 0
`,
			diff: `diff --git b.go b.go
--- b.go
+++ b.go
@@ -1,0 +1,10 @@
+x
diff --git a.go a.go
--- a.go
+++ a.go
@@ -2 +2 @@
+y
`,
			want: []string{"a.go:2", "b.go:3-6", "b.go:8"},
		},
		{
			name: "deletions add no lines",
			profile: `mode: set
example.com/m/a.go:1.1,9.2 3 0
`,
			diff: `diff --git a.go a.go
--- a.go
+++ a.go
@@ -4,2 +3,0 @@
-	gone := 1
-	gone2 := 2
diff --git old.go old.go
deleted file mode 100644
--- old.go
+++ /dev/null
@@ -1,3 +0,0 @@
-package a
`,
		},
		{
			name: "added content resembling a header",
			profile: `mode: set
example.com/m/a.go:1.1,3.2 1 0
`,
			diff: `diff --git a.go a.go
--- a.go
+++ a.go
@@ -0,0 +1,2 @@
+++ b.go
+x
`,
			want: []string{"a.go:1-2"},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			changed, err := parseDiff(strings.NewReader(tt.diff))
			if err != nil {
				t.Fatal(err)
			}
			uncovered, err := parseProfile(strings.NewReader(tt.profile), module)
			if err != nil {
				t.Fatal(err)
			}
			var got []string
			for _, g := range intersect(changed, uncovered) {
				got = append(got, g.String())
			}
			if diff := cmp.Diff(tt.want, got); diff != "" {
				t.Errorf("gaps (-want +got):\n%s", diff)
			}
		})
	}
}

func TestProfileOutsideModule(t *testing.T) {
	_, err := parseProfile(strings.NewReader("mode: set\nother.org/x/a.go:1.1,2.2 1 0\n"), "example.com/m")
	if err == nil || !strings.Contains(err.Error(), "outside module") {
		t.Fatalf("err = %v, want a profile entry outside the module refused", err)
	}
}

func TestChangedFilesTheBuildLeavesOut(t *testing.T) {
	root := t.TempDir()
	files := map[string]string{
		"a.go":                        "package a\n",
		"a_linux.go":                  "package a\n",
		"a_darwin.go":                 "package a\n",
		"hardware.go":                 "//go:build hardware && linux\n\npackage a\n",
		"integration.go":              "//go:build integration\n\npackage a\n",
		"sub/process_linux_test.go":   "package sub\n",
		"sub/process_darwin_test.go":  "package sub\n",
		"sub/fallback.go":             "//go:build !linux\n\npackage sub\n",
		"sub/unchanged_linux_test.go": "package sub\n",
	}
	for name, content := range files {
		path := filepath.Join(root, name)
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	changed := map[string][]lineRange{}
	for name := range files {
		if name != "sub/unchanged_linux_test.go" {
			changed[name] = []lineRange{{1, 1}}
		}
	}
	for _, tt := range []struct {
		goos string
		want []string
	}{
		{"darwin", []string{"a_linux.go", "hardware.go", "sub/process_linux_test.go"}},
		{"linux", []string{"a_darwin.go", "hardware.go", "sub/fallback.go", "sub/process_darwin_test.go"}},
	} {
		t.Run(tt.goos, func(t *testing.T) {
			ctx := build.Default
			ctx.GOOS, ctx.GOARCH, ctx.BuildTags = tt.goos, "arm64", []string{"integration"}
			got, err := notBuilt(ctx, root, changed)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tt.want, got); diff != "" {
				t.Errorf("files left out (-want +got):\n%s", diff)
			}
		})
	}
}
