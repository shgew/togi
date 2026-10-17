//! A lexical scan of Rust source for the items under a `cfg` attribute the host does not satisfy.
//! It skips comments and literals and tracks brackets; it does not parse Rust. `cfg_attr` is not
//! a cfg: an item it gates by a nested `cfg` stays in.

use crate::cfg::{CfgValue, HostCfg, closing_quote, find};
use crate::diff::LineRange;

/// The line ranges of the items in `src` under a `#[cfg(...)]` or `#![cfg(...)]` predicate that is
/// false or unknown under `cfg`. An outer attribute's item ends at its first semicolon or comma
/// outside brackets, before the bracket closing around it, or at the end of its first brace block.
/// An inner attribute's item ends at the brace closing the block holding it, or at the end of the
/// file. Attributes inside macro invocations and `macro_rules` bodies are tokens, not attributes,
/// and are skipped.
pub fn cfg_out(src: &[u8], cfg: &HostCfg) -> Vec<LineRange> {
    let mut out = Vec::new();
    let line_of = |i: usize| {
        let lines = src[..i].split(|&b| b == b'\n').count();
        u32::try_from(lines).unwrap_or(u32::MAX)
    };
    let mut blocks: Vec<usize> = Vec::new();
    // The index of the last byte outside whitespace and comments.
    let mut last: Option<usize> = None;
    let mut i = 0;
    while i < src.len() {
        if let Some(next) = skip_literal(src, i) {
            if src[i] != b'/' {
                last = Some(next - 1);
            }
            i = next;
            continue;
        }
        if is_space(src[i]) {
            i += 1;
            continue;
        }
        let prev = last;
        last = Some(i);
        match src[i] {
            b'{' => blocks.push(i),
            b'}' => {
                blocks.pop();
            }
            b'!' => {
                if let Some(group) = macro_group(src, prev, i) {
                    let Some(end) = matching(src, group) else {
                        return out;
                    };
                    i = end;
                    last = Some(end);
                }
            }
            b'#' => {
                let mut open = skip_space(src, i + 1);
                let inner = src.get(open) == Some(&b'!');
                if inner {
                    open = skip_space(src, open + 1);
                }
                if src.get(open) == Some(&b'[') {
                    let Some(end) = matching(src, open) else {
                        return out;
                    };
                    let start = i;
                    i = end;
                    last = Some(end);
                    if let Some(CfgValue::False | CfgValue::Unknown) =
                        cfg.attribute(&src[open + 1..end])
                    {
                        let mut item_end = if !inner {
                            item_end(src, end + 1)
                        } else if let Some(&block) = blocks.last() {
                            matching(src, block).map_or(src.len(), |close| close + 1)
                        } else {
                            src.len()
                        };
                        while item_end > end + 1 && is_space(src[item_end - 1]) {
                            item_end -= 1;
                        }
                        out.push(LineRange::new(line_of(start), line_of(item_end - 1)));
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// The index of the bracket opening the token tree of the macro invocation or `macro_rules`
/// definition whose `!` is at `bang`. `last` is the index of the last byte before `bang` outside
/// whitespace and comments.
fn macro_group(src: &[u8], last: Option<usize>, bang: usize) -> Option<usize> {
    let end = last? + 1;
    let mut start = end;
    while start > 0 && is_ident(src[start - 1]) {
        start -= 1;
    }
    if start == end || src[start].is_ascii_digit() {
        return None;
    }
    let name = &src[start..end];
    if matches!(
        name,
        b"if"
            | b"while"
            | b"match"
            | b"return"
            | b"break"
            | b"in"
            | b"else"
            | b"let"
            | b"mut"
            | b"move"
            | b"as"
            | b"yield"
    ) {
        return None;
    }
    let mut k = skip_trivia(src, bang + 1);
    if name == b"macro_rules" {
        let ident = k;
        while src.get(k).is_some_and(|&b| is_ident(b)) {
            k += 1;
        }
        if k == ident {
            return None;
        }
        k = skip_trivia(src, k);
    }
    matches!(src.get(k), Some(b'(' | b'[' | b'{')).then_some(k)
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

fn skip_space(src: &[u8], mut i: usize) -> usize {
    while src.get(i).is_some_and(|&b| is_space(b)) {
        i += 1;
    }
    i
}

/// The index of the first byte at or after `i` that is neither whitespace nor in a comment.
pub(crate) fn skip_trivia(src: &[u8], mut i: usize) -> usize {
    while let Some(&b) = src.get(i) {
        if is_space(b) {
            i += 1;
        } else if b == b'/' && matches!(src.get(i + 1), Some(b'/' | b'*')) {
            i = skip_literal(src, i).unwrap_or(src.len());
        } else {
            break;
        }
    }
    i
}

/// The index after the comment, string, byte string, C string or character literal starting at
/// `i`, or `None` when none starts there. A lifetime is not a literal.
fn skip_literal(src: &[u8], i: usize) -> Option<usize> {
    match &src[i..src.len().min(i + 2)] {
        b"//" => {
            return Some(
                src[i..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(src.len(), |end| i + end),
            );
        }
        b"/*" => {
            let mut depth = 0;
            let mut j = i;
            while j + 1 < src.len() {
                match &src[j..j + 2] {
                    b"/*" => {
                        depth += 1;
                        j += 1;
                    }
                    b"*/" => {
                        depth -= 1;
                        j += 1;
                        if depth == 0 {
                            return Some(j + 1);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            return Some(src.len());
        }
        _ => {}
    }
    match src[i] {
        b'"' => return Some(end_of_string(src, i + 1)),
        b'\'' => return end_of_char(src, i),
        _ if i > 0 && is_ident(src[i - 1]) => return None,
        _ => {}
    }
    let mut j = i;
    if matches!(src[j], b'b' | b'c') {
        j += 1;
        match src.get(j) {
            Some(b'"') => return Some(end_of_string(src, j + 1)),
            Some(b'\'') if src[i] == b'b' => return end_of_char(src, j),
            _ => {}
        }
    }
    if src.get(j) == Some(&b'r') {
        j += 1;
        let hashes_start = j;
        while src.get(j) == Some(&b'#') {
            j += 1;
        }
        if src.get(j) == Some(&b'"') {
            return Some(end_of_raw_string(src, j + 1, j - hashes_start));
        }
    }
    None
}

/// The index after the character literal whose quote is at `i`, or `None` for a lifetime or label.
fn end_of_char(src: &[u8], i: usize) -> Option<usize> {
    let &next = src.get(i + 1)?;
    if next == b'\\' {
        let from = src.len().min(i + 3);
        return Some(
            src[from..]
                .iter()
                .position(|&b| b == b'\'')
                .map_or(src.len(), |end| from + end + 1),
        );
    }
    let size = src[i + 1..]
        .utf8_chunks()
        .next()
        .and_then(|chunk| chunk.valid().chars().next())
        .map_or(1, char::len_utf8);
    (src.get(i + 1 + size) == Some(&b'\'')).then_some(i + 2 + size)
}

/// Whether `b` can be part of an identifier; every byte of a non-ASCII character counts.
pub(crate) fn is_ident(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric() || !b.is_ascii()
}

fn end_of_string(src: &[u8], mut i: usize) -> usize {
    while let Some(&b) = src.get(i) {
        match b {
            b'\\' => i += 1,
            b'"' => return i + 1,
            _ => {}
        }
        i += 1;
    }
    src.len()
}

fn end_of_raw_string(src: &[u8], i: usize, hashes: usize) -> usize {
    let closing = closing_quote(hashes);
    find(&src[i..], &closing).map_or(src.len(), |end| i + end + closing.len())
}

/// The index of the bracket closing the one at `open`.
fn matching(src: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0_usize;
    let mut i = open;
    while i < src.len() {
        if let Some(next) = skip_literal(src, i) {
            i = next;
            continue;
        }
        match src[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Where angle brackets stand in an item: counting generics, or past an `=`, a comparison or a
/// `=>`, after which `<` and `>` are operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Angles {
    Generics(usize),
    Operators,
}

/// The index after the item starting at `i`: after its first semicolon or comma outside brackets
/// and generics, at the unmatched bracket closing around it, or after its first brace block. An
/// `=` inside generics binds an associated type or a default, as in `Iterator<Item = u8>`.
fn item_end(src: &[u8], mut i: usize) -> usize {
    let mut depth = 0_usize;
    let mut angles = Angles::Generics(0);
    while i < src.len() {
        if let Some(next) = skip_literal(src, i) {
            i = next;
            continue;
        }
        match src[i] {
            b'(' | b'[' => depth += 1,
            b'{' if depth == 0 => {
                return matching(src, i).map_or(src.len(), |end| end + 1);
            }
            b'{' => depth += 1,
            b')' | b']' | b'}' => {
                if depth == 0 {
                    return i;
                }
                depth -= 1;
            }
            b'<' if depth == 0 => {
                if let Angles::Generics(n) = angles {
                    angles = Angles::Generics(n + 1);
                }
            }
            b'>' if depth == 0 && src[i - 1] != b'-' => {
                if let Angles::Generics(n) = angles {
                    angles = Angles::Generics(n.saturating_sub(1));
                }
            }
            b'=' if depth == 0 => {
                let operator = matches!(src[i - 1], b'=' | b'<' | b'>' | b'!')
                    || matches!(src.get(i + 1), Some(b'=' | b'>'));
                if operator || angles == Angles::Generics(0) {
                    angles = Angles::Operators;
                }
            }
            b';' if depth == 0 => return i + 1,
            b',' if depth == 0 && matches!(angles, Angles::Generics(0) | Angles::Operators) => {
                return i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    src.len()
}

#[cfg(test)]
#[path = "tests/scan.rs"]
mod tests;
