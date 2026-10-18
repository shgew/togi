//! Rust `cfg` predicates evaluated for the coverage build's host, three-valued: a name the host
//! does not decide, such as a Cargo feature, a custom `--cfg` or `test`, is unknown, never false.

use std::collections::HashSet;

use crate::scan::{is_ident, skip_trivia};

/// What a cfg predicate evaluates to on the host: unknown when another build may decide it either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CfgValue {
    False,
    True,
    Unknown,
}

impl CfgValue {
    fn not(self) -> Self {
        match self {
            Self::True => Self::False,
            Self::False => Self::True,
            Self::Unknown => Self::Unknown,
        }
    }
}

/// The target and compiler names whose absence from `rustc --print cfg` means false for the host.
const WELL_KNOWN: [&str; 24] = [
    "target_os",
    "target_family",
    "target_arch",
    "target_env",
    "target_vendor",
    "target_abi",
    "target_pointer_width",
    "target_endian",
    "target_feature",
    "target_has_atomic",
    "panic",
    "unix",
    "windows",
    "debug_assertions",
    "overflow_checks",
    "coverage",
    "miri",
    "doc",
    "doctest",
    "proc_macro",
    "relocation_model",
    "ub_checks",
    "fmt_debug",
    "target_thread_local",
];

/// The cfg options of the coverage build's target and the names it decides.
#[derive(Clone, Debug)]
pub struct HostCfg {
    set: HashSet<String>,
    known: HashSet<String>,
}

impl HostCfg {
    /// The cfg of options as `rustc --print cfg` prints them: `unix` or `target_os="linux"`.
    pub fn new<'a>(options: impl IntoIterator<Item = &'a str>) -> Self {
        let mut cfg = Self {
            set: HashSet::new(),
            known: WELL_KNOWN.iter().map(|&name| name.to_owned()).collect(),
        };
        for option in options {
            cfg.set.insert(option.to_owned());
            let name = option.split_once('=').map_or(option, |(name, _)| name);
            cfg.known.insert(name.to_owned());
        }
        cfg
    }

    fn option(&self, name: &str, option: &str) -> CfgValue {
        if !self.known.contains(name) {
            CfgValue::Unknown
        } else if self.set.contains(option) {
            CfgValue::True
        } else {
            CfgValue::False
        }
    }

    /// A cfg predicate: a name, `name = "value"`, `true`, `false`, `all(...)`, `any(...)` or
    /// `not(...)`. A name the host decides is true when the host sets it; any other name, an
    /// unsupported operator or a malformed predicate is unknown. `not` of unknown is unknown;
    /// `all` is false if any argument is false, else unknown if any is; `any` is true if any
    /// argument is true, else unknown if any is.
    pub fn eval(&self, predicate: &str) -> CfgValue {
        let Some(tokens) = tokens(predicate.as_bytes()).complete() else {
            return CfgValue::Unknown;
        };
        let mut parser = Parser {
            tokens: &tokens,
            pos: 0,
            cfg: self,
        };
        match parser.predicate() {
            Some(value) if parser.pos == tokens.len() => value,
            Some(_) | None => CfgValue::Unknown,
        }
    }

    /// The attribute whose text between the brackets is `body`, or `None` when it is not a cfg
    /// attribute. A cfg attribute that does not parse is unknown.
    pub fn attribute(&self, body: &[u8]) -> Option<CfgValue> {
        let Tokens { tokens, complete } = tokens(body);
        if tokens.first() != Some(&Token::Ident("cfg".to_owned())) {
            return None;
        }
        if !complete || tokens.get(1) != Some(&Token::Open) {
            return Some(CfgValue::Unknown);
        }
        let mut parser = Parser {
            tokens: &tokens,
            pos: 2,
            cfg: self,
        };
        let Some(value) = parser.predicate() else {
            return Some(CfgValue::Unknown);
        };
        parser.accept(&Token::Comma);
        if !parser.accept(&Token::Close) || parser.pos != tokens.len() {
            return Some(CfgValue::Unknown);
        }
        Some(value)
    }
}

