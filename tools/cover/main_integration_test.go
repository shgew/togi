//go:build integration

package main

import (
	"bytes"
	"go/build"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

// repository makes a temporary Git repository the working directory and returns helpers that run Git and write files in it.
func repository(t *testing.T) (git func(args ...string), writeFile func(name, content string)) {
	t.Helper()
	dir := t.TempDir()
	t.Chdir(dir)
	t.Setenv("GIT_CONFIG_NOSYSTEM", "1")
	t.Setenv("GIT_CONFIG_GLOBAL", "/dev/null")
	t.Setenv("GIT_AUTHOR_NAME", "Coverage Test")
	t.Setenv("GIT_AUTHOR_EMAIL", "coverage@example.com")
	t.Setenv("GIT_COMMITTER_NAME", "Coverage Test")
	t.Setenv("GIT_COMMITTER_EMAIL", "coverage@example.com")
	git = func(args ...string) {
		t.Helper()
		if out, err := exec.Command("git", args...).CombinedOutput(); err != nil {
			t.Fatalf("git %s: %v: %s", strings.Join(args, " "), err, out)
		}
	}
	writeFile = func(name, content string) {
		t.Helper()
		if err := os.WriteFile(filepath.Join(dir, name), []byte(content), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	git("init", "--initial-branch=main")
	writeFile("go.mod", "module example.com/m\n")
	return git, writeFile
}

func TestRunIgnoresConfiguredInterHunkContext(t *testing.T) {
	git, writeFile := repository(t)
	writeFile("a.go", "package a\n\nfunc f() {\n\tx := 1\n\ty := 2\n\tz := 3\n\t_, _, _ = x, y, z\n}\n")
	git("add", ".")
	git("commit", "-m", "Base")
	git("config", "diff.interHunkContext", "10")
	writeFile("a.go", "package a\n\nfunc f() {\n\tx := 4\n\ty := 2\n\tz := 5\n\t_, _, _ = x, y, z\n}\n")
	writeFile("profile", "mode: set\nexample.com/m/a.go:4.1,6.8 3 0\n")
	var out, errOut bytes.Buffer
	if err := run([]string{"--profile", "profile", "--base", "HEAD"}, &out, &errOut); err != nil {
		t.Fatalf("run: %v: %s", err, errOut.String())
	}
	if diff := cmp.Diff("a.go:4\na.go:6\n", out.String()); diff != "" {
		t.Errorf("ranges (-want +got):\n%s", diff)
	}
}

func TestRunNamesChangedFilesTheBuildLeavesOut(t *testing.T) {
	git, writeFile := repository(t)
	writeFile("a.go", "package a\n\nfunc f() int {\n\treturn 1\n}\n")
	writeFile("hardware.go", "//go:build hardware\n\npackage a\n\nfunc g() int {\n\treturn 1\n}\n")
	writeFile("integration.go", "//go:build integration\n\npackage a\n\nfunc h() int {\n\treturn 1\n}\n")
	git("add", ".")
	git("commit", "-m", "Base")
	writeFile("a.go", "package a\n\nfunc f() int {\n\treturn 2\n}\n")
	writeFile("hardware.go", "//go:build hardware\n\npackage a\n\nfunc g() int {\n\treturn 2\n}\n")
	writeFile("integration.go", "//go:build integration\n\npackage a\n\nfunc h() int {\n\treturn 2\n}\n")
	writeFile("profile", "mode: set\nexample.com/m/a.go:3.14,5.2 1 1\nexample.com/m/integration.go:5.14,7.2 1 1\n")
	var out, errOut bytes.Buffer
	if err := run([]string{"--profile", "profile", "--base", "HEAD", "--tags", "integration"}, &out, &errOut); err != nil {
		t.Fatalf("run: %v: %s", err, errOut.String())
	}
	want := "Every changed Go line the profile measures ran in a test since HEAD.\n" +
		"hardware.go: not built for " + build.Default.GOOS + "/" + build.Default.GOARCH + " with tags integration\n"
	if diff := cmp.Diff(want, out.String()); diff != "" {
		t.Errorf("report (-want +got):\n%s", diff)
	}
}
