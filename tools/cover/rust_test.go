package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestParseLcovUnreachedRanges(t *testing.T) {
	t.Parallel()
	trace := `TN:
SF:/repo/crates/togi-a/src/lib.rs
FN:3,f
DA:3,1
DA:4,0
DA:5,0
DA:7,0
DA:8,2
DA:10,0
DA:10,3
end_of_record
SF:/elsewhere/dep.rs
DA:1,0
end_of_record
`
	files, err := parseLcov(strings.NewReader(trace), "/repo")
	if err != nil {
		t.Fatal(err)
	}
	want := map[string][]lineRange{"crates/togi-a/src/lib.rs": {{4, 7}}}
	if diff := cmp.Diff(want, unreached(files), cmp.AllowUnexported(lineRange{})); diff != "" {
		t.Errorf("unreached (-want +got):\n%s", diff)
	}
}

func TestParseLcovRejectsMalformedLine(t *testing.T) {
	t.Parallel()
	if _, err := parseLcov(strings.NewReader("SF:/repo/a.rs\nDA:x,1\n"), "/repo"); err == nil {
		t.Error("parseLcov accepted a malformed DA line")
	}
}

func TestEvalCfg(t *testing.T) {
	t.Parallel()
	cfg := map[string]bool{"unix": true, `target_os="linux"`: true, "test": true}
	for predicate, want := range map[string]bool{
		"unix":                           true,
		`target_os = "linux"`:            true,
		`target_os = "macos"`:            false,
		"test":                           true,
		"feature_x":                      false,
		`not(target_os = "macos")`:       true,
		`all(unix, target_os = "macos")`: false,
		`any(target_os = "macos", test)`: true,
		`all(unix, not(any(windows, feature_x)))`:   true,
		`any(all(unix, target_os = "macos"), test)`: true,
	} {
		if got := evalCfg(predicate, cfg); got != want {
			t.Errorf("evalCfg(%q) = %t, want %t", predicate, got, want)
		}
	}
}

func TestCfgOut(t *testing.T) {
	t.Parallel()
	src := `fn kept() {}

#[cfg(target_os = "macos")]
fn mac() {
    let s = "}{";
    // } in a comment
}

#[cfg(target_os = "linux")]
fn linux() {}

#[cfg(not(test))]
const RELEASE: u32 = 1;

#[derive(Debug)]
#[cfg(feature = "x")]
struct Gated {
    a: u32,
}

mod m {
    #[cfg(windows)]
    use std::os::windows;
}
`
	want := []lineRange{{3, 7}, {12, 13}, {16, 19}, {22, 23}}
	cfg := map[string]bool{"unix": true, `target_os="linux"`: true, "test": true}
	if diff := cmp.Diff(want, cfgOut([]byte(src), cfg), cmp.AllowUnexported(lineRange{})); diff != "" {
		t.Errorf("cfgOut (-want +got):\n%s", diff)
	}
}

func TestCfgOutInnerAttributeCoversTheFile(t *testing.T) {
	t.Parallel()
	src := "#![cfg(windows)]\nfn a() {}\nfn b() {}\n"
	if diff := cmp.Diff([]lineRange{{1, 3}}, cfgOut([]byte(src), map[string]bool{"unix": true}), cmp.AllowUnexported(lineRange{})); diff != "" {
		t.Errorf("cfgOut (-want +got):\n%s", diff)
	}
}

func TestReportRustNamesWhatTheBuildLeavesOut(t *testing.T) {
	root := t.TempDir()
	deps := filepath.Join(root, "target", "deps")
	for name, content := range map[string]string{
		"crates/togi-a/src/lib.rs":      "fn a() {}\n\n#[cfg(target_os = \"macos\")]\nfn mac() {\n    1;\n}\n",
		"crates/togi-a/src/mac.rs":      "fn only_mac() {}\n",
		"target/deps/togi_a-1234.d":     "crates/togi-a/src/lib.rs: crates/togi-a/src/lib.rs\n\ncrates/togi-a/src/lib.rs:\n",
		"crates/togi-a/src/untested.rs": "fn u() {}\n",
	} {
		if err := os.MkdirAll(filepath.Dir(filepath.Join(root, name)), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(root, name), []byte(content), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	built, err := builtSources(deps, root)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(map[string]bool{"crates/togi-a/src/lib.rs": true}, built); diff != "" {
		t.Errorf("built (-want +got):\n%s", diff)
	}
	changed := map[string][]lineRange{
		"crates/togi-a/src/lib.rs":      {{1, 1}},
		"crates/togi-a/src/mac.rs":      {{1, 1}},
		"crates/togi-a/src/untested.rs": {{1, 1}},
	}
	cfg := map[string]bool{"unix": true, `target_os="linux"`: true}
	got, err := rustNotBuilt(root, changed, built, cfg)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]string{"crates/togi-a/src/mac.rs", "crates/togi-a/src/untested.rs"}, got); diff != "" {
		t.Errorf("not built (-want +got):\n%s", diff)
	}
	changed["crates/togi-a/src/lib.rs"] = []lineRange{{5, 5}}
	got, err = rustNotBuilt(root, changed, built, cfg)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(strings.Join(got, " "), "crates/togi-a/src/lib.rs") {
		t.Errorf("not built = %v, want lib.rs for a changed line inside its macos item", got)
	}
}
