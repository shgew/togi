package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

// linuxHost is the cfg of a Linux host: what `rustc --print cfg` prints there, without the cargo llvm-cov option.
func linuxHost() hostCfg {
	return newHostCfg("unix", `target_os="linux"`, `target_family="unix"`, `target_arch="x86_64"`, "debug_assertions")
}

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

func TestHostFrom(t *testing.T) {
	t.Parallel()
	host, err := hostFrom("rustc 1.90.0\nhost: x86_64-unknown-linux-gnu\nrelease: 1.90.0\n", "unix\ntarget_os=\"linux\"\ntarget_has_atomic_load_store=\"64\"\n")
	if err != nil {
		t.Fatal(err)
	}
	if host.triple != "x86_64-unknown-linux-gnu" {
		t.Errorf("triple = %q", host.triple)
	}
	for predicate, want := range map[string]cfgValue{
		"unix":                                cfgTrue,
		`target_os = "macos"`:                 cfgFalse,
		"coverage":                            cfgTrue,
		"coverage_nightly":                    cfgUnknown,
		"target_has_atomic_load_store":        cfgFalse,
		`target_has_atomic_load_store = "64"`: cfgTrue,
	} {
		if got := evalCfg(predicate, host.cfg); got != want {
			t.Errorf("evalCfg(%q) = %s, want %s", predicate, got, want)
		}
	}
	if _, err := hostFrom("rustc 1.90.0\n", ""); err == nil {
		t.Error("hostFrom accepted version output without a host")
	}
}

