//! The Markdown the exporter writes: the phrase catalogue, the forwarded journal messages, the manifest of source
//! phrases with the cases that rendered them, and the `togi status` corpus.
//!
//! The catalogue is a reading aid, not the raw record. [`catalogue`] reduces the templates the collector kept, per
//! producer, in two steps that never invent a phrase:
//!
//! 1. [`fold`] takes a template that is only a cut of another into it: a truncation (`so a freeze needs a...`), a
//!    wrapped row or a column cut. The forms, cases and texts of the cut stay on the full template. A truncation no
//!    other template contains stays, and says so.
//! 2. [`alternatives`] merges templates that differ in one run of one to three tokens, and agree on every token
//!    around it, into `before {a | b} after`. Each alternative was rendered in exactly that context; two positions
//!    never vary at once, so a combination nobody rendered is never implied.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::fmt::Write as _;

use super::manifest::{self, SILENT};
use super::{Cases, Collector, Item, MESSAGES, Provenance, TestRun};
use crate::frames::corpus::Swept;

/// How much of a source literal the manifest shows.
const LITERAL_WIDTH: usize = 100;

/// The fewest characters of a truncation before its `...` that are looked up in the other templates.
const MIN_CUT: usize = 6;

/// The fewest characters of a template that is folded into a longer one as a part of it.
const MIN_PART: usize = 16;

/// The most tokens a merged group lets vary.
const MAX_WINDOW: usize = 3;

/// The fewest letters the tokens around a merged group's alternatives hold.
const MIN_CONTEXT_LETTERS: usize = 4;

/// `text` as inline code, whatever backticks it holds.
fn code(text: &str) -> String {
    let longest = text
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let fence = "`".repeat(longest + 1);
    let text = text.replace(['\n', '\r'], " ");
    format!("{fence} {text} {fence}")
}

fn cases(label: &str, cases: &Cases) -> Option<String> {
    if cases.renders == 0 {
        return None;
    }
    let names: Vec<&str> = cases.names.iter().map(String::as_str).collect();
    Some(format!(
        "{label} {} ({} renders)",
        names.join(", "),
        cases.renders
    ))
}

/// Where a phrase was rendered, as one line.
fn provenance(provenance: &Provenance) -> String {
    let parts: Vec<String> = [
        cases("tests", &provenance.tests),
        cases("sweep", &provenance.sweep),
    ]
    .into_iter()
    .flatten()
    .collect();
    parts.join("; ")
}

fn truncated(text: &str) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(LITERAL_WIDTH).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// One line of the catalogue: a template, what was folded or merged into it, and everything that rendered it.
#[derive(Debug)]
struct Entry {
    template: String,
    item: Item,
    /// Templates folded in because they were cuts of this one.
    folded: usize,
    /// Templates merged into this one's alternatives; zero when it has none.
    merged: usize,
    /// Whether it ends in a truncation mark and no other template holds it whole.
    cut: bool,
}

/// What of `template` is left before a trailing truncation mark, when it has one.
fn cut_stem(template: &str) -> Option<&str> {
    template
        .strip_suffix("...")
        .or_else(|| template.strip_suffix('…'))
        .map(str::trim_end)
}

/// Whether `needle` is in `haystack` starting at a word; when `whole`, ending at one as well. A truncation may
/// end inside a word, so only its start is checked.
fn holds(haystack: &str, needle: &str, whole: bool) -> bool {
    haystack.match_indices(needle).any(|(at, _)| {
        let before = haystack[..at].chars().next_back();
        let after = haystack[at + needle.len()..].chars().next();
        before.is_none_or(|c| !c.is_alphanumeric())
            && (!whole || after.is_none_or(|c| !c.is_alphanumeric()))
    })
}

