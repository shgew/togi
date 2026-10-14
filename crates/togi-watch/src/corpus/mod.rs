//! The narration corpus of #904: every phrase the dashboard renders, collected from the renderers themselves.
//!
//! Test-only. The renderers hand what they produce to [`text`], [`line`], [`lines`] or [`region`]: the narration,
//! the resting band, the outcome branches before they merge and the rows after, and the history sentences, all
//! unclipped; and every panel and frame region as fitted at its width. Nothing records unless a corpus run
//! collects, so an ordinary test run pays one thread-local check per hook.
//!
//! [`narration_corpus`] is the exporter, ignored by default. Given `TOGI_NARRATION_CORPUS`, an absolute path, it
//!
//! 1. reruns every other unit test of this crate in a child process of this test binary; each test records what it
//!    renders under its own name, into a records file beside the document;
//! 2. folds every prefix of every [`togi_watchtest::Base`] journal and of the cycle a rerun completes, and draws
//!    each at the three main sizes and at its clocks ([`crate::frames::corpus::run`]);
//! 3. draws the 18 [`togi_watchtest::Scenario`] cuts and the journal states as whole frames;
//! 4. writes one Markdown document ([`document`]): the phrases by producer, form and shape, the forwarded journal
//!    messages, the frames, and the [`manifest`] of source literals with the cases that rendered each.
//!
//! It fails, after writing the document, when an enforced source literal was rendered by no case and is not listed
//! as silent.

mod document;
mod manifest;

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use togi_journal::Event;

use crate::history::Entry;

/// The document the exporter writes: an absolute path the coordinator supplies.
const DOC_VAR: &str = "TOGI_NARRATION_CORPUS";

/// Set by the exporter for its child run: the records file every test appends what it renders to.
const RECORDS_VAR: &str = "TOGI_NARRATION_CORPUS_RECORDS";

/// How many case names a phrase or a literal keeps, per kind of case; the rest are counted.
const NAMES: usize = 3;

/// The producer of the journal's own messages, which the log view and status forward unchanged.
pub(crate) const MESSAGES: &str = "journal.message";

thread_local! {
    /// The case the current thread renders for; the thread's name, a test's path, when unset.
    static CASE: RefCell<Option<String>> = const { RefCell::new(None) };
    /// The collector of the exporter's own thread.
    static LOCAL: RefCell<Option<Collector>> = const { RefCell::new(None) };
}

/// Runs `render` as the case `id`: everything it records is credited to `id`.
pub(crate) fn case<R>(id: impl Into<String>, render: impl FnOnce() -> R) -> R {
    let previous = CASE.with(|case| case.replace(Some(id.into())));
    let out = render();
    CASE.with(|case| *case.borrow_mut() = previous);
    out
}

/// Whether anything records on this thread.
pub(crate) fn collecting() -> bool {
    LOCAL.with(|local| local.borrow().is_some()) || file_sink().is_some()
}

/// Records one rendered phrase of `producer` (`story.lines`, `frame.ccd`) in `form` (`full`, `brief`,
/// `Main Compact`): a no-op unless a corpus run collects. Blank text is not a phrase.
pub(crate) fn text(producer: &str, form: &str, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    let collected = LOCAL.with(|local| match local.borrow_mut().as_mut() {
        Some(collector) => {
            collector.add(&current_case(), producer, form, text);
            true
        }
        None => false,
    });
    if !collected && let Some(sink) = file_sink() {
        sink.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .write(&current_case(), producer, form, text);
    }
}

/// Records the plain text of one rendered line.
pub(crate) fn line(producer: &str, form: &str, line: &Line<'_>) {
    if collecting() {
        text(producer, form, &crate::frames::plain(line));
    }
}

