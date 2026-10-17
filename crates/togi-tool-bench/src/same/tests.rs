use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;
use togi_journal::fs::{FileKind, Fs, MemFs};

struct MemoryDir {
    fs: Arc<MemFs>,
    root: &'static Path,
}

impl MemoryDir {
    fn new() -> Self {
        let fs = Arc::new(MemFs::new());
        let root = Path::new("/runs");
        fs.create_dir(root).unwrap();
        Self { fs, root }
    }

    fn path(&self) -> &Path {
        self.root
    }
}

use super::SameError;
use super::compare::{self, Config, Launcher, Pair, Ran, Side};
use super::digests::DigestCache;
use super::journal::{self, Difference, JournalError};
use crate::cache::Costs;
use crate::error::RunError;
use crate::process::Fleet;
use crate::suite::{RunSpec, Split};

const SESSION_START: &str = r#"{"seq":1,"time":"2026-01-01T00:00:00Z","boot":"b","kind":"session.start","msg":"session s started by togi old+abc (schema 1, ruleset 7, evidence epoch 2, fixes 3, 2 cores); togi old+abc","data":{"build":{"version":"old","rev":"abc","ruleset":7,"schema":1,"fixes":3,"evidence":2},"session":"s","cores":[]}}"#;

fn plain() -> String {
    SESSION_START.replace("; togi old+abc", "")
}