/// The decoded value of a plain `path = "..."` attribute, not one nested in `cfg_attr`.
pub(crate) fn path_attribute(body: &str) -> Option<String> {
    let tokens = tokens(body.as_bytes()).complete()?;
    match tokens.as_slice() {
        [Token::Ident(name), Token::Eq, Token::Str(path)] if name == "path" => Some(path.clone()),
        _ => None,
    }
}

/// One token of a cfg predicate.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    /// An identifier, raw ones without their `r#`.
    Ident(String),
    /// A string literal's decoded value.
    Str(String),
    Open,
    Close,
    Comma,
    Eq,
}

/// The tokens of a predicate up to the first text that is not one, and whether all of it tokenized.
struct Tokens {
    tokens: Vec<Token>,
    complete: bool,
}

impl Tokens {
    fn complete(self) -> Option<Vec<Token>> {
        self.complete.then_some(self.tokens)
    }
}

/// Splits a cfg predicate into tokens, dropping whitespace and comments.
fn tokens(src: &[u8]) -> Tokens {
    let mut tokens = Vec::new();
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    let stop = |tokens| Tokens {
        tokens,
        complete: false,
    };
    let mut i = skip_trivia(src, 0);
    while let Some(&c) = src.get(i) {
        match c {
            b'(' | b')' | b',' | b'=' => {
                tokens.push(match c {
                    b'(' => Token::Open,
                    b')' => Token::Close,
                    b',' => Token::Comma,
                    _ => Token::Eq,
                });
                i += 1;
            }
            b'"' => {
                let Some((value, end)) = cooked_string(src, i + 1) else {
                    return stop(tokens);
                };
                tokens.push(Token::Str(value));
                i = end;
            }
            b'r' if matches!(src.get(i + 1), Some(b'"' | b'#')) => {
                let mut j = i + 1;
                while src.get(j) == Some(&b'#') {
                    j += 1;
                }
                let hashes = j - i - 1;
                match src.get(j) {
                    Some(b'"') => {
                        let closing = closing_quote(hashes);
                        let Some(length) = find(&src[j + 1..], &closing) else {
                            return stop(tokens);
                        };
                        tokens.push(Token::Str(text(&src[j + 1..j + 1 + length])));
                        i = j + 1 + length + closing.len();
                    }
                    Some(&b) if hashes == 1 && is_ident(b) => {
                        let start = j;
                        while src.get(j).is_some_and(|&b| is_ident(b)) {
                            j += 1;
                        }
                        tokens.push(Token::Ident(text(&src[start..j])));
                        i = j;
                    }
                    _ => return stop(tokens),
                }
            }
            _ if is_ident(c) && !c.is_ascii_digit() => {
                let start = i;
                while src.get(i).is_some_and(|&b| is_ident(b)) {
                    i += 1;
                }
                tokens.push(Token::Ident(text(&src[start..i])));
            }
            _ => return stop(tokens),
        }
        i = skip_trivia(src, i);
    }
    Tokens {
        tokens,
        complete: true,
    }
}

/// `"` followed by `hashes` `#`: what closes a raw string opened with that many.
pub(crate) fn closing_quote(hashes: usize) -> Vec<u8> {
    let mut closing = vec![b'"'];
    closing.resize(hashes + 1, b'#');
    closing
}