func TestEvalCfg(t *testing.T) {
	t.Parallel()
	for predicate, want := range map[string]cfgValue{
		"unix":                               cfgTrue,
		`target_os = "linux"`:                cfgTrue,
		`target_os = "macos"`:                cfgFalse,
		"windows":                            cfgFalse,
		"debug_assertions":                   cfgTrue,
		"miri":                               cfgFalse,
		"test":                               cfgUnknown,
		"not(test)":                          cfgUnknown,
		`feature = "x"`:                      cfgUnknown,
		"my_build":                           cfgUnknown,
		`not(feature = "x")`:                 cfgUnknown,
		`not(target_os = "macos")`:           cfgTrue,
		`not(unix)`:                          cfgFalse,
		`all(unix, target_os = "macos")`:     cfgFalse,
		"all(unix, test)":                    cfgUnknown,
		"all(windows, test)":                 cfgFalse,
		"all(test, windows)":                 cfgFalse,
		"all()":                              cfgTrue,
		"any()":                              cfgFalse,
		`any(target_os = "macos", test)`:     cfgUnknown,
		"any(unix, test)":                    cfgTrue,
		"any(test, unix)":                    cfgTrue,
		"any(windows, my_build)":             cfgUnknown,
		"true":                               cfgTrue,
		"false":                              cfgFalse,
		`all(unix, not(any(windows, miri)))`: cfgTrue,
		`any(all(unix, target_os = "macos"), test)`: cfgUnknown,
		"unix,":         cfgUnknown,
		"all(unix,)":    cfgTrue,
		"any(windows,)": cfgFalse,
		"all (unix)":    cfgTrue,
		"all(\n    unix, // on unix\n    /* and */ target_os = \"linux\",\n)": cfgTrue,
		`target_os = "li\x6eux"`:        cfgTrue,
		`target_os = "li\u{6e}ux"`:      cfgTrue,
		`target_os = r"linux"`:          cfgTrue,
		`target_os = r#"linux"#`:        cfgTrue,
		`target_os = r##"mac"os"##`:     cfgFalse,
		"r#unix":                        cfgTrue,
		"all(unix":                      cfgUnknown,
		"unix windows":                  cfgUnknown,
		`target_os =`:                   cfgUnknown,
		`target_os = linux`:             cfgUnknown,
		`target_os = "linux`:            cfgUnknown,
		"not(unix, windows)":            cfgUnknown,
		"not()":                         cfgUnknown,
		`version("1.80")`:               cfgUnknown,
		`all(windows, version("1.80"))`: cfgFalse,
		"":                              cfgUnknown,
	} {
		if got := evalCfg(predicate, linuxHost()); got != want {
			t.Errorf("evalCfg(%q) = %s, want %s", predicate, got, want)
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
	if diff := cmp.Diff(want, cfgOut([]byte(src), linuxHost()), cmp.AllowUnexported(lineRange{})); diff != "" {
		t.Errorf("cfgOut (-want +got):\n%s", diff)
	}
}

func TestCfgOutItemExtent(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		src  string
		want []lineRange
	}{
		{
			name: "a struct field ends at its comma",
			src:  "struct S {\n    #[cfg(windows)]\n    a: u32,\n    b: u32,\n}\n",
			want: []lineRange{{2, 3}},
		},
		{
			name: "the last field ends before the closing brace",
			src:  "struct S {\n    b: u32,\n    #[cfg(windows)]\n    a: u32\n}\nfn live() {}\n",
			want: []lineRange{{3, 4}},
		},
		{
			name: "a field with generic arguments ends at the comma after them",
			src:  "struct S {\n    #[cfg(windows)]\n    a: HashMap<u8, u8>,\n    b: u8,\n}\n",
			want: []lineRange{{2, 3}},
		},
		{
			name: "a tuple variant ends at its comma",
			src:  "enum E {\n    #[cfg(windows)]\n    A(u8, u8),\n    B,\n}\n",
			want: []lineRange{{2, 3}},
		},
		{
			name: "a struct variant ends at its braces",
			src:  "enum E {\n    #[cfg(windows)]\n    A {\n        x: u8,\n    },\n    B,\n}\n",
			want: []lineRange{{2, 5}},
		},
		{
			name: "a match arm ends at its comma",
			src:  "fn f(x: u8) -> u8 {\n    match x {\n        #[cfg(windows)]\n        1 => 10,\n        _ => 0,\n    }\n}\n",
			want: []lineRange{{3, 4}},
		},
		{
			name: "a match arm with a block ends at the block",
			src:  "fn f(x: u8) -> u8 {\n    match x {\n        #[cfg(windows)]\n        1 => {\n            10\n        }\n        _ => 0,\n    }\n}\n",
			want: []lineRange{{3, 6}},
		},
		{
			name: "an array element ends before the closing bracket",
			src:  "fn f() {\n    let v = [\n        #[cfg(windows)]\n        1\n    ];\n    v;\n}\n",
			want: []lineRange{{3, 4}},
		},
		{
			name: "a function argument ends before the closing parenthesis",
			src:  "fn f(\n    #[cfg(windows)]\n    a: u8\n) {}\n",
			want: []lineRange{{2, 3}},
		},
		{
			name: "attributes and doc comments before the item belong to it",
			src:  "#[cfg(windows)]\n#[derive(Debug)]\n/// Docs, with a comma.\nstruct A { a: u8 }\nfn live() {}\n",
			want: []lineRange{{1, 4}},
		},
		{
			name: "an inner attribute in an inline module ends at the module's brace",
			src:  "mod m {\n    #![cfg(windows)]\n    fn a() {}\n}\nfn live() {}\n",
			want: []lineRange{{2, 4}},
		},
		{
			name: "an inner attribute after a closed block still ends at its own module",
			src:  "fn first() { let x = 1; }\nmod m {\n    fn kept() {}\n    mod n {\n        #![cfg(windows)]\n    }\n    fn live() {}\n}\n",
			want: []lineRange{{5, 6}},
		},
		{
			name: "an inner attribute at the top level covers the file",
			src:  "#![cfg(windows)]\nfn a() {}\nfn b() {}\n",
			want: []lineRange{{1, 3}},
		},
		{
			name: "whitespace, newlines and comments inside the attribute",
			src:  "#[ cfg (\n    windows // not here\n)]\nfn a() {}\n#[cfg( /* gone */ windows , )]\nfn b() {}\n",
			want: []lineRange{{1, 4}, {5, 6}},
		},
		{
			name: "true predicates of any spelling leave nothing out",
			src:  "#[cfg(unix,)]\nfn a() {}\n#[cfg(all (unix))]\nfn b() {}\n#[cfg(target_os = r#\"linux\"#)]\nfn c() {}\n#[cfg(target_os = \"li\\u{6e}ux\")]\nfn d() {}\n",
		},
		{
			name: "cfg_attr is not a cfg",
			src:  "#[cfg_attr(windows, derive(Debug))]\nstruct A;\n",
		},
		{
			name: "a predicate that does not parse is left out as unknown",
			src:  "#[cfg(all(unix,, windows))]\nfn a() {}\n",
			want: []lineRange{{1, 2}},
		},
		{
			name: "cfg text in byte, C and raw strings is not an attribute",
			src: "const A: &[u8] = b\"#[cfg(windows)] fn f() {}\";\n" +
				"const B: &[u8] = br#\"#[cfg(windows)] \"quoted\" fn f() {}\"#;\n" +
				"const C: &CStr = c\"#[cfg(windows)]\";\n" +
				"const D: &CStr = cr#\"#[cfg(windows)]\"#;\n" +
				"const E: &str = r#\"#[cfg(windows)]\"#;\n" +
				"const F: &str = \"#[cfg(windows)]\";\n",
		},
		{
			name: "byte and character literals holding a quote do not open a string",
			src:  "const Q: u8 = b'\"';\nconst R: char = '\"';\nconst S: char = '\\'';\nfn lifetime<'a>(x: &'a str) -> &'a str { x }\n#[cfg(windows)]\nfn gone() {}\n",
			want: []lineRange{{5, 6}},
		},
		{
			name: "cfg text in macro token trees is not an attribute",
			src: "const A: &str = stringify!(#[cfg(windows)] fn f() {});\n" +
				"macro_rules! m {\n    () => {\n        #[cfg(windows)]\n        fn g() {}\n    };\n}\n" +
				"fn live() {}\n" +
				"#[cfg(windows)]\nfn gone() {}\n",
			want: []lineRange{{9, 10}},
		},
		{
			name: "an item after a not-equal comparison and a negation is still found",
			src:  "fn f(a: u8, b: bool) -> bool { a != 1 && !b }\n#[cfg(windows)]\nfn gone() {}\n",
			want: []lineRange{{2, 3}},
		},
		{
			name: "an associated type binding inside generics does not end the item at the next generic comma",
			src:  "#[cfg(windows)]\nstruct Gone<T: Iterator<Item=u8>, U> {\n    a: T,\n    b: U,\n}\nfn live() {}\n",
			want: []lineRange{{1, 5}},
		},
		{
			name: "a field type with a binding in nested generics ends at the comma after the type",
			src:  "struct S {\n    #[cfg(windows)]\n    f: Result<\n        Box<dyn Iterator<Item=u8>>,\n        String,\n    >,\n    g: u8,\n}\n",
			want: []lineRange{{2, 6}},
		},
		{
			name: "a generic default does not end the item at the next generic comma",
			src:  "#[cfg(windows)]\nstruct D<T = u8,\n    U = u16> {\n    a: T,\n}\nfn live() {}\n",
			want: []lineRange{{1, 5}},
		},
		{
			name: "a match arm with a comparison guard ends at its comma",
			src:  "fn f(x: Option<u8>) -> u8 {\n    match x {\n        #[cfg(windows)]\n        Some(v) if v < 3 => v,\n        _ => 0,\n    }\n}\n",
			want: []lineRange{{3, 4}},
		},
		{
			name: "a block comment between a macro name and its bang does not make its token tree an attribute",
			src:  "const A: &str = stringify /* c */ !(#[cfg(windows)] fn f() {});\n#[cfg(windows)]\nfn gone() {}\n",
			want: []lineRange{{2, 3}},
		},
		{
			name: "a line comment between a macro name and its bang does not make its token tree an attribute",
			src:  "const A: &str = stringify // c\n    !(#[cfg(windows)] fn f() {});\n#[cfg(windows)]\nfn gone() {}\n",
			want: []lineRange{{3, 4}},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			if diff := cmp.Diff(tc.want, cfgOut([]byte(tc.src), linuxHost()), cmp.AllowUnexported(lineRange{})); diff != "" {
				t.Errorf("cfgOut (-want +got):\n%s", diff)
			}
		})
	}
}