/// Folds each template of one producer that is a cut of a longer one into that one. The container is the shortest
/// that holds the cut, a template that is not itself a truncation before one that is, then the least in order.
fn fold(items: Vec<(String, Item)>) -> Vec<Entry> {
    let lengths: Vec<usize> = items
        .iter()
        .map(|(template, _)| template.chars().count())
        .collect();
    let target: Vec<Option<usize>> = items
        .iter()
        .enumerate()
        .map(|(i, (template, _))| {
            let stem = cut_stem(template);
            let part = stem.unwrap_or(template);
            let least = if stem.is_some() { MIN_CUT } else { MIN_PART };
            if part.chars().count() < least {
                return None;
            }
            items
                .iter()
                .enumerate()
                .filter(|&(j, (other, _))| {
                    lengths[j] > lengths[i]
                        && other.len() >= part.len()
                        && holds(other, part, stem.is_none())
                })
                .min_by_key(|&(j, (other, _))| {
                    (cut_stem(other).is_some(), lengths[j], other.as_str())
                })
                .map(|(j, _)| j)
        })
        .collect();
    // Each fold goes to a strictly longer template, so following them ends.
    let root = |mut at: usize| {
        while let Some(next) = target[at] {
            at = next;
        }
        at
    };
    let mut entries: Vec<Entry> = items
        .into_iter()
        .map(|(template, item)| Entry {
            cut: cut_stem(&template).is_some(),
            template,
            item,
            folded: 0,
            merged: 0,
        })
        .collect();
    for (i, folds) in target.iter().enumerate() {
        if folds.is_some() {
            let taken = entries[i].item.clone();
            let whole = &mut entries[root(i)];
            whole.item.merge(&taken);
            whole.folded += 1;
        }
    }
    entries
        .into_iter()
        .enumerate()
        .filter(|(i, _)| target[*i].is_none())
        .map(|(_, entry)| entry)
        .collect()
}

/// The letters of the tokens around a group's alternatives.
fn context_letters(before: &str, after: &str) -> usize {
    before
        .chars()
        .chain(after.chars())
        .filter(|c| c.is_alphabetic())
        .count()
}