/// The index of the first occurrence of `needle` in `haystack`.
pub(crate) fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Decodes the string literal whose content starts at `i`: its value and the index after its
/// closing quote, or `None` when it does not close or holds an invalid escape.
fn cooked_string(s: &[u8], mut i: usize) -> Option<(String, usize)> {
    let mut value = Vec::new();
    while let Some(&c) = s.get(i) {
        match c {
            b'"' => return Some((String::from_utf8_lossy(&value).into_owned(), i + 1)),
            b'\\' => {}
            _ => {
                value.push(c);
                i += 1;
                continue;
            }
        }
        i += 1;
        let escape = *s.get(i)?;
        match escape {
            b'n' => value.push(b'\n'),
            b'r' => value.push(b'\r'),
            b't' => value.push(b'\t'),
            b'0' => value.push(0),
            b'\\' | b'"' | b'\'' => value.push(escape),
            b'\n' => {
                while s.get(i + 1).is_some_and(|&b| is_space(b)) {
                    i += 1;
                }
            }
            b'x' => {
                let (hi, lo) = (hex_digit(*s.get(i + 1)?)?, hex_digit(*s.get(i + 2)?)?);
                if hi > 7 {
                    return None;
                }
                value.push(hi << 4 | lo);
                i += 2;
            }
            b'u' => {
                if s.get(i + 1) != Some(&b'{') {
                    return None;
                }
                let end = s[i..].iter().position(|&b| b == b'}')?;
                let mut code: u32 = 0;
                for &d in &s[i + 2..i + end] {
                    if d == b'_' {
                        continue;
                    }
                    code = code << 4 | u32::from(hex_digit(d)?);
                    if code > 0x10_FFFF {
                        return None;
                    }
                }
                let mut buf = [0; 4];
                value.extend_from_slice(char::from_u32(code)?.encode_utf8(&mut buf).as_bytes());
                i += end;
            }
            _ => return None,
        }
        i += 1;
    }
    None
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    cfg: &'a HostCfg,
}

impl Parser<'_> {
    fn accept(&mut self, token: &Token) -> bool {
        if self.tokens.get(self.pos) == Some(token) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// One predicate at the current token, or `None` when the tokens there do not form one.
    fn predicate(&mut self) -> Option<CfgValue> {
        let Some(Token::Ident(name)) = self.tokens.get(self.pos) else {
            return None;
        };
        self.pos += 1;
        if self.accept(&Token::Open) {
            return self.call(name);
        }
        if self.accept(&Token::Eq) {
            let Some(Token::Str(value)) = self.tokens.get(self.pos) else {
                return None;
            };
            self.pos += 1;
            return Some(self.cfg.option(name, &format!("{name}=\"{value}\"")));
        }
        Some(match name.as_str() {
            "true" => CfgValue::True,
            "false" => CfgValue::False,
            _ => self.cfg.option(name, name),
        })
    }

    /// The arguments of `name(...)` after its opening parenthesis, through its closing one.
    fn call(&mut self, name: &str) -> Option<CfgValue> {
        if !matches!(name, "all" | "any" | "not") {
            let mut depth = 1;
            while depth > 0 {
                match self.tokens.get(self.pos)? {
                    Token::Open => depth += 1,
                    Token::Close => depth -= 1,
                    Token::Ident(_) | Token::Str(_) | Token::Comma | Token::Eq => {}
                }
                self.pos += 1;
            }
            return Some(CfgValue::Unknown);
        }
        let mut args = Vec::new();
        while !self.accept(&Token::Close) {
            args.push(self.predicate()?);
            if !self.accept(&Token::Comma) {
                if !self.accept(&Token::Close) {
                    return None;
                }
                break;
            }
        }
        match name {
            "not" => match args.as_slice() {
                [value] => Some(value.not()),
                _ => None,
            },
            "all" => Some(combine(&args, CfgValue::False, CfgValue::True)),
            _ => Some(combine(&args, CfgValue::True, CfgValue::False)),
        }
    }
}

/// `decisive` if any argument is, else unknown if any argument is, else `otherwise`.
fn combine(args: &[CfgValue], decisive: CfgValue, otherwise: CfgValue) -> CfgValue {
    if args.contains(&decisive) {
        decisive
    } else if args.contains(&CfgValue::Unknown) {
        CfgValue::Unknown
    } else {
        otherwise
    }
}

#[cfg(test)]
#[path = "tests/cfg.rs"]
mod tests;