// writeTree writes files, named relative to root, creating their directories.
func writeTree(t *testing.T, root string, files map[string]string) {
	t.Helper()
	for name, content := range files {
		if err := os.MkdirAll(filepath.Dir(filepath.Join(root, name)), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(root, name), []byte(content), 0o600); err != nil {
			t.Fatal(err)
		}
	}
}

func TestBuiltSources(t *testing.T) {
	t.Parallel()
	root := t.TempDir()
	writeTree(t, root, map[string]string{
		"target/debug/deps/togi_a-1.d":     "target/debug/deps/togi_a-1: crates/togi-a/src/lib.rs\n\ncrates/togi-a/src/lib.rs:\n",
		"target/debug/build/togi-a-2/b.d":  "target/debug/build/togi-a-2/b: crates/togi-a/build.rs\n\ncrates/togi-a/build.rs:\n",
		"target/debug/deps/togi_b-3.d":     "crates/with\\ space/src/lib.rs:\n" + root + "/crates/togi-b/src/lib.rs:\n/elsewhere/dep.rs:\n",
		"target/debug/incremental/other.d": "crates/togi-c/src/lib.rs:\n",
	})
	built, err := builtSources(filepath.Join(root, "target/debug"), root)
	if err != nil {
		t.Fatal(err)
	}
	want := map[string]bool{
		"crates/togi-a/src/lib.rs":     true,
		"crates/togi-a/build.rs":       true,
		"crates/with space/src/lib.rs": true,
		"crates/togi-b/src/lib.rs":     true,
	}
	if diff := cmp.Diff(want, built); diff != "" {
		t.Errorf("built (-want +got):\n%s", diff)
	}
	if _, err := builtSources(filepath.Join(root, "empty"), root); err == nil {
		t.Error("builtSources accepted a directory with no dep-info files")
	}
}

