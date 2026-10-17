use test_case::test_case;

use super::*;
use crate::fakes::linux_host;

fn out(src: &str) -> Vec<(u32, u32)> {
    cfg_out(src.as_bytes(), &linux_host())
        .into_iter()
        .map(|range| (range.first, range.last))
        .collect()
}

#[test]
fn items_under_false_and_unknown_predicates() {
    let src = r#"fn kept() {}

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
"#;
    assert_eq!(out(src), [(3, 7), (12, 13), (16, 19), (22, 23)]);
}

#[test_case("struct S {\n    #[cfg(windows)]\n    a: u32,\n    b: u32,\n}\n", &[(2, 3)] ; "a struct field ends at its comma")]
#[test_case("struct S {\n    b: u32,\n    #[cfg(windows)]\n    a: u32\n}\nfn live() {}\n", &[(3, 4)] ; "the last field ends before the closing brace")]
#[test_case("struct S {\n    #[cfg(windows)]\n    a: HashMap<u8, u8>,\n    b: u8,\n}\n", &[(2, 3)] ; "a field with generic arguments ends at the comma after them")]
#[test_case("enum E {\n    #[cfg(windows)]\n    A(u8, u8),\n    B,\n}\n", &[(2, 3)] ; "a tuple variant ends at its comma")]
#[test_case("enum E {\n    #[cfg(windows)]\n    A {\n        x: u8,\n    },\n    B,\n}\n", &[(2, 5)] ; "a struct variant ends at its braces")]
#[test_case("fn f(x: u8) -> u8 {\n    match x {\n        #[cfg(windows)]\n        1 => 10,\n        _ => 0,\n    }\n}\n", &[(3, 4)] ; "a match arm ends at its comma")]
#[test_case("fn f(x: u8) -> u8 {\n    match x {\n        #[cfg(windows)]\n        1 => {\n            10\n        }\n        _ => 0,\n    }\n}\n", &[(3, 6)] ; "a match arm with a block ends at the block")]
#[test_case("fn f() {\n    let v = [\n        #[cfg(windows)]\n        1\n    ];\n    v;\n}\n", &[(3, 4)] ; "an array element ends before the closing bracket")]
#[test_case("fn f(\n    #[cfg(windows)]\n    a: u8\n) {}\n", &[(2, 3)] ; "a function argument ends before the closing parenthesis")]
#[test_case("#[cfg(windows)]\n#[derive(Debug)]\n/// Docs, with a comma.\nstruct A { a: u8 }\nfn live() {}\n", &[(1, 4)] ; "attributes and doc comments before the item belong to it")]
#[test_case("mod m {\n    #![cfg(windows)]\n    fn a() {}\n}\nfn live() {}\n", &[(2, 4)] ; "an inner attribute in an inline module ends at the module's brace")]
#[test_case("fn first() { let x = 1; }\nmod m {\n    fn kept() {}\n    mod n {\n        #![cfg(windows)]\n    }\n    fn live() {}\n}\n", &[(5, 6)] ; "an inner attribute after a closed block still ends at its own module")]
#[test_case("#![cfg(windows)]\nfn a() {}\nfn b() {}\n", &[(1, 3)] ; "an inner attribute at the top level covers the file")]
#[test_case("#[ cfg (\n    windows // not here\n)]\nfn a() {}\n#[cfg( /* gone */ windows , )]\nfn b() {}\n", &[(1, 4), (5, 6)] ; "whitespace newlines and comments inside the attribute")]
#[test_case("#[cfg(unix,)]\nfn a() {}\n#[cfg(all (unix))]\nfn b() {}\n#[cfg(target_os = r#\"linux\"#)]\nfn c() {}\n#[cfg(target_os = \"li\\u{6e}ux\")]\nfn d() {}\n", &[] ; "true predicates of any spelling leave nothing out")]
#[test_case("#[cfg_attr(windows, derive(Debug))]\nstruct A;\n", &[] ; "cfg_attr is not a cfg")]
#[test_case("#[cfg(all(unix,, windows))]\nfn a() {}\n", &[(1, 2)] ; "a predicate that does not parse is left out as unknown")]
#[test_case(
    concat!(
        "const A: &[u8] = b\"#[cfg(windows)] fn f() {}\";\n",
        "const B: &[u8] = br#\"#[cfg(windows)] \"quoted\" fn f() {}\"#;\n",
        "const C: &CStr = c\"#[cfg(windows)]\";\n",
        "const D: &CStr = cr#\"#[cfg(windows)]\"#;\n",
        "const E: &str = r#\"#[cfg(windows)]\"#;\n",
        "const F: &str = \"#[cfg(windows)]\";\n",
    ),
    &[] ;
    "cfg text in byte C and raw strings is not an attribute"
)]
#[test_case("const Q: u8 = b'\"';\nconst R: char = '\"';\nconst S: char = '\\'';\nfn lifetime<'a>(x: &'a str) -> &'a str { x }\n#[cfg(windows)]\nfn gone() {}\n", &[(5, 6)] ; "byte and character literals holding a quote do not open a string")]
#[test_case(
    concat!(
        "const A: &str = stringify!(#[cfg(windows)] fn f() {});\n",
        "macro_rules! m {\n    () => {\n        #[cfg(windows)]\n        fn g() {}\n    };\n}\n",
        "fn live() {}\n",
        "#[cfg(windows)]\nfn gone() {}\n",
    ),
    &[(9, 10)] ;
    "cfg text in macro token trees is not an attribute"
)]
#[test_case("fn f(a: u8, b: bool) -> bool { a != 1 && !b }\n#[cfg(windows)]\nfn gone() {}\n", &[(2, 3)] ; "an item after a not-equal comparison and a negation is still found")]
#[test_case("#[cfg(windows)]\nstruct Gone<T: Iterator<Item=u8>, U> {\n    a: T,\n    b: U,\n}\nfn live() {}\n", &[(1, 5)] ; "an associated type binding inside generics does not end the item at the next generic comma")]
#[test_case("struct S {\n    #[cfg(windows)]\n    f: Result<\n        Box<dyn Iterator<Item=u8>>,\n        String,\n    >,\n    g: u8,\n}\n", &[(2, 6)] ; "a field type with a binding in nested generics ends at the comma after the type")]
#[test_case("#[cfg(windows)]\nstruct D<T = u8,\n    U = u16> {\n    a: T,\n}\nfn live() {}\n", &[(1, 5)] ; "a generic default does not end the item at the next generic comma")]
#[test_case("fn f(x: Option<u8>) -> u8 {\n    match x {\n        #[cfg(windows)]\n        Some(v) if v < 3 => v,\n        _ => 0,\n    }\n}\n", &[(3, 4)] ; "a match arm with a comparison guard ends at its comma")]
#[test_case("const A: &str = stringify /* c */ !(#[cfg(windows)] fn f() {});\n#[cfg(windows)]\nfn gone() {}\n", &[(2, 3)] ; "a block comment between a macro name and its bang does not make its token tree an attribute")]
#[test_case("const A: &str = stringify // c\n    !(#[cfg(windows)] fn f() {});\n#[cfg(windows)]\nfn gone() {}\n", &[(3, 4)] ; "a line comment between a macro name and its bang does not make its token tree an attribute")]
fn item_extent(src: &str, want: &[(u32, u32)]) {
    assert_eq!(out(src), want);
}