fn value(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

fn normalized(line: &str) -> Value {
    value(&journal::normalize(format!("{line}\n").as_bytes()).unwrap())
}

#[test]
fn normalizing_removes_only_the_build_stamp_of_a_session_start() {
    let want = value(
        br#"{"seq":1,"time":"2026-01-01T00:00:00Z","boot":"b","kind":"session.start","msg":"session s started by  (schema 1, ruleset 7, evidence epoch 2, fixes 3, 2 cores); togi old+abc","data":{"build":{"ruleset":7,"schema":1,"fixes":3,"evidence":2},"session":"s","cores":[]}}"#,
    );
    assert_eq!(normalized(SESSION_START), want);
}

#[test]
fn normalizing_takes_the_description_out_of_both_config_messages() {
    let file = r#"{"kind":"config.loaded","msg":"config loaded from config.toml by togi old (schema 1, ruleset 7, fixes 3)","data":{"build":{"version":"old","ruleset":7},"path":"config.toml","file":true}}"#;
    let defaults = r#"{"kind":"config.loaded","msg":"no config at config.toml; using defaults with togi old+abc (schema 1, ruleset 7, fixes 3)","data":{"build":{"version":"old","rev":"abc","ruleset":7},"path":"config.toml","file":false}}"#;
    assert_eq!(
        normalized(file)["msg"],
        "config loaded from config.toml by  (schema 1, ruleset 7, fixes 3)"
    );
    assert_eq!(
        normalized(defaults)["msg"],
        "no config at config.toml; using defaults with  (schema 1, ruleset 7, fixes 3)"
    );
    assert_eq!(
        normalized(file)["data"]["build"],
        serde_json::json!({"ruleset": 7})
    );
}

#[test]
fn normalizing_leaves_every_other_event_byte_for_byte() {
    let other = " {\"kind\":\"trial.end\", \"msg\":\"session.start togi old+abc\"} \n";
    assert!(matches!(
        journal::normalize(other.as_bytes()).unwrap(),
        std::borrow::Cow::Borrowed(line) if line == other.as_bytes()
    ));
    let unplaced = r#"{"kind":"session.start","msg":"unexpected togi old+abc","data":{"build":{"version":"old","rev":"abc"},"session":"s"}}"#;
    assert_eq!(normalized(unplaced)["msg"], "unexpected togi old+abc");
}

#[test]
fn a_stamped_event_that_does_not_decode_is_an_error() {
    assert!(journal::normalize(b"{\"kind\":\"session.start\"\n").is_err());
}

#[test]
fn a_build_only_difference_is_not_a_difference() {
    let base = plain();
    let head = base.replace("old", "new").replace("abc", "def");
    let diff = |head: &str| {
        journal::first_difference(
            format!("{base}\n").as_bytes(),
            format!("{head}\n").as_bytes(),
        )
        .unwrap()
        .is_some()
    };
    assert!(!diff(&head));
    for (name, before, after) in [
        ("session", r#""session":"s""#, r#""session":"other""#),
        ("ruleset", r#""ruleset":7"#, r#""ruleset":8"#),
        ("schema", r#""schema":1"#, r#""schema":2"#),
        ("fixes", r#""fixes":3"#, r#""fixes":4"#),
        ("evidence", r#""evidence":2"#, r#""evidence":3"#),
        ("message decision", "2 cores", "3 cores"),
        ("cores", r#""cores":[]"#, r#""cores":[1]"#),
    ] {
        assert!(diff(&head.replacen(before, after, 1)), "{name}");
    }
}

#[test]
fn the_first_difference_is_the_first_line_that_differs() {
    let line = |base: &str, head: &str| {
        journal::first_difference(base.as_bytes(), head.as_bytes()).unwrap()
    };
    assert_eq!(line("{}\n", "{}\n"), None);
    assert_eq!(
        line(
            "{}\n{}\n{\"seq\":3}\n{\"seq\":4}\n",
            "{}\n{}\n{\"seq\":9}\n{}\n"
        ),
        Some((3, r#"{"seq":3}"#.into(), r#"{"seq":9}"#.into()))
    );
    assert_eq!(
        line("{}\n{}\n", "{}\n"),
        Some((2, "{}".into(), "<missing event>".into()))
    );
    assert_eq!(
        line("{}\n", "{}\n{}\n"),
        Some((2, "<missing event>".into(), "{}".into()))
    );
    assert_eq!(
        line(" {\"a\":1}\n", " { \"a\":1}\n"),
        Some((1, r#" {"a":1}"#.into(), r#" { "a":1}"#.into()))
    );
    assert_eq!(line("{}\n", "{}"), Some((1, "{}".into(), "{}".into())));
}

fn write(dir: &MemoryDir, name: &str, content: &str) {
    dir.fs.put(dir.path().join(name), content.as_bytes());
}

#[test]
fn directories_differ_in_the_journal_one_side_lacks_but_not_in_other_files() {
    let difference = |file: &str, line: usize, base: &str, head: &str| {
        Some(Difference {
            file: file.to_owned(),
            line: Some(line),
            base: base.to_owned(),
            head: head.to_owned(),
        })
    };
    for (name, file, content, side, want) in [
        ("equal", "", "", 0, None),
        (
            "head lacks an archive",
            "archive/session.jsonl",
            "{}\n",
            0,
            difference("archive/session.jsonl", 1, "{}", "<missing file>"),
        ),
        (
            "base lacks the live journal",
            "events.jsonl",
            "{}\n",
            1,
            difference("events.jsonl", 1, "<missing file>", "{}"),
        ),
        (
            "an empty file still exists",
            "events.jsonl",
            "",
            0,
            difference("events.jsonl", 1, "<empty file>", "<missing file>"),
        ),
        (
            "samples are not journals",
            "trials/t/samples.jsonl",
            "{}\n",
            1,
            None,
        ),
        (
            "archived samples are not journals",
            "archive/session-trials/t/samples.jsonl",
            "{}\n",
            1,
            None,
        ),
        (
            "a marker is not a journal",
            "archive/s-reset-all",
            "x",
            1,
            None,
        ),
    ] {
        let fs = MemFs::new();
        let dirs = [Path::new("/base"), Path::new("/head")];
        for dir in dirs {
            fs.put(dir.join("archive/common.jsonl"), "{}\n");
        }
        if !file.is_empty() {
            fs.put(dirs[side].join(file), content.as_bytes());
        }
        let got = journal::compare_dirs_with(&fs, dirs[0], dirs[1]).unwrap();
        assert_eq!(got, want, "{name}");
    }
}

#[test]
fn a_side_without_journals_or_events_cannot_be_compared() {
    let dir = MemoryDir::new();
    let a = dir.path().join("base");
    let b = dir.path().join("head");
    dir.fs.put(a.join("events.jsonl"), b"{}\n");
    assert!(matches!(
        journal::compare_dirs_with(&*dir.fs, &a, &b),
        Err(JournalError::MissingOnASide { base: 1, head: 0 })
    ));
    dir.fs.put(b.join("events.jsonl"), b"");
    assert!(matches!(
        journal::compare_dirs_with(&*dir.fs, &a, &b),
        Err(JournalError::EmptyOnASide { side: "head" })
    ));
}

#[test]
fn the_digest_names_every_journal_and_ignores_the_build_and_other_files() {
    let digest = |files: &[(&str, &str)]| {
        let dir = MemoryDir::new();
        for (name, content) in files {
            write(&dir, name, content);
        }
        journal::digest_with(&*dir.fs, dir.path()).unwrap()
    };
    let event = |version: &str| format!("{}\n{{\"seq\":2}}\n", plain().replace("old", version));
    let base = digest(&[("events.jsonl", &event("old"))]);
    assert_eq!(
        digest(&[("events.jsonl", &event("new"))]),
        base,
        "build stamp"
    );
    let changed = format!("{}\n{{\"seq\":3}}\n", plain());
    assert_ne!(digest(&[("events.jsonl", &changed)]), base, "event");
    let unterminated = format!("{}\n{{\"seq\":2}}", plain());
    assert_ne!(
        digest(&[("events.jsonl", &unterminated)]),
        base,
        "final newline"
    );
    assert_ne!(
        digest(&[("events.jsonl", &event("old")), ("archive/a.jsonl", "{}\n")]),
        base,
        "archive"
    );
    assert_eq!(
        digest(&[
            ("events.jsonl", &event("old")),
            ("trials/t/samples.jsonl", "{}\n")
        ]),
        base,
        "samples"
    );
    assert_ne!(
        digest(&[("archive/a.jsonl", "{}\n")]),
        digest(&[("archive/b.jsonl", "{}\n")]),
        "journal names"
    );
}

#[test]
fn no_journals_or_only_empty_ones_have_no_digest() {
    let dir = MemoryDir::new();
    assert!(matches!(
        journal::digest_with(&*dir.fs, dir.path()),
        Err(JournalError::Missing)
    ));
    write(&dir, "events.jsonl", "");
    assert!(matches!(
        journal::digest_with(&*dir.fs, dir.path()),
        Err(JournalError::Empty)
    ));
}

#[derive(Clone)]
struct Behavior {
    journal: Option<String>,
    exit: i32,
    wall: f64,
    fail: bool,
}

type Behave = Box<dyn Fn(Side, &RunSpec) -> Behavior + Sync>;
type Hook<'a> = Box<dyn Fn(Side, &RunSpec) + Sync + 'a>;

struct Fake<'a> {
    root: PathBuf,
    fs: Arc<MemFs>,
    fleet: &'a Fleet,
    behave: Behave,
    hook: Option<Hook<'a>>,
    launches: Mutex<Vec<String>>,
}

impl<'a> Fake<'a> {
    fn new(
        root: &MemoryDir,
        fleet: &'a Fleet,
        behave: impl Fn(Side, &RunSpec) -> Behavior + Sync + 'static,
    ) -> Self {
        Self {
            root: root.path().to_path_buf(),
            fs: Arc::clone(&root.fs),
            fleet,
            behave: Box::new(behave),
            hook: None,
            launches: Mutex::new(Vec::new()),
        }
    }

    fn launches(&self) -> Vec<String> {
        self.launches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn journal(text: &str) -> Behavior {
    Behavior {
        journal: Some(text.to_owned()),
        exit: 0,
        wall: 1.0,
        fail: false,
    }
}

impl Launcher for Fake<'_> {
    fn dir(&self, side: Side, spec: &RunSpec) -> PathBuf {
        self.root.join(side.name()).join(spec.name())
    }

    fn launch(&self, side: Side, spec: &RunSpec) -> Result<Ran, RunError> {
        self.launches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(format!("{} {}", side.name(), spec.name()));
        let dir = self.dir(side, spec);
        self.fs.put(dir.join("sim.log"), b"diagnostic log\n");
        if let Some(hook) = &self.hook {
            hook(side, spec);
        }
        if self.fleet.stopping() {
            return Err(RunError::Interrupted);
        }
        let behavior = (self.behave)(side, spec);
        if behavior.fail {
            return Err(RunError::TimedOut { seconds: 1 });
        }
        if let Some(text) = &behavior.journal {
            self.fs.put(dir.join("events.jsonl"), text.as_bytes());
        }
        Ok(Ran {
            dir,
            exit: behavior.exit,
            wall: Duration::from_secs_f64(behavior.wall),
        })
    }

    fn digest(&self, dir: &Path) -> Result<String, JournalError> {
        journal::digest_with(&*self.fs, dir)
    }

    fn difference(&self, base: &Path, head: &Path) -> Result<Option<Difference>, JournalError> {
        journal::compare_dirs_with(&*self.fs, base, head)
    }

    fn remove(&self, dir: &Path) -> std::io::Result<()> {
        for (path, kind) in self.fs.entries() {
            if kind == FileKind::File && path.starts_with(dir) {
                self.fs.remove_file(&path)?;
            }
        }
        Ok(())
    }
}

fn spec(scenario: &str, split: Split, seed: u64) -> RunSpec {
    RunSpec {
        scenario: scenario.to_owned(),
        split,
        seed,
        machine: None,
        replay: None,
        smoke: false,
    }
}

fn same_sessions(seeds: &[u64]) -> Vec<Pair> {
    let runs = || {
        seeds
            .iter()
            .map(|&seed| spec("s", Split::Dev, seed))
            .collect()
    };
    compare::pair([runs(), runs()])
}

fn config(fleet: &Fleet) -> Config<'_> {
    Config {
        jobs: 2,
        keep: false,
        keep_going: false,
        fresh: false,
        caches: [None, None],
        costs: None,
        scopes: [String::new(), String::new()],
        fleet,
    }
}

fn files_under(root: &MemoryDir) -> Vec<String> {
    root.fs
        .entries()
        .into_iter()
        .filter_map(|(path, kind)| {
            (kind == FileKind::File && path.starts_with(root.path())).then(|| {
                path.strip_prefix(root.path())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .collect()
}

fn report(outcome: &compare::Outcome) -> String {
    let mut out = Vec::new();
    outcome.write(&mut out).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn the_report_names_each_differing_session_and_the_sessions_one_tree_lacks() {
    let root = MemoryDir::new();
    let fleet = Fleet::new();
    let pairs = compare::pair([
        vec![
            spec("scenario", Split::Dev, 1),
            spec("removed", Split::Holdout, 2),
        ],
        vec![
            spec("scenario", Split::Dev, 1),
            spec("added", Split::Holdout, 3),
        ],
    ]);
    let fake = Fake::new(&root, &fleet, |side, spec| match (side, spec.seed) {
        (Side::Base, 1) => journal("{}\n{\"decision\":0}\n"),
        (Side::Head, 1) => Behavior {
            exit: 1,
            ..journal("{}\n{\"decision\":1}\n")
        },
        _ => journal("{}\n"),
    });
    let mut config = config(&fleet);
    config.keep_going = true;
    let outcome = compare::run(&pairs, &config, &fake).unwrap();
    assert!(outcome.different());
    let mut text = String::new();
    for line in report(&outcome)
        .lines()
        .filter(|line| !line.starts_with("same: simulator time"))
    {
        text.push_str(line);
        text.push('\n');
    }
    let expected = include_str!(
        "snapshots/togi_tool_bench__same__tests__the_report_names_each_differing_session_and_the_sessions_one_tree_lacks.snap"
    );
    assert_eq!(
        text,
        expected
            .splitn(3, "---")
            .nth(2)
            .unwrap()
            .trim_start_matches('\n')
    );
}

#[test]
fn a_session_is_identified_by_its_split_and_judged_by_its_exit_status() {
    for (name, splits, exits, different) in [
        (
            "equal nonzero exits",
            [Split::Dev, Split::Dev],
            [1, 1],
            false,
        ),
        ("different exits", [Split::Dev, Split::Dev], [0, 1], true),
        (
            "split is part of identity",
            [Split::Dev, Split::Holdout],
            [0, 0],
            true,
        ),
    ] {
        let root = MemoryDir::new();
        let fleet = Fleet::new();
        let pairs = compare::pair([vec![spec("s", splits[0], 1)], vec![spec("s", splits[1], 1)]]);
        let fake = Fake::new(&root, &fleet, move |side, _| Behavior {
            exit: exits[side.index()],
            ..journal("{}\n")
        });
        let mut config = config(&fleet);
        config.keep_going = true;
        let outcome = compare::run(&pairs, &config, &fake).unwrap();
        assert_eq!(outcome.different(), different, "{name}");
    }
}

#[test]
fn only_differing_sessions_keep_their_runs_unless_asked_to_keep_all() {
    for (keep, want) in [
        (
            false,
            vec![
                "base/differs/dev-1/events.jsonl",
                "base/differs/dev-1/sim.log",
                "head/differs/dev-1/events.jsonl",
                "head/differs/dev-1/sim.log",
            ],
        ),
        (
            true,
            vec![
                "base/differs/dev-1/events.jsonl",
                "base/differs/dev-1/sim.log",
                "base/matches/dev-1/events.jsonl",
                "base/matches/dev-1/sim.log",
                "head/differs/dev-1/events.jsonl",
                "head/differs/dev-1/sim.log",
                "head/matches/dev-1/events.jsonl",
                "head/matches/dev-1/sim.log",
            ],
        ),
    ] {
        let root = MemoryDir::new();
        let fleet = Fleet::new();
        let pairs = compare::pair([
            vec![
                spec("matches", Split::Dev, 1),
                spec("differs", Split::Dev, 1),
            ],
            vec![
                spec("matches", Split::Dev, 1),
                spec("differs", Split::Dev, 1),
            ],
        ]);
        let fake = Fake::new(&root, &fleet, |side, spec| Behavior {
            exit: i32::from(side == Side::Head && spec.scenario == "differs"),
            ..journal("{}\n")
        });
        let mut config = config(&fleet);
        config.keep = keep;
        config.keep_going = true;
        compare::run(&pairs, &config, &fake).unwrap();
        assert_eq!(files_under(&root), want, "keep={keep}");
    }
}

#[test]
fn a_matching_session_is_removed_before_the_next_one_starts() {
    for keep in [false, true] {
        let root = MemoryDir::new();
        let fleet = Fleet::new();
        let pairs = compare::pair([
            vec![spec("first", Split::Dev, 1), spec("second", Split::Dev, 1)],
            vec![spec("first", Split::Dev, 1), spec("second", Split::Dev, 1)],
        ]);
        let mut fake = Fake::new(&root, &fleet, |_, _| journal("{}\n"));
        let seen = root.path().to_path_buf();
        let fs = Arc::clone(&root.fs);
        fake.hook = Some(Box::new(move |side, spec| {
            if side == Side::Base && spec.scenario == "second" {
                for name in ["base", "head"] {
                    let path = seen.join(name).join("first/dev-1");
                    let present = fs
                        .entries()
                        .iter()
                        .any(|(entry, kind)| *kind == FileKind::File && entry.starts_with(&path));
                    assert_eq!(present, keep, "{name} first/dev-1 with keep={keep}");
                }
            }
        }));
        let mut config = config(&fleet);
        config.jobs = 1;
        config.keep = keep;
        let outcome = compare::run(&pairs, &config, &fake).unwrap();
        assert!(!outcome.different());
    }
}

#[test]
fn a_failed_run_stops_the_comparison_and_leaves_its_own_diagnostics() {
    for failed_side in Side::BOTH {
        let root = MemoryDir::new();
        let fleet = Fleet::new();
        let pairs = compare::pair([
            vec![
                spec("failed", Split::Dev, 1),
                spec("matches", Split::Dev, 1),
            ],
            vec![
                spec("failed", Split::Dev, 1),
                spec("matches", Split::Dev, 1),
            ],
        ]);
        let fake = Fake::new(&root, &fleet, move |side, spec| Behavior {
            fail: side == failed_side && spec.scenario == "failed",
            ..journal("{}\n")
        });
        let mut config = config(&fleet);
        config.jobs = 1;
        let error = compare::run(&pairs, &config, &fake).unwrap_err();
        let SameError::Run { side, key, .. } = &error else {
            panic!("want a run error, got {error}");
        };
        assert_eq!((*side, key.as_str()), (failed_side.name(), "failed/dev-1"));
        assert_eq!(
            String::from_utf8(
                root.fs
                    .read(
                        &root
                            .path()
                            .join(failed_side.name())
                            .join("failed/dev-1/sim.log")
                    )
                    .unwrap()
            )
            .unwrap(),
            "diagnostic log\n"
        );
        let left = files_under(&root);
        assert!(
            left.iter()
                .all(|path| path.ends_with("failed/dev-1/sim.log")),
            "{left:?}"
        );
        assert_eq!(
            left.len(),
            1,
            "the unfinished pair's other run is removed: {left:?}"
        );
    }
}

#[test]
fn sessions_without_journals_or_events_never_compare_equal() {
    for (journal, message) in [
        (None, "missing journals"),
        (Some(String::new()), "journals contain no events"),
    ] {
        let root = MemoryDir::new();
        let fleet = Fleet::new();
        let fake = Fake::new(&root, &fleet, move |_, _| Behavior {
            journal: journal.clone(),
            exit: 1,
            wall: 1.0,
            fail: false,
        });
        let error = compare::run(&same_sessions(&[1]), &config(&fleet), &fake).unwrap_err();
        let chain = togi_machine::error_chain(&error);
        assert!(chain.contains(message), "{chain}");
    }
}

#[test]
fn a_failed_comparison_renders_its_cause_once() {
    let failed = super::Failed {
        error: Box::new(SameError::Remove {
            side: "base",
            key: "x/dev-1".to_owned(),
            source: std::io::Error::other("disk gone"),
        }),
        kept: None,
    };
    assert_eq!(
        togi_machine::error_chain(&failed),
        "remove matching base x/dev-1: disk gone"
    );
}

struct Cached {
    _dir: MemoryDir,
    caches: [DigestCache; 2],
    costs: Costs,
}

fn cached() -> Cached {
    let dir = MemoryDir::new();
    let caches = ["base", "head"].map(|key| DigestCache::with_fs(dir.path(), key, dir.fs.clone()));
    let costs = Costs::with_fs(dir.path().join("costs.json"), dir.fs.clone());
    Cached {
        _dir: dir,
        caches,
        costs,
    }
}

impl Cached {
    fn config<'a>(&'a self, fleet: &'a Fleet) -> Config<'a> {
        Config {
            caches: [Some(&self.caches[0]), Some(&self.caches[1])],
            costs: Some(&self.costs),
            scopes: ["base".to_owned(), "head".to_owned()],
            ..config(fleet)
        }
    }
}

#[test]
fn both_sides_are_cached_and_a_fresh_run_ignores_the_cache() {
    let cache = cached();
    let root = MemoryDir::new();
    let fleet = Fleet::new();
    let fake = Fake::new(&root, &fleet, |_, _| journal("{}\n"));
    let pairs = same_sessions(&[1, 2, 3]);
    let config = cache.config(&fleet);
    for run in 0..2 {
        let outcome = compare::run(&pairs, &config, &fake).unwrap();
        assert!(!outcome.different(), "run {run}");
        assert_eq!(fake.launches().len(), 6, "run {run}");
    }
    assert!(cache.costs.get("base", "s/dev-2").is_some());
    assert!(files_under(&root).is_empty());
    let mut fresh = cache.config(&fleet);
    fresh.fresh = true;
    compare::run(&pairs, &fresh, &fake).unwrap();
    assert_eq!(fake.launches().len(), 12);
}

#[test]
fn a_difference_reruns_only_the_sides_that_have_no_run_to_show() {
    let cache = cached();
    let root = MemoryDir::new();
    let fleets = [Fleet::new(), Fleet::new(), Fleet::new()];
    let fake = |fleet| {
        Fake::new(&root, fleet, |side, _| {
            journal(&format!("{{}}\n{{\"side\":{}}}\n", side.index()))
        })
    };
    let pairs = same_sessions(&[1]);
    let want = "same: s/dev-1\n  events.jsonl:2\n  base: {\"side\":0}\n  head: {\"side\":1}\n";
    let cold = fake(&fleets[0]);
    let outcome = compare::run(&pairs, &cache.config(&fleets[0]), &cold).unwrap();
    assert!(report(&outcome).starts_with(want), "{}", report(&outcome));
    assert_eq!(cold.launches().len(), 2);
    let warm = fake(&fleets[1]);
    let outcome = compare::run(&pairs, &cache.config(&fleets[1]), &warm).unwrap();
    assert!(report(&outcome).starts_with(want));
    assert_eq!(
        warm.launches().len(),
        2,
        "both cached sides rerun to show the line"
    );
    let head_only = fake(&fleets[2]);
    let mut head_fresh = cache.config(&fleets[2]);
    head_fresh.caches[1] = None;
    compare::run(&pairs, &head_fresh, &head_only).unwrap();
    assert_eq!(
        head_only.launches(),
        ["head s/dev-1", "base s/dev-1"],
        "a fresh head runs once and the cached base once more to show the line"
    );
}

#[test]
fn the_comparison_stops_at_the_first_difference_unless_told_to_go_on() {
    for keep_going in [false, true] {
        let root = MemoryDir::new();
        let fleet = Fleet::new();
        let fake = Fake::new(&root, &fleet, |side, spec| {
            if spec.seed == 1 && side == Side::Head {
                journal("{\"changed\":true}\n")
            } else {
                journal("{}\n")
            }
        });
        let mut config = config(&fleet);
        config.jobs = 1;
        config.keep_going = keep_going;
        let outcome = compare::run(&same_sessions(&[1, 2, 3, 4]), &config, &fake).unwrap();
        assert!(outcome.different());
        let (launches, summary) = if keep_going {
            (8, "same: 1 of 4 sessions differ")
        } else {
            (2, "same: stopped at the first difference")
        };
        assert_eq!(fake.launches().len(), launches, "keep_going={keep_going}");
        assert!(report(&outcome).contains(summary), "{}", report(&outcome));
        assert_eq!(
            files_under(&root),
            [
                "base/s/dev-1/events.jsonl",
                "base/s/dev-1/sim.log",
                "head/s/dev-1/events.jsonl",
                "head/s/dev-1/sim.log"
            ]
        );
    }
}

#[test]
fn an_interrupt_removes_every_run_and_reports_nothing() {
    let root = MemoryDir::new();
    let fleet = Fleet::new();
    let mut fake = Fake::new(&root, &fleet, |_, _| journal("{}\n"));
    fake.hook = Some(Box::new(|_, _| fleet.cancel()));
    let error = compare::run(&same_sessions(&[1, 2, 3, 4]), &config(&fleet), &fake).unwrap_err();
    assert!(matches!(error, SameError::Interrupted), "{error}");
    assert!(files_under(&root).is_empty(), "{:?}", files_under(&root));
    assert!(
        fake.launches().len() <= 2,
        "runs kept starting: {:?}",
        fake.launches()
    );
}

#[test]
fn a_differing_session_keeps_the_journals_of_its_cached_sides() {
    for (name, missing_on_head, exits) in [
        ("exit code", false, [0, 1]),
        ("missing on head", true, [0, 0]),
    ] {
        let cache = cached();
        let fleets = [Fleet::new(), Fleet::new()];
        let pairs = || {
            let mut pairs = same_sessions(&[1]);
            if missing_on_head {
                pairs[0].runs[1] = None;
            }
            pairs
        };
        let behave = move |side: Side, _: &RunSpec| Behavior {
            exit: exits[side.index()],
            ..journal("{}\n")
        };
        let mut config = cache.config(&fleets[0]);
        config.keep = true;
        let cold = MemoryDir::new();
        let first = Fake::new(&cold, &fleets[0], behave);
        assert!(
            compare::run(&pairs(), &config, &first).unwrap().different(),
            "{name}"
        );
        let root = MemoryDir::new();
        let second = Fake::new(&root, &fleets[1], behave);
        let mut cached_config = cache.config(&fleets[1]);
        cached_config.keep = true;
        assert!(
            compare::run(&pairs(), &cached_config, &second)
                .unwrap()
                .different(),
            "{name}"
        );
        let want: Vec<&str> = if missing_on_head {
            vec!["base/s/dev-1/events.jsonl", "base/s/dev-1/sim.log"]
        } else {
            vec![
                "base/s/dev-1/events.jsonl",
                "base/s/dev-1/sim.log",
                "head/s/dev-1/events.jsonl",
                "head/s/dev-1/sim.log",
            ]
        };
        assert_eq!(files_under(&root), want, "{name}");
    }
}

#[test]
fn sessions_that_race_to_a_difference_report_one_when_stopping_at_the_first() {
    let cache = cached();
    let fleet = Fleet::new();
    let changed = |side: Side, _: &RunSpec| journal(&format!("{{\"side\":{}}}\n", side.index()));
    let cold = MemoryDir::new();
    let mut keep_going = cache.config(&fleet);
    keep_going.keep_going = true;
    compare::run(
        &same_sessions(&[1, 2]),
        &keep_going,
        &Fake::new(&cold, &fleet, changed),
    )
    .unwrap();
    let root = MemoryDir::new();
    let barrier = Barrier::new(2);
    let reruns = Arc::new(AtomicUsize::new(0));
    let finished = Arc::clone(&reruns);
    // Both sessions pass the stop check of their last rerun before either can report, so neither is
    // interrupted: the one that loses the race to report must drop its runs.
    let fake = Fake::new(&root, &fleet, move |side, _| {
        if side == Side::Head {
            barrier.wait();
            finished.fetch_add(1, Ordering::SeqCst);
        }
        journal(&format!("{{\"side\":{}}}\n", side.index()))
    });
    let outcome = compare::run(&same_sessions(&[1, 2]), &cache.config(&fleet), &fake).unwrap();
    assert_eq!(reruns.load(Ordering::SeqCst), 2);
    assert_eq!(outcome.differences.len(), 1);
    let reported = outcome.differences[0].key.to_string();
    let mut want: Vec<String> = ["base", "head"]
        .into_iter()
        .flat_map(|side| {
            ["events.jsonl", "sim.log"].map(|file| format!("{side}/{reported}/{file}"))
        })
        .collect();
    let mut files = files_under(&root);
    files.sort();
    want.sort();
    assert_eq!(files, want, "only the reported session's runs remain");
}

#[test]
fn a_rerun_cancelled_for_an_interrupt_leaves_its_directory_nowhere() {
    let cache = cached();
    let fleet = Fleet::new();
    let changed = |side: Side, _: &RunSpec| journal(&format!("{{\"side\":{}}}\n", side.index()));
    let cold = MemoryDir::new();
    compare::run(
        &same_sessions(&[1]),
        &cache.config(&fleet),
        &Fake::new(&cold, &fleet, changed),
    )
    .unwrap();
    let root = MemoryDir::new();
    let mut fake = Fake::new(&root, &fleet, changed);
    fake.hook = Some(Box::new(|_, _| fleet.cancel()));
    let error = compare::run(&same_sessions(&[1]), &cache.config(&fleet), &fake).unwrap_err();
    assert!(matches!(error, SameError::Interrupted), "{error}");
    assert!(files_under(&root).is_empty());
}

#[test]
fn the_time_of_the_compared_sessions_is_reported_without_failing() {
    let cache = cached();
    let root = MemoryDir::new();
    let fleet = Fleet::new();
    let times: BTreeMap<(usize, u64), f64> = [
        ((0, 1), 10.0),
        ((1, 1), 21.0),
        ((0, 2), 10.0),
        ((1, 2), 20.0),
        ((0, 3), 1.0),
        ((1, 3), 5.0),
    ]
    .into();
    let fake = Fake::new(&root, &fleet, move |side, spec| Behavior {
        wall: times[&(side.index(), spec.seed)],
        ..journal("{}\n")
    });
    let want = "same: simulator time of the compared sessions: base 21.0s, head 46.0s\n\
        same: 2 sessions took more than 2x their base's time (informational)\n\
        \x20 s/dev-3: head 5.0s, base 1.0s, 5.0x\n\
        \x20 s/dev-1: head 21.0s, base 10.0s, 2.1x\n\
        same: 0 of 3 sessions differ\n";
    for run in ["ran", "cached"] {
        let outcome =
            compare::run(&same_sessions(&[1, 2, 3]), &cache.config(&fleet), &fake).unwrap();
        assert!(!outcome.different(), "{run}");
        assert_eq!(report(&outcome), want, "{run}");
    }
}

#[test]
fn only_the_slowest_sessions_are_listed_and_the_rest_counted() {
    let root = MemoryDir::new();
    let fleet = Fleet::new();
    let fake = Fake::new(&root, &fleet, |side, _| Behavior {
        wall: if side == Side::Base { 1.0 } else { 5.0 },
        ..journal("{}\n")
    });
    let seeds: Vec<u64> = (1..=12).collect();
    let outcome = compare::run(&same_sessions(&seeds), &config(&fleet), &fake).unwrap();
    let text = report(&outcome);
    assert!(
        text.contains("same: 12 sessions took more than 2x their base's time (informational)"),
        "{text}"
    );
    assert_eq!(text.matches("5.0x").count(), 10, "{text}");
    assert!(text.contains("  and 2 more\n"), "{text}");
}

#[test]
fn real_fs_a_digest_cache_keeps_the_most_recent_trees_only() {
    let dir = tempfile::tempdir().unwrap();
    for key in (0..10).map(|n| format!("key-{n}")) {
        DigestCache::open(dir.path(), &key).unwrap();
    }
    let kept = std::fs::read_dir(dir.path().join("same")).unwrap().count();
    assert_eq!(kept, 8);
    assert!(dir.path().join("same/key-9").is_dir());
}

#[test]
fn a_session_s_inputs_change_with_any_file_of_its_machine() {
    use crate::suite::Machine;
    use togi_sim::MachineDefinition;
    let with = |name: &str, sum: &str| {
        let mut run = spec("s", Split::Dev, 1);
        run.machine = Some(Arc::new(Machine {
            name: name.to_owned(),
            path: PathBuf::from(name),
            definition: MachineDefinition::default(),
            inputs: vec![(PathBuf::from(name), Some(sum.to_owned()))],
        }));
        super::digests::inputs(&run)
    };
    let plain = super::digests::inputs(&spec("s", Split::Dev, 1));
    let base = with("m.json", "aa");
    assert_ne!(base, plain);
    assert_ne!(base, with("m.json", "bb"));
    assert_ne!(base, with("n.json", "aa"));
    assert_eq!(base, with("m.json", "aa"));
}

#[test]
fn a_session_s_inputs_change_with_its_identity_and_replay() {
    use togi_sim::replay::Replay;
    let base = spec("s", Split::Dev, 1);
    let want = super::digests::inputs(&base);
    assert_eq!(want, super::digests::inputs(&spec("s", Split::Dev, 1)));
    let mut replaying = base.clone();
    replaying.replay = Some(Arc::new(Replay::default()));
    for (what, other) in [
        ("seed", spec("s", Split::Dev, 2)),
        ("split", spec("s", Split::Holdout, 1)),
        ("name", spec("t", Split::Dev, 1)),
        ("replay", replaying),
    ] {
        assert_ne!(super::digests::inputs(&other), want, "{what}");
    }
}

#[test]
fn the_digest_key_changes_with_the_simulator_and_the_boot_cap() {
    let base = super::digests::key("simulator", 1000);
    assert_eq!(base, super::digests::key("simulator", 1000));
    assert_ne!(base, super::digests::key("simulator 2", 1000), "simulator");
    assert_ne!(base, super::digests::key("simulator", 5), "boot cap");
}

#[test]
fn a_digest_record_holds_only_for_the_inputs_that_made_it() {
    let dir = MemoryDir::new();
    let cache = DigestCache::with_fs(dir.path(), "k", dir.fs.clone());
    let run = spec("s", Split::Dev, 1);
    assert_eq!(cache.get(&run, "in"), None, "an empty cache hits");
    let record = super::digests::SideRecord {
        digest: "abc".to_owned(),
        exit: 3,
        wall_s: 1.5,
    };
    cache.put(&run, "in", &record).unwrap();
    assert_eq!(cache.get(&run, "in"), Some(record));
    assert_eq!(
        cache.get(&run, "other"),
        None,
        "a record made from other inputs hits"
    );
    assert_eq!(cache.get(&spec("s", Split::Dev, 2), "in"), None);
}

#[test]
fn a_corrupt_digest_record_misses() {
    let dir = MemoryDir::new();
    let cache = DigestCache::with_fs(dir.path(), "k", dir.fs.clone());
    let run = spec("s", Split::Dev, 1);
    let record = super::digests::SideRecord {
        digest: "abc".to_owned(),
        exit: 0,
        wall_s: 1.0,
    };
    cache.put(&run, "in", &record).unwrap();
    let path = dir.path().join("same/k/s/dev-1.json");
    for (what, bytes) in [
        ("truncated", "{"),
        ("an array", "[]"),
        ("another shape", "{\"inputs\":\"in\",\"record\":{}}"),
    ] {
        dir.fs.put(&path, bytes.as_bytes());
        assert_eq!(cache.get(&run, "in"), None, "{what}");
    }
}

const SUITE: &str = r#"{"scenarios":[{"name":"default","dev":[1,2],"holdout":[101],"smoke":[2]},{"name":"late","dev":[1],"smoke":[1]}]}"#;

fn options() -> super::Options {
    super::Options {
        base: PathBuf::from("/base"),
        head: PathBuf::from("/head"),
        suite: PathBuf::from("suite.json"),
        smoke: false,
        keep_going: false,
        no_cache: false,
        keep: None,
        jobs: std::num::NonZeroUsize::MIN,
        timeout: Duration::from_secs(1),
        max_boots: std::num::NonZeroU32::MIN,
        cache: PathBuf::from("/cache"),
        simulators: [None, None],
    }
}

fn plan(bytes: &str, path: &Path) -> Result<Vec<RunSpec>, crate::error::Error> {
    crate::suite::Suite::parse(bytes.as_bytes(), path.parent().unwrap().to_path_buf())?
        .plan(crate::suite::Selection::All)
        .map(|plan| plan.runs)
}

#[test]
fn two_trees_that_build_the_same_simulator_from_the_same_inputs_run_nothing() {
    let options = options();
    let trees = [options.base.clone(), options.head.clone()];
    let pairs =
        compare::pair(super::load_runs_with(&options, &trees, &|path| plan(SUITE, path)).unwrap());
    let keys = ["same binary".to_owned(), "same binary".to_owned()];
    assert!(super::same_inputs(&keys, &pairs));
    let done =
        super::identical_report(&options, pairs.len(), true, super::Timings::default()).unwrap();
    assert!(done.identical && !done.different());
    assert_eq!(done.outcome.scheduled, [0, 0]);
    let mut text = Vec::new();
    done.write(&mut text).unwrap();
    assert_eq!(
        String::from_utf8(text).unwrap(),
        "same: both trees build the same simulator and read the same inputs; 0 of 4 sessions differ\n"
    );
    assert!(!super::same_inputs(
        &["base".to_owned(), "head".to_owned()],
        &pairs
    ));
    let mut missing = pairs.clone();
    missing[0].runs[1] = None;
    assert!(!super::same_inputs(&keys, &missing));
    let mut changed = pairs;
    changed[0].inputs[1].push_str("changed");
    assert!(!super::same_inputs(&keys, &changed));
    for (no_cache, keep) in [(true, false), (false, true)] {
        let mut options = options.clone();
        options.no_cache = no_cache;
        options.keep = keep.then(|| PathBuf::from("/kept"));
        assert!(super::identical_report(&options, 4, true, super::Timings::default()).is_none());
    }
}

#[test]
fn smoke_compares_only_the_sessions_the_head_suite_lists() {
    let mut options = options();
    options.smoke = true;
    let trees = [options.base.clone(), options.head.clone()];
    let base = r#"{"scenarios":[{"name":"default","dev":[1,2],"holdout":[101],"smoke":[1]},{"name":"late","dev":[1]}]}"#;
    let runs = super::load_runs_with(&options, &trees, &|path| {
        plan(
            if path.starts_with("/base") {
                base
            } else {
                SUITE
            },
            path,
        )
    })
    .unwrap();
    assert_eq!(
        runs[0]
            .iter()
            .map(|spec| (spec.scenario.as_str(), spec.seed))
            .collect::<Vec<_>>(),
        [("default", 2), ("late", 1)]
    );
    let root = MemoryDir::new();
    let fleet = Fleet::new();
    let fake = Fake::new(&root, &fleet, |_, _| journal("{}\n"));
    let done = compare::run(&compare::pair(runs), &config(&fleet), &fake).unwrap();
    assert!(!done.different());
    assert!(report(&done).ends_with("same: 0 of 2 sessions differ\n"));
    assert_eq!(
        fake.launches()
            .iter()
            .filter(|name| name.starts_with("head "))
            .count(),
        2
    );
}

#[test]
fn smoke_without_smoke_sessions_is_an_error() {
    let mut options = options();
    options.smoke = true;
    let trees = [options.base.clone(), options.head.clone()];
    let failed = super::load_runs_with(&options, &trees, &|path| {
        plan(r#"{"scenarios":[{"name":"default","dev":[1]}]}"#, path)
    })
    .unwrap_err();
    assert!(matches!(failed, SameError::NoSmoke), "{failed}");
}

#[test]
fn a_tree_whose_suite_cannot_be_read_is_an_error_naming_it() {
    let options = options();
    let trees = [options.base.clone(), options.head.clone()];
    let failed = super::load_runs_with(&options, &trees, &|path| {
        plan(
            if path.starts_with("/head") {
                "not json"
            } else {
                SUITE
            },
            path,
        )
    })
    .unwrap_err();
    assert!(
        failed.to_string().starts_with("load the head suite in "),
        "{failed}"
    );
}

#[test]
fn an_absolute_suite_path_is_read_from_the_same_file_in_both_trees() {
    let mut options = options();
    options.suite = PathBuf::from("/base/suite.json");
    let trees = [options.base.clone(), options.head.clone()];
    let paths = std::cell::RefCell::new(Vec::new());
    let runs = super::load_runs_with(&options, &trees, &|path| {
        paths.borrow_mut().push(path.to_path_buf());
        plan(SUITE, path)
    })
    .unwrap();
    assert_eq!(*paths.borrow(), [options.suite.clone(), options.suite]);
    let root = MemoryDir::new();
    let fleet = Fleet::new();
    let fake = Fake::new(&root, &fleet, |_, _| journal("{}\n"));
    let done = compare::run(&compare::pair(runs), &config(&fleet), &fake).unwrap();
    assert!(!done.different());
    assert!(report(&done).contains("0 of 4 sessions differ"));
}

#[test]
fn real_fs_journal_discovery_uses_file_types_and_archive_names() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("archive")).unwrap();
    symlink("archive/a.jsonl", dir.path().join("events.jsonl")).unwrap();
    std::fs::write(dir.path().join("archive/a.jsonl"), "{}\n").unwrap();
    std::fs::write(dir.path().join("archive/marker"), "x").unwrap();
    std::fs::create_dir(dir.path().join("archive/directory.jsonl")).unwrap();
    symlink("a.jsonl", dir.path().join("archive/link.jsonl")).unwrap();
    assert_eq!(
        journal::files(dir.path())
            .unwrap()
            .into_keys()
            .collect::<Vec<_>>(),
        ["archive/a.jsonl", "events.jsonl"]
    );
}

#[test]
fn real_fs_pruning_removes_empty_branches_without_following_symlinks() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("empty/nested")).unwrap();
    std::fs::create_dir_all(root.path().join("kept/nested")).unwrap();
    std::fs::write(root.path().join("kept/nested/events.jsonl"), "{}\n").unwrap();
    std::fs::create_dir(outside.path().join("untouched")).unwrap();
    symlink(outside.path(), root.path().join("link")).unwrap();
    compare::remove_empty_dirs(root.path());
    assert!(root.path().is_dir());
    assert!(!root.path().join("empty").exists());
    assert!(root.path().join("kept/nested/events.jsonl").is_file());
    assert!(root.path().join("link").is_symlink());
    assert!(outside.path().join("untouched").is_dir());
}

fn full_pairs() -> Vec<Pair> {
    let options = options();
    compare::pair(
        super::load_runs_with(
            &options,
            &[options.base.clone(), options.head.clone()],
            &|path| plan(SUITE, path),
        )
        .unwrap(),
    )
}

#[test]
fn identical_simulators_still_run_when_asked_to_keep_or_ignore_the_cache() {
    for (no_cache, keep) in [(true, false), (false, true)] {
        let cache = cached();
        let root = MemoryDir::new();
        let fleet = Fleet::new();
        let fake = Fake::new(&root, &fleet, |_, _| journal("{}\n"));
        let mut config = cache.config(&fleet);
        config.fresh = no_cache || keep;
        config.keep = keep;
        let done = compare::run(&full_pairs(), &config, &fake).unwrap();
        assert!(!done.different());
        assert_eq!(done.scheduled, [4, 4]);
        assert_eq!(fake.launches().len(), 8, "no_cache={no_cache} keep={keep}");
        assert_eq!(!files_under(&root).is_empty(), keep);
    }
}

#[test]
fn a_build_stamp_alone_is_not_a_difference_and_a_decision_is() {
    let text = format!("{}\n{{\"seq\":2}}\n{{\"seq\":3,\"decision\":0}}\n", plain());
    let stamped = text.replace("old", "new");
    let root = MemoryDir::new();
    let fleet = Fleet::new();
    let base = text.clone();
    let fake = Fake::new(&root, &fleet, move |side, _| {
        journal(if side == Side::Base { &base } else { &stamped })
    });
    let done = compare::run(&full_pairs(), &config(&fleet), &fake).unwrap();
    assert!(!done.different());
    assert_eq!(
        report(&done).lines().last(),
        Some("same: 0 of 4 sessions differ")
    );
    assert_eq!(
        fake.launches()
            .iter()
            .filter(|name| name.starts_with("base "))
            .count(),
        4
    );
    assert_eq!(
        fake.launches()
            .iter()
            .filter(|name| name.starts_with("head "))
            .count(),
        4
    );

    let changed = text.replacen("\"seq\":3,", "\"seq\":3000,", 1);
    assert_ne!(changed, text);
    let root = MemoryDir::new();
    let fleet = Fleet::new();
    let fake = Fake::new(&root, &fleet, move |side, _| {
        journal(if side == Side::Base { &text } else { &changed })
    });
    let mut config = config(&fleet);
    config.jobs = 1;
    let done = compare::run(&full_pairs(), &config, &fake).unwrap();
    assert!(done.different());
    let text = report(&done);
    assert!(
        text.starts_with("same: default/dev-1\n  events.jsonl:3\n"),
        "{text}"
    );
    assert!(text.ends_with(
        "same: stopped at the first difference; --keep-going reports every differing session\n"
    ));
    let kept = files_under(&root);
    assert!(kept.contains(&"base/default/dev-1/events.jsonl".to_owned()));
    assert!(kept.contains(&"head/default/dev-1/events.jsonl".to_owned()));
}

#[test]
fn a_second_comparison_of_the_same_binaries_runs_nothing_again() {
    let cache = cached();
    let root = MemoryDir::new();
    let fleet = Fleet::new();
    let text = format!("{}\n{{\"seq\":2}}\n", plain());
    let stamped = text.replace("old", "new");
    let fake = Fake::new(&root, &fleet, move |side, _| {
        journal(if side == Side::Base { &text } else { &stamped })
    });
    compare::run(&full_pairs(), &cache.config(&fleet), &fake).unwrap();
    let done = compare::run(&full_pairs(), &cache.config(&fleet), &fake).unwrap();
    assert_eq!(done.cached, [4, 4]);
    assert_eq!(done.scheduled, [0, 0]);
    assert_eq!(
        fake.launches()
            .iter()
            .filter(|name| name.starts_with("base "))
            .count(),
        4
    );
    assert_eq!(
        fake.launches()
            .iter()
            .filter(|name| name.starts_with("head "))
            .count(),
        4
    );
}

#[test]
fn a_session_one_suite_lacks_is_a_difference() {
    let options = options();
    let head = r#"{"scenarios":[{"name":"default","dev":[1,2],"holdout":[101],"smoke":[2]}]}"#;
    let runs = super::load_runs_with(
        &options,
        &[options.base.clone(), options.head.clone()],
        &|path| {
            plan(
                if path.starts_with("/head") {
                    head
                } else {
                    SUITE
                },
                path,
            )
        },
    )
    .unwrap();
    let root = MemoryDir::new();
    let fleet = Fleet::new();
    let fake = Fake::new(&root, &fleet, |_, _| journal("{}\n"));
    let mut config = config(&fleet);
    config.keep_going = true;
    let done = compare::run(&compare::pair(runs), &config, &fake).unwrap();
    let text: String = report(&done)
        .lines()
        .filter(|line| !line.starts_with("same: simulator time"))
        .flat_map(|line| [line, "\n"])
        .collect();
    assert_eq!(
        text,
        "same: late/dev-1\n  <session>\n  base: <present>\n  head: <missing>\nsame: 1 of 4 sessions differ\n"
    );
}

#[test]
fn a_simulator_that_leaves_no_journal_fails_the_comparison_and_keeps_the_runs() {
    let root = MemoryDir::new();
    let fleet = Fleet::new();
    let fake = Fake::new(&root, &fleet, |side, _| {
        if side == Side::Base {
            journal("{}\n")
        } else {
            Behavior {
                exit: 1,
                ..journal("")
            }
        }
    });
    let mut config = config(&fleet);
    config.jobs = 1;
    let failed = compare::run(&full_pairs(), &config, &fake).unwrap_err();
    let chain = togi_machine::error_chain(&failed);
    assert!(chain.contains("journals contain no events"), "{chain}");
    let kept = files_under(&root);
    assert!(
        kept.contains(&"head/default/dev-1/sim.log".to_owned()),
        "{kept:?}"
    );
    assert!(
        kept.contains(&"head/default/dev-1/events.jsonl".to_owned()),
        "{kept:?}"
    );
}
