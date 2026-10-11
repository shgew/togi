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

// TestReportRustReportsLinesAndFilesTheBuildLeavesOut gives reportRust a host of its own, as the flake's package check runs this tag without rustc.
func TestReportRustReportsLinesAndFilesTheBuildLeavesOut(t *testing.T) {
	git, writeFile := repository(t)
	for _, dir := range []string{"crates/a/src", "target/debug/deps", "target/debug/build/a-1"} {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	writeFile("crates/a/src/lib.rs", "fn a() -> u32 {\n    1\n}\n\n#[cfg(target_os = \"nowhere\")]\nfn gone() -> u32 {\n    1\n}\n")
	writeFile("crates/a/build.rs", "fn main() {\n    let _ = 1;\n}\n")
	writeFile("target/debug/deps/a.d", "crates/a/src/lib.rs:\n")
	writeFile("target/debug/build/a-1/build_script_build.d", "crates/a/build.rs:\n")
	git("add", ".")
	git("commit", "-m", "Base")
	writeFile("crates/a/src/lib.rs", "fn a() -> u32 {\n    2\n}\n\n#[cfg(target_os = \"nowhere\")]\nfn gone() -> u32 {\n    2\n}\n\nfn b() {\n    todo!();\n}\n")
	writeFile("crates/a/build.rs", "fn main() {\n    let _ = 2;\n}\n")
	root, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	writeFile("trace.info", "SF:"+root+"/crates/a/src/lib.rs\nDA:1,1\nDA:2,1\nDA:9,1\nDA:10,0\nend_of_record\nSF:"+root+"/crates/a/build.rs\nDA:1,1\nDA:2,1\nend_of_record\n")
	trace, err := os.Open("trace.info")
	if err != nil {
		t.Fatal(err)
	}
	defer trace.Close()
	host := rustHost{triple: "test-host", cfg: newHostCfg("unix", `target_os="linux"`)}
	var out bytes.Buffer
	if err := reportRust(trace, filepath.Join(root, "target/debug"), root, "HEAD", host, &out); err != nil {
		t.Fatalf("reportRust: %v", err)
	}
	want := "crates/a/src/lib.rs:10\ncrates/a/src/lib.rs: not built for test-host\n"
	if diff := cmp.Diff(want, out.String()); diff != "" {
		t.Errorf("report (-want +got):\n%s", diff)
	}
}