/// Records the plain text of each rendered line.
pub(crate) fn lines(producer: &str, form: &str, lines: &[Line<'_>]) {
    if collecting() {
        for line in lines {
            text(producer, form, &crate::frames::plain(line));
        }
    }
}

/// Records each row of `rect` in a drawn `buf` as the reader sees it, trailing blanks trimmed.
pub(crate) fn region(producer: &str, form: &str, buf: &Buffer, rect: Rect) {
    if !collecting() {
        return;
    }
    let area = buf.area.intersection(rect);
    for y in area.top()..area.bottom() {
        let row: String = (area.left()..area.right())
            .map(|x| buf[(x, y)].symbol())
            .collect();
        text(producer, form, row.trim_end());
    }
}

/// Records the history entry an event made, under the event's kind.
pub(crate) fn described(event: &Event, entry: &Entry) {
    text("describe", event.kind().as_str(), &entry.text);
}

/// Records a journal message the log view lists, under its tag.
pub(crate) fn journal_message(tag: &str, message: &str) {
    text(MESSAGES, tag, message);
}

/// Records a history entry's final sentence, after every merge into it, under the way it is built.
pub(crate) fn sentence(entry: &Entry, (before, alarm, after): &(String, String, String)) {
    if collecting() {
        text(
            "history.sentence",
            &format!("{:?}", entry.kind),
            &format!("{before}{alarm}{after}"),
        );
    }
}

fn current_case() -> String {
    CASE.with(|case| case.borrow().clone()).unwrap_or_else(|| {
        std::thread::current()
            .name()
            .filter(|name| *name != "main")
            .map_or_else(|| "unnamed thread".to_owned(), str::to_owned)
    })
}

/// The records file of a child run, appended to once per distinct case, producer, form and text.
struct FileSink {
    file: File,
    seen: HashSet<u64>,
}

impl FileSink {
    fn write(&mut self, case: &str, producer: &str, form: &str, text: &str) {
        let record = format!(
            "{}\t{}\t{}\t{}\n",
            escape(case),
            escape(producer),
            escape(form),
            escape(text)
        );
        if self.seen.insert(hash_of(&record)) {
            self.file
                .write_all(record.as_bytes())
                .unwrap_or_else(|error| panic!("writing the corpus records: {error}"));
        }
    }
}

fn file_sink() -> Option<&'static Mutex<FileSink>> {
    static SINK: LazyLock<Option<Mutex<FileSink>>> = LazyLock::new(|| {
        let path = std::env::var_os(RECORDS_VAR)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap_or_else(|error| panic!("opening {}: {error}", Path::new(&path).display()));
        Some(Mutex::new(FileSink {
            file,
            seen: HashSet::new(),
        }))
    });
    SINK.as_ref()
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

fn hash_of(value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// Whether `c` draws a gauge, a bar or a rule: block elements and box drawing.
fn is_track(c: char) -> bool {
    matches!(c, '\u{2500}'..='\u{259F}')
}

/// `text` with its parameters folded, so phrases of one template with other cores, offsets, times, counts, gauge
/// fills or column padding share a shape: every run of digits is `#`, lists and ranges of numbers are one `#`,
/// every run of gauge or rule glyphs is one `▮` and every run of two or more spaces is two.
pub(crate) fn shape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = None;
    let mut spaces = 0;
    for c in text.chars() {
        let class = if c.is_ascii_digit() {
            '#'
        } else if is_track(c) {
            '▮'
        } else {
            c
        };
        if c == ' ' {
            spaces += 1;
            if spaces > 2 {
                continue;
            }
        } else {
            spaces = 0;
        }
        if (class == '#' || class == '▮') && last == Some(class) {
            continue;
        }
        out.push(class);
        last = Some(class);
    }
    loop {
        let before = out.len();
        for joined in [
            "# #", "#-#", "#–#", "#:#", "#.#", "#/#", "#,#", "#, #", "#h#", "#h #", "#m#",
        ] {
            if out.contains(joined) {
                out = out.replace(joined, "#");
            }
        }
        if out.len() == before {
            return out;
        }
    }
}

/// Where a case comes from: the sweep of simulated journals and frames, or a unit test.
fn is_sweep(case: &str) -> bool {
    case.starts_with("sweep/") || case.starts_with("scenario/") || case.starts_with("journal/")
}

/// What rendered something: how many renderings, and the [`NAMES`] first case names in order.
#[derive(Debug, Default, Clone)]
pub(crate) struct Cases {
    pub(crate) names: BTreeSet<String>,
    pub(crate) renders: usize,
}

impl Cases {
    fn add(&mut self, case: &str) {
        self.renders += 1;
        if self.names.contains(case) {
            return;
        }
        if self.names.len() < NAMES {
            self.names.insert(case.to_owned());
        } else if self.names.last().is_some_and(|last| case < last.as_str()) {
            self.names.insert(case.to_owned());
            self.names.pop_last();
        }
    }
}

/// The cases that rendered something, unit tests and sweep apart.
#[derive(Debug, Default, Clone)]
pub(crate) struct Provenance {
    pub(crate) tests: Cases,
    pub(crate) sweep: Cases,
}

impl Provenance {
    fn add(&mut self, case: &str) {
        if is_sweep(case) {
            self.sweep.add(case);
        } else {
            self.tests.add(case);
        }
    }

    /// Whether any case rendered it.
    pub(crate) fn any(&self) -> bool {
        self.tests.renders + self.sweep.renders > 0
    }
}

/// One phrase shape of one producer and form: its smallest rendered text, how many distinct texts share the
/// shape, and the cases that rendered one.
#[derive(Debug, Default, Clone)]
pub(crate) struct Item {
    pub(crate) text: String,
    pub(crate) variants: usize,
    pub(crate) cases: Provenance,
    texts: HashSet<u64>,
}

/// The key of an [`Item`]: producer, form and shape.
pub(crate) type Key = (String, String, String);

/// What a corpus run collected.
#[derive(Debug)]
pub(crate) struct Collector {
    /// By producer, form and shape.
    pub(crate) items: BTreeMap<Key, Item>,
    /// Whole frames to show as they are, by case.
    pub(crate) frames: BTreeMap<String, String>,
    /// For each source literal of [`manifest::literals`], the cases that rendered it.
    pub(crate) literals: Vec<Provenance>,
    /// The literals each distinct rendered text carries, by the text's hash.
    carries: HashMap<u64, Vec<usize>>,
}

impl Default for Collector {
    fn default() -> Self {
        Self {
            items: BTreeMap::new(),
            frames: BTreeMap::new(),
            literals: vec![Provenance::default(); manifest::literals().len()],
            carries: HashMap::new(),
        }
    }
}

impl Collector {
    fn add(&mut self, case: &str, producer: &str, form: &str, text: &str) {
        let key = (producer.to_owned(), form.to_owned(), shape(text));
        let item = self.items.entry(key).or_default();
        let hash = hash_of(&text);
        if item.texts.insert(hash) {
            item.variants += 1;
            if item.text.is_empty() || text < item.text.as_str() {
                text.clone_into(&mut item.text);
            }
        }
        item.cases.add(case);
        let carried = self
            .carries
            .entry(hash)
            .or_insert_with(|| manifest::carried(text));
        for &literal in carried.iter() {
            self.literals[literal].add(case);
        }
    }
}

fn start() {
    LOCAL.with(|local| *local.borrow_mut() = Some(Collector::default()));
}

fn finish() -> Collector {
    LOCAL
        .with(|local| local.borrow_mut().take())
        .expect("the exporter's thread collects")
}

/// Keeps a whole frame for the document, as the case that drew it.
pub(crate) fn keep_frame(case: &str, frame: String) {
    LOCAL.with(|local| {
        if let Some(collector) = local.borrow_mut().as_mut() {
            collector.frames.insert(case.to_owned(), frame);
        }
    });
}

fn records_path(doc: &Path) -> PathBuf {
    let mut name = doc.as_os_str().to_owned();
    name.push(".records");
    PathBuf::from(name)
}

/// What the child run of the unit tests reported.
#[derive(Debug, Default)]
pub(crate) struct TestRun {
    /// Tests that passed.
    pub(crate) passed: usize,
    /// Distinct cases that recorded a phrase.
    pub(crate) recording: usize,
}

/// Runs every other unit test of this binary in a child process, recording to `records`. Several test threads
/// keep every test on a thread named after it.
fn run_tests(records: &Path) -> usize {
    let exe = std::env::current_exe().expect("the test binary's path");
    let output = std::process::Command::new(&exe)
        .args(["--skip", "narration_corpus", "--test-threads", "4"])
        .env(RECORDS_VAR, records)
        .env_remove(DOC_VAR)
        .output()
        .unwrap_or_else(|error| panic!("running {}: {error}", exe.display()));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "the unit tests the corpus collects from failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
        .lines()
        .filter(|line| line.starts_with("test ") && line.ends_with(" ... ok"))
        .count()
}

/// Adds the child run's records to `collector`; returns how many distinct cases recorded.
fn load(records: &Path, collector: &mut Collector) -> usize {
    let file = File::open(records)
        .unwrap_or_else(|error| panic!("{}: {error}; the child run recorded nothing", records.display()));
    let mut cases = HashSet::new();
    for record in BufReader::new(file).lines() {
        let record = record.unwrap_or_else(|error| panic!("{}: {error}", records.display()));
        let mut fields = record.splitn(4, '\t');
        let (Some(case), Some(producer), Some(form), Some(text)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            panic!("{}: a malformed record: {record:?}", records.display());
        };
        let case = unescape(case);
        collector.add(&case, &unescape(producer), &unescape(form), &unescape(text));
        cases.insert(case);
    }
    cases.len()
}

/// Exports the dashboard's narration corpus to `$TOGI_NARRATION_CORPUS`.
#[test]
#[ignore = "exports the #904 narration corpus; needs TOGI_NARRATION_CORPUS"]
fn narration_corpus() {
    let doc = PathBuf::from(std::env::var_os(DOC_VAR).unwrap_or_else(|| {
        panic!("{DOC_VAR} must name the absolute path of the corpus document to write")
    }));
    assert!(
        doc.is_absolute(),
        "{DOC_VAR} must be absolute: {}",
        doc.display()
    );
    let records = records_path(&doc);
    File::create(&records).unwrap_or_else(|error| panic!("{}: {error}", records.display()));

    let passed = run_tests(&records);

    start();
    let swept = crate::frames::corpus::run();
    let mut collector = finish();
    let recording = load(&records, &mut collector);
    assert!(recording > 0, "the child test run recorded no rendered phrase");
    std::fs::remove_file(&records)
        .unwrap_or_else(|error| panic!("{}: {error}", records.display()));

    let run = TestRun { passed, recording };
    let markdown = document::write(&collector, &run, &swept);
    std::fs::write(&doc, markdown).unwrap_or_else(|error| panic!("{}: {error}", doc.display()));
    println!("narration corpus: {}", doc.display());
    let uncovered = manifest::uncovered(&collector);
    assert!(
        uncovered.is_empty(),
        "{} enforced source literals were rendered by no case and are not listed as silent; see \
         \"Uncovered literals\" in {}:\n{}",
        uncovered.len(),
        doc.display(),
        uncovered.join("\n")
    );
}