/// The merges of templates that differ in one run of one to [`MAX_WINDOW`] tokens and agree on all the others:
/// each merged template with the entries it replaces, the largest group first. A template joins one group only.
fn merges(entries: &[Entry]) -> Vec<(String, Vec<usize>)> {
    let tokens: Vec<Vec<&str>> = entries
        .iter()
        .map(|entry| entry.template.split(' ').collect())
        .collect();
    // Key: how many tokens, how many vary, where they start, the tokens before and after.
    type Group = (usize, usize, usize, String, String);
    let mut groups: BTreeMap<Group, Vec<usize>> = BTreeMap::new();
    for (i, entry) in entries.iter().enumerate() {
        if entry.cut || entry.template.contains(['{', '}']) {
            continue;
        }
        let n = tokens[i].len();
        for width in 1..=MAX_WINDOW.min(n / 2) {
            for at in 0..=n - width {
                let before = tokens[i][..at].join(" ");
                let after = tokens[i][at + width..].join(" ");
                if context_letters(&before, &after) >= MIN_CONTEXT_LETTERS {
                    groups
                        .entry((n, width, at, before, after))
                        .or_default()
                        .push(i);
                }
            }
        }
    }
    let groups: Vec<(&Group, &Vec<usize>)> = groups
        .iter()
        .filter(|(_, members)| members.len() > 1)
        .collect();
    let mut heap: BinaryHeap<(usize, Reverse<usize>)> = groups
        .iter()
        .enumerate()
        .map(|(g, (_, members))| (members.len(), Reverse(g)))
        .collect();
    let mut taken = vec![false; entries.len()];
    let mut out = Vec::new();
    // A group shrinks as others take its members; a stale size goes back with the live one.
    while let Some((size, Reverse(g))) = heap.pop() {
        let ((_, width, at, before, after), members) = groups[g];
        let live: Vec<usize> = members.iter().copied().filter(|&i| !taken[i]).collect();
        if live.len() < 2 {
            continue;
        }
        if live.len() < size {
            heap.push((live.len(), Reverse(g)));
            continue;
        }
        let alternatives: BTreeSet<String> = live
            .iter()
            .map(|&i| tokens[i][*at..at + width].join(" "))
            .collect();
        let alternatives = alternatives.into_iter().collect::<Vec<_>>().join(" | ");
        let parts = [before.clone(), format!("{{{alternatives}}}"), after.clone()];
        let template = parts
            .iter()
            .filter(|part| !part.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        for &i in &live {
            taken[i] = true;
        }
        out.push((template, live));
    }
    out
}

/// Replaces each group [`merges`] finds with one entry holding all of its members' forms, texts and cases.
fn alternatives(entries: Vec<Entry>) -> Vec<Entry> {
    let plan = merges(&entries);
    let mut entries: Vec<Option<Entry>> = entries.into_iter().map(Some).collect();
    let mut out = Vec::new();
    for (template, members) in plan {
        let mut merged = Entry {
            template,
            item: Item::default(),
            folded: 0,
            merged: members.len(),
            cut: false,
        };
        for i in members {
            if let Some(member) = entries[i].take() {
                merged.item.merge(&member.item);
                merged.folded += member.folded;
            }
        }
        out.push(merged);
    }
    out.extend(entries.into_iter().flatten());
    out.sort_by(|a, b| a.template.cmp(&b.template));
    out
}

/// The templates of one producer as the document lists them.
fn catalogue(items: Vec<(String, Item)>) -> Vec<Entry> {
    alternatives(fold(items))
}

/// Every producer's entries, and what the collector held before the reduction.
struct Catalogue {
    producers: BTreeMap<String, Vec<Entry>>,
    /// Templates the collector kept.
    shapes: usize,
    /// Distinct rendered clauses.
    texts: usize,
}

fn build(collector: &Collector) -> Catalogue {
    let mut by: BTreeMap<String, Vec<(String, Item)>> = BTreeMap::new();
    for ((producer, template), item) in &collector.items {
        by.entry(producer.clone())
            .or_default()
            .push((template.clone(), item.clone()));
    }
    Catalogue {
        producers: by
            .into_iter()
            .map(|(producer, items)| (producer, catalogue(items)))
            .collect(),
        shapes: collector.items.len(),
        texts: collector.items.values().map(|item| item.variants).sum(),
    }
}

/// Writes the whole document; `status` is the `togi status` corpus, appended as it is.
pub(super) fn write(collector: &Collector, run: &TestRun, swept: &Swept, status: &str) -> String {
    let catalogue = build(collector);
    let mut out = String::new();
    intro(&mut out, &catalogue, run, swept);
    phrases(&mut out, &catalogue);
    messages(&mut out, &catalogue);
    manifest_section(&mut out, collector);
    out.push_str("## `togi status` corpus\n\n");
    out.push_str(status.trim());
    out.push('\n');
    out
}

fn intro(out: &mut String, catalogue: &Catalogue, run: &TestRun, swept: &Swept) {
    let entries = || catalogue.producers.values().flatten();
    let templates = entries().count();
    let folded: usize = entries().map(|entry| entry.folded).sum();
    let merged: usize = entries().map(|entry| entry.merged).sum();
    let cuts = entries().filter(|entry| entry.cut).count();
    let _ = write!(
        out,
        "# Narration corpus (#904)\n\n\
         Every phrase below is text a renderer produced in this run; none is typed in. Wording is as it is at the \
         commit that ran the export, after the #904 wording audit.\n\n\
         ## How it was collected\n\n\
         - **Unit tests.** {} unit tests ran in the same binary; {} of them rendered at least one phrase. Each \
         phrase is credited to the test (`tests`) that rendered it.\n\
         - **Sweep.** {} journals were folded one event at a time, and every prefix was drawn at 240x67, 160x45 \
         and 120x33 at the event's own time and 40 s later, and at the planned end and 90 s past it when a trial \
         runs that nothing holds. A started idle (R6) trial freezes its frame, so those are drawn at the first two \
         clocks only, and the sweep asserts the frame reports itself held. {} scenario cuts were drawn at the three \
         sizes too, with the journal states, help and the journal view ({} frames in all). Credited as `sweep`.\n\
         - **Clauses and templates.** A rendered phrase is cut into clauses at ` · `, ` ─ `, ` │ `, ` → ` and \
         runs of two spaces, and a clause with no letter is dropped. A clause becomes a template by folding its \
         digits, lists and ranges of numbers to `#`, the signs before offsets, hexadecimal ids, gauge and rule \
         glyphs and runs of spaces. The numbers, core lists and error strings a phrase can carry are unbounded, so \
         only the finite templates, forms and branches are claimed as covered.\n\
         - **Cuts.** A template that is only a cut of a longer one of the same producer, a truncation ending in \
         `...`, a wrapped row or a column cut, is folded into the longer one; its forms, cases and texts stay on it. \
         A truncation no other template holds whole stays and is marked.\n\
         - **Alternatives.** Templates of one producer that differ in one run of up to {MAX_WINDOW} tokens and agree \
         on every token around it are one line, `before {{a | b}} after`. Each alternative was rendered in exactly \
         that context, and two positions never vary at once.\n\
         - **Provenance.** Each line names its forms, the two least case names that rendered it, and how many \
         renderings there were. A manifest literal is `file:line`.\n\n\
         {} templates from {} producers, reduced from {} shapes and {} distinct texts: {folded} cuts folded in, \
         {merged} templates merged into alternatives, {cuts} truncations with no full form.\n\n",
        run.passed,
        run.recording,
        swept.journals.len(),
        swept.scenarios,
        swept.frames,
        templates,
        catalogue.producers.len(),
        catalogue.shapes,
        catalogue.texts,
    );
    out.push_str(
        "## Regenerating\n\n\
         Both exporters are ignored tests. `<repo>` is the checkout and `<status-export>` an absolute path for the \
         status corpus the second one reads; the first writes it, the second writes this document.\n\n\
         ```sh\n\
         TOGI_NARRATION_CORPUS=<status-export> \\\n    cargo test --locked -p togi narration_corpus -- --ignored --nocapture\n\
         TOGI_NARRATION_STATUS=<status-export> TOGI_NARRATION_CORPUS=<repo>/docs/narration-phrases.md \\\n    cargo test --locked -p togi-watch narration_corpus -- --ignored --nocapture\n\
         ```\n\n",
    );
    let _ = writeln!(out, "### Sweep journals\n");
    for (name, events, drawn) in &swept.journals {
        let _ = writeln!(out, "- `{name}`: {events} events, {drawn} frames");
    }
    let _ = writeln!(out, "\n### Phrases by producer\n");
    for (producer, entries) in &catalogue.producers {
        let cuts = entries.iter().filter(|entry| entry.cut).count();
        let note = if cuts > 0 {
            format!(", {cuts} truncations with no full form")
        } else {
            String::new()
        };
        let _ = writeln!(out, "- `{producer}`: {} templates{note}", entries.len());
    }
    out.push('\n');
}

/// What follows a template: its forms, what was folded or merged into it, and where it was rendered.
fn detail(entry: &Entry) -> String {
    let forms: Vec<String> = entry
        .item
        .forms
        .iter()
        .map(|form| {
            if form.is_empty() {
                "(none)".to_owned()
            } else {
                code(form)
            }
        })
        .collect();
    let mut parts = vec![format!("forms {}", forms.join(", "))];
    if entry.folded > 0 {
        parts.push(format!("{} cuts folded in", entry.folded));
    }
    if entry.merged > 0 {
        parts.push(format!("{} templates merged", entry.merged));
    }
    if entry.cut {
        parts.push(format!("example {}", code(&entry.item.text)));
    }
    parts.push(provenance(&entry.item.cases));
    parts.join("; ")
}

fn entry(out: &mut String, entry: &Entry) {
    let cut = if entry.cut {
        " — truncated, no full form rendered"
    } else {
        ""
    };
    let _ = writeln!(
        out,
        "- {} ×{}{cut}\n  - {}",
        code(&entry.template),
        entry.item.variants,
        detail(entry)
    );
}

fn phrases(out: &mut String, catalogue: &Catalogue) {
    let _ = writeln!(out, "## Phrase catalogue\n");
    for (producer, entries) in &catalogue.producers {
        if producer == MESSAGES {
            continue;
        }
        let _ = writeln!(out, "\n### `{producer}`\n");
        for one in entries {
            entry(out, one);
        }
    }
    out.push('\n');
}

fn messages(out: &mut String, catalogue: &Catalogue) {
    let _ = writeln!(
        out,
        "## Forwarded journal messages\n\n\
         The log view and `togi status` pass these through as the journal wrote them, so a wording problem in \
         one belongs to the tuner that wrote it, not to a dashboard phrase.\n"
    );
    for one in catalogue.producers.get(MESSAGES).into_iter().flatten() {
        entry(out, one);
    }
    out.push('\n');
}

fn manifest_section(out: &mut String, collector: &Collector) {
    let literals = manifest::literals();
    let _ = writeln!(
        out,
        "## Source-phrase manifest\n\n\
         Every string literal of a renderer's production source that holds words ({} of them), read from the \
         source this binary was built from. A literal is *covered* when one recorded phrase carries every piece \
         of it that sits between placeholders; the cases that rendered it follow. Literals the renderers format \
         from several pieces are covered by the pieces' own literals. A literal listed as silent shows its reason \
         first, and any phrase that happens to carry its words after it as substring evidence only.\n",
        literals.len()
    );
    let mut by_file: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, literal) in literals.iter().enumerate() {
        by_file.entry(literal.file).or_default().push(i);
    }
    let mut uncovered = Vec::new();
    for (file, indices) in &by_file {
        let covered = indices
            .iter()
            .filter(|&&i| collector.literals[i].any() && literals[i].silent().is_none())
            .count();
        let _ = writeln!(
            out,
            "### `{file}`: {covered} of {} covered\n",
            indices.len()
        );
        for &i in indices {
            let literal = &literals[i];
            let seen = &collector.literals[i];
            let line = format!(
                "- `{}:{}` {}",
                literal.file,
                literal.line,
                code(&truncated(&literal.text))
            );
            match (literal.silent(), seen.any()) {
                (Some(reason), true) => {
                    let _ = writeln!(
                        out,
                        "{line} — silent: {reason}; substring evidence: {}",
                        provenance(seen)
                    );
                }
                (Some(reason), false) => {
                    let _ = writeln!(out, "{line} — silent: {reason}");
                }
                (None, true) => {
                    let _ = writeln!(out, "{line} — {}", provenance(seen));
                }
                (None, false) => {
                    let _ = writeln!(out, "{line} — **not rendered**");
                    uncovered.push(i);
                }
            }
        }
        out.push('\n');
    }
    let _ = writeln!(out, "### Uncovered literals\n");
    if uncovered.is_empty() {
        let _ = writeln!(out, "None.\n");
    }
    for i in &uncovered {
        let literal = &literals[*i];
        let _ = writeln!(
            out,
            "- `{}:{}` {}",
            literal.file,
            literal.line,
            code(&truncated(&literal.text))
        );
    }
    let _ = writeln!(out, "\n### Silent on purpose\n");
    if SILENT.is_empty() {
        let _ = writeln!(out, "None declared.\n");
    }
    for (file, start, reason) in SILENT {
        let _ = writeln!(out, "- `{file}` {}: {reason}", code(start));
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalogue of the phrases `renders` hand the collector, as (case, producer, form, text).
    fn of(renders: &[(&str, &str, &str, &str)]) -> Vec<Entry> {
        let mut collector = Collector::default();
        for &(case, producer, form, text) in renders {
            collector.add(case, producer, form, text);
        }
        let items = collector
            .items
            .iter()
            .map(|((_, template), item)| (template.clone(), item.clone()))
            .collect();
        catalogue(items)
    }

    /// The templates `entry` stands for: itself, or one per alternative.
    fn expanded(template: &str) -> BTreeSet<String> {
        let Some((before, rest)) = template.split_once('{') else {
            return BTreeSet::from([template.to_owned()]);
        };
        let (alternatives, after) = rest.split_once('}').expect("a closed brace");
        alternatives
            .split(" | ")
            .map(|alternative| format!("{before}{alternative}{after}").replace("  ", " "))
            .collect()
    }

    #[test]
    fn a_truncation_folds_into_the_full_template_and_keeps_its_forms_and_cases() {
        let entries = of(&[
            (
                "full",
                "frame.say",
                "Main Full",
                "so a freeze needs a manual reset",
            ),
            ("cut", "frame.say", "Main Compact", "so a freeze needs a..."),
            ("cut", "frame.say", "Main Compact", "so a freeze needs a…"),
        ]);
        assert_eq!(entries.len(), 1, "{entries:?}");
        let whole = &entries[0];
        assert_eq!(whole.template, "so a freeze needs a manual reset");
        assert!(!whole.cut);
        assert_eq!(whole.folded, 2);
        assert_eq!(
            whole.item.forms,
            BTreeSet::from(["Main Compact".to_owned(), "Main Full".to_owned()])
        );
        assert_eq!(whole.item.variants, 3, "the cut's two texts and the full one");
        assert_eq!(
            whole.item.cases.tests.names,
            BTreeSet::from(["cut".to_owned(), "full".to_owned()])
        );
        assert_eq!(whole.item.cases.tests.renders, 3);
    }

    #[test]
    fn a_truncation_nothing_holds_whole_stays_and_says_so() {
        let entries = of(&[
            ("a", "frame.say", "Main", "heavy vector load on core..."),
            ("b", "frame.say", "Main", "an unrelated whole phrase"),
        ]);
        let cut: Vec<&Entry> = entries.iter().filter(|entry| entry.cut).collect();
        assert_eq!(cut.len(), 1, "{entries:?}");
        assert_eq!(cut[0].template, "heavy vector load on core...");
        assert_eq!(cut[0].folded, 0);
        let mut doc = String::new();
        entry(&mut doc, cut[0]);
        assert!(doc.contains("truncated, no full form rendered"), "{doc}");
        assert!(doc.contains("example"), "{doc}");
    }

    #[test]
    fn a_short_phrase_is_not_folded_into_a_longer_one_that_merely_contains_it() {
        let entries = of(&[
            ("a", "p", "f", "all passed"),
            ("b", "p", "f", "core 03 passed and 4 others failed"),
            ("c", "p", "f", "passed"),
        ]);
        assert_eq!(entries.len(), 3, "{entries:?}");
    }

    #[test]
    fn a_part_of_a_longer_phrase_folds_only_at_word_boundaries() {
        let entries = of(&[
            ("a", "p", "f", "freeze needs a manual reset"),
            ("b", "p", "f", "so a freeze needs a manual reset first"),
            ("c", "p", "f", "needs a manual rese"),
        ]);
        let templates: Vec<&str> = entries.iter().map(|e| e.template.as_str()).collect();
        assert_eq!(
            templates,
            ["needs a manual rese", "so a freeze needs a manual reset first"],
            "{entries:?}"
        );
    }

    #[test]
    fn templates_that_differ_in_one_run_merge_into_alternatives() {
        let entries = of(&[
            ("a", "p", "Main", "core 03 failed at -9"),
            ("b", "p", "Compact", "core 04 passed at -12"),
            ("c", "p", "Main", "core 05 crashed at 7"),
        ]);
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].template, "core # {crashed | failed | passed} at #");
        assert_eq!(entries[0].merged, 3);
        assert_eq!(
            entries[0].item.forms,
            BTreeSet::from(["Compact".to_owned(), "Main".to_owned()])
        );
        assert_eq!(entries[0].item.cases.tests.renders, 3);
    }

    #[test]
    fn independent_positions_never_merge_into_combinations_nobody_rendered() {
        let rendered = [
            "run alpha one tail words",
            "run alpha two tail words",
            "run beta one tail words",
            "stop gamma three tail words",
        ];
        let renders: Vec<(&str, &str, &str, &str)> =
            rendered.iter().map(|text| ("a", "p", "f", *text)).collect();
        let entries = of(&renders);
        let mut shown = BTreeSet::new();
        for entry in &entries {
            assert!(entry.template.matches('{').count() <= 1, "{entry:?}");
            shown.extend(expanded(&entry.template));
        }
        assert_eq!(
            shown,
            rendered.iter().map(|text| (*text).to_owned()).collect(),
            "{entries:?}"
        );
    }

    #[test]
    fn producers_never_share_a_template() {
        let mut collector = Collector::default();
        collector.add("a", "one", "f", "so a freeze needs a manual reset");
        collector.add("a", "two", "f", "so a freeze needs a...");
        let mut by: BTreeMap<String, Vec<(String, Item)>> = BTreeMap::new();
        for ((producer, template), item) in &collector.items {
            by.entry(producer.clone())
                .or_default()
                .push((template.clone(), item.clone()));
        }
        for items in by.into_values() {
            let entries = catalogue(items);
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].folded, 0);
        }
    }
}