func TestRustNotBuilt(t *testing.T) {
	t.Parallel()
	const lib = "crates/a/src/lib.rs"
	for _, tc := range []struct {
		name     string
		src      string
		changed  []lineRange
		measured map[int]bool
		want     []string
	}{
		{
			name:     "an item whose cfg is false and that the trace does not measure",
			src:      "fn a() {}\n\n#[cfg(windows)]\nfn win() {\n    1;\n}\n",
			changed:  []lineRange{{5, 5}},
			measured: map[int]bool{1: true},
			want:     []string{lib},
		},
		{
			name:     "a changed line outside the item",
			src:      "fn a() {}\n\n#[cfg(windows)]\nfn win() {\n    1;\n}\n",
			changed:  []lineRange{{1, 1}},
			measured: map[int]bool{1: true},
		},
		{
			name:     "a false cfg item holding a measured line was compiled by some build",
			src:      "fn a() {}\n\n#[cfg(windows)]\nfn win() {\n    1;\n}\n",
			changed:  []lineRange{{4, 5}},
			measured: map[int]bool{1: true, 5: false},
		},
		{
			name:     "a cargo feature item with no measured line",
			src:      "#[cfg(feature = \"x\")]\nconst X: u8 = 1;\nfn a() {}\n",
			changed:  []lineRange{{2, 2}},
			measured: map[int]bool{3: true},
			want:     []string{lib},
		},
		{
			name:     "a cargo feature item with a measured line",
			src:      "#[cfg(feature = \"x\")]\nfn x() {\n    1;\n}\nfn a() {}\n",
			changed:  []lineRange{{3, 3}},
			measured: map[int]bool{3: true, 5: true},
		},
		{
			name:     "a custom cfg item with a measured line",
			src:      "#[cfg(my_build)]\nfn x() {\n    1;\n}\n",
			changed:  []lineRange{{3, 3}},
			measured: map[int]bool{3: false},
		},
		{
			name:     "cfg(not(test)) code the unit test build compiles out but another build compiles",
			src:      "#[cfg(not(test))]\nfn release() {\n    1;\n}\n",
			changed:  []lineRange{{3, 3}},
			measured: map[int]bool{3: true},
		},
		{
			name:     "cfg(test) code is not claimed when the test build measures it",
			src:      "#[cfg(test)]\nmod tests {\n    fn t() {\n        1;\n    }\n}\n",
			changed:  []lineRange{{4, 4}},
			measured: map[int]bool{4: true},
		},
		{
			name:     "a changed field the build leaves out, beside a live field",
			src:      "struct S {\n    #[cfg(windows)]\n    a: u32,\n    b: u32,\n}\nfn live() {\n    1;\n}\n",
			changed:  []lineRange{{3, 3}},
			measured: map[int]bool{7: true},
			want:     []string{lib},
		},
		{
			name:     "a changed live field after a cfg field",
			src:      "struct S {\n    #[cfg(windows)]\n    a: u32,\n    b: u32,\n}\nfn live() {\n    1;\n}\n",
			changed:  []lineRange{{4, 4}, {7, 7}},
			measured: map[int]bool{7: true},
		},
		{
			name:     "an inline module's inner cfg does not reach the code after the module",
			src:      "mod m {\n    #![cfg(windows)]\n    pub fn a() {}\n}\nfn live() {\n    1;\n}\n",
			changed:  []lineRange{{6, 6}},
			measured: map[int]bool{6: true},
		},
		{
			name:     "a cfg inside a macro token tree followed by a measured function",
			src:      "macro_rules! m {\n    () => {\n        #[cfg(windows)]\n        fn g() {}\n    };\n}\nfn live() {\n    1;\n}\n",
			changed:  []lineRange{{4, 8}},
			measured: map[int]bool{8: true},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			root := t.TempDir()
			writeTree(t, root, map[string]string{lib: tc.src})
			got, err := rustNotBuilt(root, map[string][]lineRange{lib: tc.changed}, map[string]bool{lib: true}, map[string]map[int]bool{lib: tc.measured}, linuxHost())
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Errorf("not built (-want +got):\n%s", diff)
			}
		})
	}
}

func TestRustNotBuiltNamesFilesNoDepInfoNames(t *testing.T) {
	t.Parallel()
	root := t.TempDir()
	writeTree(t, root, map[string]string{
		"crates/a/src/lib.rs":      "fn a() {}\n",
		"crates/a/src/mac.rs":      "fn only_mac() {}\n",
		"crates/a/src/untested.rs": "fn u() {}\n",
	})
	changed := map[string][]lineRange{
		"crates/a/src/lib.rs":      {{1, 1}},
		"crates/a/src/mac.rs":      {{1, 1}},
		"crates/a/src/untested.rs": {{1, 1}},
	}
	built := map[string]bool{"crates/a/src/lib.rs": true}
	measured := map[string]map[int]bool{"crates/a/src/mac.rs": {1: true}}
	got, err := rustNotBuilt(root, changed, built, measured, linuxHost())
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]string{"crates/a/src/mac.rs", "crates/a/src/untested.rs"}, got); diff != "" {
		t.Errorf("not built (-want +got):\n%s", diff)
	}
}
