//! Quoting, JSON and path spelling the review records and messages have always used.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// `s` in double quotes, with quotes, backslashes and unprintable characters escaped.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{7}' => out.push_str("\\a"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{b}' => out.push_str("\\v"),
            ' ' => out.push(' '),
            c if c.is_control() || c.is_whitespace() => {
                let code = u32::from(c);
                if code < 0x80 {
                    let _ = write!(out, "\\x{code:02x}");
                } else if code < 0x1_0000 {
                    let _ = write!(out, "\\u{code:04x}");
                } else {
                    let _ = write!(out, "\\U{code:08x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The double-quoted string at the start of `s`, decoded, and the length of its quoted form; `None` when
/// `s` does not start with a valid one.
pub fn unquote_prefix(s: &[u8]) -> Option<(Vec<u8>, usize)> {
    if s.first() != Some(&b'"') {
        return None;
    }
    let mut out = Vec::new();
    let mut i = 1;
    loop {
        let byte = *s.get(i)?;
        i += 1;
        match byte {
            b'"' => return Some((out, i)),
            b'\n' => return None,
            b'\\' => {
                let escape = *s.get(i)?;
                i += 1;
                match escape {
                    b'a' => out.push(7),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'v' => out.push(11),
                    b'\\' | b'"' => out.push(escape),
                    b'x' => {
                        let digits = s.get(i..i + 2)?;
                        i += 2;
                        if !digits.iter().all(u8::is_ascii_hexdigit) {
                            return None;
                        }
                        out.push(u8::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()?);
                    }
                    b'0'..=b'7' => {
                        let digits = s.get(i - 1..i + 2)?;
                        i += 2;
                        if !digits.iter().all(|d| (b'0'..=b'7').contains(d)) {
                            return None;
                        }
                        let value =
                            u16::from_str_radix(std::str::from_utf8(digits).ok()?, 8).ok()?;
                        out.push(u8::try_from(value).ok()?);
                    }
                    b'u' | b'U' => {
                        let n = if escape == b'u' { 4 } else { 8 };
                        let digits = s.get(i..i + n)?;
                        i += n;
                        let digits = std::str::from_utf8(digits).ok()?;
                        if !digits.bytes().all(|d| d.is_ascii_hexdigit()) {
                            return None;
                        }
                        let rune = char::from_u32(u32::from_str_radix(digits, 16).ok()?)?;
                        out.extend_from_slice(rune.encode_utf8(&mut [0; 4]).as_bytes());
                    }
                    _ => return None,
                }
            }
            other => out.push(other),
        }
    }
}

/// `s` decoded when it is exactly one valid double-quoted string.
pub fn unquote(s: &[u8]) -> Option<Vec<u8>> {
    let (out, n) = unquote_prefix(s)?;
    (n == s.len()).then_some(out)
}

/// `value` as compact JSON, with `<`, `>`, `&`, U+2028 and U+2029 escaped as the records always had them.
pub fn json<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    Ok(escape_json(&serde_json::to_string(value)?))
}

/// `value` as JSON indented by two spaces, escaped as `json` does.
pub fn json_indented<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    Ok(escape_json(&serde_json::to_string_pretty(value)?))
}

fn escape_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out
}

/// A JSON field that reads `null` as the field's empty value.
pub fn nullable<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// The shortest path naming the same file as `path`, by lexical processing alone.
pub fn clean(path: &str) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }
    let rooted = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else if !rooted {
                    parts.push("..");
                }
            }
            part => parts.push(part),
        }
    }
    let joined = parts.join("/");
    match (rooted, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".to_owned(),
        (false, false) => joined,
    }
}

/// `dir` and `name` joined and cleaned.
pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        return clean(name);
    }
    clean(&format!("{dir}/{name}"))
}

/// The last element of `path` once cleaned.
pub fn base(path: &str) -> String {
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        return "/".to_owned();
    }
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

/// `segment` escaped for one segment of a URL path.
pub fn path_escape(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~$&+:=@".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// The first eight characters of a commit SHA.
pub fn short(sha: &str) -> &str {
    sha.char_indices().nth(8).map_or(sha, |(i, _)| &sha[..i])
}

/// `bytes` as text, invalid UTF-8 replaced.
pub fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
