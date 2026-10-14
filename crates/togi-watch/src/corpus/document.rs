//! The Markdown the exporter writes: the phrase catalogue, the forwarded journal messages, the manifest of source
//! phrases with the cases that rendered them.

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};

use super::manifest::{self, SILENT};
use super::{Cases, Collector, MESSAGES, Provenance, TestRun};
use crate::frames::corpus::Swept;

/// How much of a source literal the manifest shows.
const LITERAL_WIDTH: usize = 100;

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

/// A stable ID for the phrase shape: `producer/form/hash`; the hash comes from the shape, so an unrelated phrase
/// never renumbers it.
fn id(producer: &str, form: &str, shape: &str) -> String {
    let mut hasher = DefaultHasher::new();
    shape.hash(&mut hasher);
    let form = if form.is_empty() { "-" } else { form };
    format!("{producer}/{form}/{:08x}", hasher.finish() >> 32)
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

/// Writes the whole document.
pub(super) fn write(collector: &Collector, run: &TestRun, swept: &Swept) -> String {
    let mut out = String::new();
    intro(&mut out, collector, run, swept);
    catalogue(&mut out, collector);
    messages(&mut out, collector);
    manifest_section(&mut out, collector);
    out
}

fn intro(out: &mut String, collector: &Collector, run: &TestRun, swept: &Swept) {
    let variants: usize = collector.items.values().map(|item| item.variants).sum();
    let producers: BTreeMap<&str, usize> =
        collector
            .items
            .keys()
            .fold(BTreeMap::new(), |mut by, (producer, _, _)| {
                *by.entry(producer.as_str()).or_default() += 1;
                by
            });
    let _ = write!(
        out,
        "# Narration corpus (#904)\n\n\
         Every phrase below is text a renderer produced in this run; none is typed in. Wording is as it is at the \
         commit that ran the export, before any #904 change.\n\n\
         ## How it was collected\n\n\
         - **Unit tests.** {} unit tests ran in the same binary; {} of them rendered at least one phrase. Each \
         phrase is credited to the test (`tests`) that rendered it.\n\
         - **Sweep.** {} journals were folded one event at a time, and every prefix was drawn at 240x67, 160x45 \
         and 120x33 at the event's own time and 40 s later, and at the planned end and 90 s past it when a trial \
         runs that nothing holds. A started idle (R6) trial freezes its frame, so those are drawn at the first two \
         clocks only, and the sweep asserts the frame reports itself held. {} scenario cuts were drawn at the three \
         sizes too, with the journal states, help and the journal view ({} frames in all). Credited as `sweep`.\n\
         - **Shapes.** A phrase is kept once per producer, form and shape; a shape is the text with its digits, \
         gauge glyphs and runs of spaces folded. The representative is the least text of the shape, `variants` \
         counts the distinct texts, and the cases are the three least case names with how many renderings there \
         were. The numbers, core lists and error strings a phrase can carry are unbounded, so only the finite \
         templates, forms and branches are claimed as covered.\n\
         - **IDs.** `producer/form/hash` names a shape; the hash depends on the shape alone, so it survives \
         the addition of other phrases. A manifest literal is `file:line`.\n\n\
         {} phrase shapes, {} distinct texts, from {} producers.\n\n",
        run.passed,
        run.recording,
        swept.journals.len(),
        swept.scenarios,
        swept.frames,
        collector.items.len(),
        variants,
        producers.len(),
    );
    let _ = writeln!(out, "### Sweep journals\n");
    for (name, events, drawn) in &swept.journals {
        let _ = writeln!(out, "- `{name}`: {events} events, {drawn} frames");
    }
    let _ = writeln!(out, "\n### Phrases by producer\n");
    for (producer, shapes) in &producers {
        let _ = writeln!(out, "- `{producer}`: {shapes} shapes");
    }
    out.push('\n');
}

fn catalogue(out: &mut String, collector: &Collector) {
    let _ = writeln!(out, "## Phrase catalogue\n");
    let mut current = "";
    let mut form = "\u{0}";
    for ((producer, item_form, shape), item) in &collector.items {
        if producer == MESSAGES {
            continue;
        }
        if producer != current {
            let _ = writeln!(out, "\n### `{producer}`");
            current = producer;
            form = "\u{0}";
        }
        if item_form != form {
            let _ = writeln!(out, "\n#### form {}\n", code(item_form));
            form = item_form;
        }
        let _ = writeln!(
            out,
            "- `{}` ×{}: {}\n  - {}",
            id(producer, item_form, shape),
            item.variants,
            code(&item.text),
            provenance(&item.cases)
        );
    }
    out.push('\n');
}

fn messages(out: &mut String, collector: &Collector) {
    let _ = writeln!(
        out,
        "## Forwarded journal messages\n\n\
         The log view and `togi status` pass these through as the journal wrote them, so a wording problem in \
         one belongs to the tuner that wrote it, not to a dashboard phrase.\n"
    );
    for ((producer, form, shape), item) in &collector.items {
        if producer != MESSAGES {
            continue;
        }
        let _ = writeln!(
            out,
            "- `{}` ×{}: {}\n  - {}",
            id(producer, form, shape),
            item.variants,
            code(&item.text),
            provenance(&item.cases)
        );
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
         from several pieces are covered by the pieces' own literals.\n",
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
            .filter(|&&i| collector.literals[i].any())
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
            if seen.any() {
                let _ = writeln!(out, "{line}\n  - {}", provenance(seen));
            } else if let Some(reason) = literal.silent() {
                let _ = writeln!(out, "{line}\n  - silent: {reason}");
            } else {
                let _ = writeln!(out, "{line}\n  - **not rendered**");
                uncovered.push(i);
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
    let _ = writeln!(out, "\n### Silent or omitted on purpose\n");
    if SILENT.is_empty() {
        let _ = writeln!(out, "None declared.\n");
    }
    for (file, start, reason) in SILENT {
        let _ = writeln!(out, "- `{file}` {}: {reason}", code(start));
    }
    out.push('\n');
}
