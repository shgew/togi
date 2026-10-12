use super::{Row, WIDTH, table, wrap};

fn wrapped(first: &str, rest: &str, text: &str) -> String {
    let mut out = String::new();
    wrap(&mut out, first, rest, text);
    out
}

#[test]
fn text_wraps_at_the_width_and_continues_under_the_rest_prefix() {
    let words = vec!["word"; 30].join(" ");
    let out = wrapped("a: ", "  ", &words);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines.len() > 1);
    assert!(lines[0].starts_with("a: word"));
    assert!(lines[1..].iter().all(|line| line.starts_with("  word")));
    assert!(lines.iter().all(|line| line.chars().count() <= WIDTH));
}

#[test]
fn a_word_wider_than_the_room_overflows_instead_of_splitting() {
    let long = "x".repeat(WIDTH + 20);
    let out = wrapped("", "  ", &format!("start {long} end"));
    assert_eq!(out, format!("start\n  {long}\n  end\n"));
}

#[test]
fn leading_newlines_of_the_first_prefix_become_blank_lines() {
    assert_eq!(wrapped("\n\n  ", "    ", "text"), "\n\n  text\n");
    assert_eq!(wrapped("", "", "   "), "\n");
}

#[test]
fn unicode_words_are_kept_whole() {
    let out = wrapped("", "  ", "ünï cödé wörds stay whole 日本語");
    assert_eq!(out, "ünï cödé wörds stay whole 日本語\n");
}

#[test]
fn columns_align_with_two_spaces_and_the_last_is_unpadded() {
    let mut out = String::new();
    table(
        &mut out,
        &[
            Row::new(["A", "BBB", "C"].map(String::from)),
            Row::new(["aaaa", "b", "c"].map(String::from)),
        ],
    );
    assert_eq!(out, "A     BBB  C\naaaa  b    c\n");
}

#[test]
fn notes_wrap_under_their_row_and_do_not_widen_the_table() {
    let long = vec!["note"; 40].join(" ");
    let mut out = String::new();
    table(
        &mut out,
        &[
            Row::new(["A", "B"].map(String::from)),
            Row::new(["x", "y"].map(String::from)).note("label", long),
        ],
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[..2], ["A  B", "x  y"]);
    assert!(lines[2].starts_with("  label note"));
    assert!(
        lines[3..]
            .iter()
            .all(|line| line.starts_with("        note"))
    );
    assert!(lines.iter().all(|line| line.chars().count() <= WIDTH));
}

#[test]
fn a_row_wider_than_the_width_wraps_without_dropping_a_value() {
    let wide = vec!["value"; 20].join(" ");
    let mut out = String::new();
    table(&mut out, &[Row::new(["A".to_owned(), wide.clone()])]);
    assert!(out.lines().all(|line| line.chars().count() <= WIDTH));
    assert_eq!(
        out.split_whitespace().skip(1).collect::<Vec<_>>().join(" "),
        wide
    );
}
