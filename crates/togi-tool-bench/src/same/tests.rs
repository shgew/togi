use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Barrier, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;

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

fn write(dir: &Path, name: &str, content: &str) {
    let path = dir.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
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
        let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        for dir in &dirs {
            write(dir.path(), "archive/common.jsonl", "{}\n");
        }
        if !file.is_empty() {
            write(dirs[side].path(), file, content);
        }
        let got = journal::compare_dirs(dirs[0].path(), dirs[1].path()).unwrap();
        assert_eq!(got, want, "{name}");
    }
}

#[test]
fn a_side_without_journals_or_events_cannot_be_compared() {
    let [a, b] = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    write(a.path(), "events.jsonl", "{}\n");
    assert!(matches!(
        journal::compare_dirs(a.path(), b.path()),
        Err(JournalError::MissingOnASide { base: 1, head: 0 })
    ));
    write(b.path(), "events.jsonl", "");
    assert!(matches!(
        journal::compare_dirs(a.path(), b.path()),
        Err(JournalError::EmptyOnASide { side: "head" })
    ));
}

#[test]
fn the_digest_names_every_journal_and_ignores_the_build_and_other_files() {
    let digest = |files: &[(&str, &str)]| {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in files {
            write(dir.path(), name, content);
        }
        journal::digest(dir.path()).unwrap()
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
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        journal::digest(dir.path()),
        Err(JournalError::Missing)
    ));
    write(dir.path(), "events.jsonl", "");
    assert!(matches!(
        journal::digest(dir.path()),
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
    fleet: &'a Fleet,
    behave: Behave,
    hook: Option<Hook<'a>>,
    launches: Mutex<Vec<String>>,
}

impl<'a> Fake<'a> {
    fn new(
        root: &Path,
        fleet: &'a Fleet,
        behave: impl Fn(Side, &RunSpec) -> Behavior + Sync + 'static,
    ) -> Self {
        Self {
            root: root.to_path_buf(),
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
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("sim.log"), "diagnostic log\n").unwrap();
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
            fs::write(dir.join("events.jsonl"), text).unwrap();
        }
        Ok(Ran {
            dir,
            exit: behavior.exit,
            wall: Duration::from_secs_f64(behavior.wall),
        })
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

fn files_under(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, files: &mut Vec<String>) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, root, files);
            } else {
                files.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    let mut files = Vec::new();
    walk(root, root, &mut files);
    files
}

fn report(outcome: &compare::Outcome) -> String {
    let mut out = Vec::new();
    outcome.write(&mut out).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn the_report_names_each_differing_session_and_the_sessions_one_tree_lacks() {
    let root = tempfile::tempdir().unwrap();
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
    let fake = Fake::new(root.path(), &fleet, |side, spec| match (side, spec.seed) {
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
    insta::assert_snapshot!(text);
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
        let root = tempfile::tempdir().unwrap();
        let fleet = Fleet::new();
        let pairs = compare::pair([vec![spec("s", splits[0], 1)], vec![spec("s", splits[1], 1)]]);
        let fake = Fake::new(root.path(), &fleet, move |side, _| Behavior {
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
        let root = tempfile::tempdir().unwrap();
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
        let fake = Fake::new(root.path(), &fleet, |side, spec| Behavior {
            exit: i32::from(side == Side::Head && spec.scenario == "differs"),
            ..journal("{}\n")
        });
        let mut config = config(&fleet);
        config.keep = keep;
        config.keep_going = true;
        compare::run(&pairs, &config, &fake).unwrap();
        compare::remove_empty_dirs(root.path());
        assert_eq!(files_under(root.path()), want, "keep={keep}");
    }
}

#[test]
fn a_matching_session_is_removed_before_the_next_one_starts() {
    for keep in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let fleet = Fleet::new();
        let pairs = compare::pair([
            vec![spec("first", Split::Dev, 1), spec("second", Split::Dev, 1)],
            vec![spec("first", Split::Dev, 1), spec("second", Split::Dev, 1)],
        ]);
        let mut fake = Fake::new(root.path(), &fleet, |_, _| journal("{}\n"));
        let seen = root.path().to_path_buf();
        fake.hook = Some(Box::new(move |side, spec| {
            if side == Side::Base && spec.scenario == "second" {
                for name in ["base", "head"] {
                    let present = seen.join(name).join("first/dev-1").exists();
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
        let root = tempfile::tempdir().unwrap();
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
        let fake = Fake::new(root.path(), &fleet, move |side, spec| Behavior {
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
            fs::read_to_string(
                root.path()
                    .join(failed_side.name())
                    .join("failed/dev-1/sim.log")
            )
            .unwrap(),
            "diagnostic log\n"
        );
        let left = files_under(root.path());
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
        let root = tempfile::tempdir().unwrap();
        let fleet = Fleet::new();
        let fake = Fake::new(root.path(), &fleet, move |_, _| Behavior {
            journal: journal.clone(),
            exit: 1,
            wall: 1.0,
            fail: false,
        });
        let error = compare::run(&same_sessions(&[1]), &config(&fleet), &fake).unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
    }
}

struct Cached {
    _dir: tempfile::TempDir,
    caches: [DigestCache; 2],
    costs: Costs,
}

fn cached() -> Cached {
    let dir = tempfile::tempdir().unwrap();
    let caches = ["base", "head"].map(|key| DigestCache::open(dir.path(), key).unwrap());
    let costs = Costs::load(dir.path().join("costs.json"));
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
    let root = tempfile::tempdir().unwrap();
    let fleet = Fleet::new();
    let fake = Fake::new(root.path(), &fleet, |_, _| journal("{}\n"));
    let pairs = same_sessions(&[1, 2, 3]);
    let config = cache.config(&fleet);
    for run in 0..2 {
        let outcome = compare::run(&pairs, &config, &fake).unwrap();
        assert!(!outcome.different(), "run {run}");
        assert_eq!(fake.launches().len(), 6, "run {run}");
    }
    assert!(cache.costs.get("base", "s/dev-2").is_some());
    assert!(files_under(root.path()).is_empty());
    let mut fresh = cache.config(&fleet);
    fresh.fresh = true;
    compare::run(&pairs, &fresh, &fake).unwrap();
    assert_eq!(fake.launches().len(), 12);
}

#[test]
fn a_difference_reruns_only_the_sides_that_have_no_run_to_show() {
    let cache = cached();
    let root = tempfile::tempdir().unwrap();
    let fleets = [Fleet::new(), Fleet::new(), Fleet::new()];
    let fake = |fleet| {
        Fake::new(root.path(), fleet, |side, _| {
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
        let root = tempfile::tempdir().unwrap();
        let fleet = Fleet::new();
        let fake = Fake::new(root.path(), &fleet, |side, spec| {
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
            files_under(root.path()),
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
    let root = tempfile::tempdir().unwrap();
    let fleet = Fleet::new();
    let mut fake = Fake::new(root.path(), &fleet, |_, _| journal("{}\n"));
    fake.hook = Some(Box::new(|_, _| fleet.cancel()));
    let error = compare::run(&same_sessions(&[1, 2, 3, 4]), &config(&fleet), &fake).unwrap_err();
    assert!(matches!(error, SameError::Interrupted), "{error}");
    assert!(
        files_under(root.path()).is_empty(),
        "{:?}",
        files_under(root.path())
    );
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
        let cold = tempfile::tempdir().unwrap();
        let first = Fake::new(cold.path(), &fleets[0], behave);
        assert!(
            compare::run(&pairs(), &config, &first).unwrap().different(),
            "{name}"
        );
        let root = tempfile::tempdir().unwrap();
        let second = Fake::new(root.path(), &fleets[1], behave);
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
        assert_eq!(files_under(root.path()), want, "{name}");
    }
}

#[test]
fn sessions_that_race_to_a_difference_report_one_when_stopping_at_the_first() {
    let cache = cached();
    let fleet = Fleet::new();
    let changed = |side: Side, _: &RunSpec| journal(&format!("{{\"side\":{}}}\n", side.index()));
    let cold = tempfile::tempdir().unwrap();
    let mut keep_going = cache.config(&fleet);
    keep_going.keep_going = true;
    compare::run(
        &same_sessions(&[1, 2]),
        &keep_going,
        &Fake::new(cold.path(), &fleet, changed),
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let barrier = Barrier::new(2);
    let mut fake = Fake::new(root.path(), &fleet, changed);
    fake.hook = Some(Box::new(move |side, _| {
        if side == Side::Base {
            barrier.wait();
        }
    }));
    let outcome = compare::run(&same_sessions(&[1, 2]), &cache.config(&fleet), &fake).unwrap();
    assert_eq!(outcome.differences.len(), 1);
    assert_eq!(
        files_under(root.path()).len(),
        4,
        "the journals and logs of the reported session"
    );
}

#[test]
fn a_rerun_cancelled_for_an_interrupt_leaves_its_directory_nowhere() {
    let cache = cached();
    let fleet = Fleet::new();
    let changed = |side: Side, _: &RunSpec| journal(&format!("{{\"side\":{}}}\n", side.index()));
    let cold = tempfile::tempdir().unwrap();
    compare::run(
        &same_sessions(&[1]),
        &cache.config(&fleet),
        &Fake::new(cold.path(), &fleet, changed),
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(root.path(), &fleet, changed);
    fake.hook = Some(Box::new(|_, _| fleet.cancel()));
    let error = compare::run(&same_sessions(&[1]), &cache.config(&fleet), &fake).unwrap_err();
    assert!(matches!(error, SameError::Interrupted), "{error}");
    assert!(files_under(root.path()).is_empty());
}

#[test]
fn the_time_of_the_compared_sessions_is_reported_without_failing() {
    let cache = cached();
    let root = tempfile::tempdir().unwrap();
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
    let fake = Fake::new(root.path(), &fleet, move |side, spec| Behavior {
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
    let root = tempfile::tempdir().unwrap();
    let fleet = Fleet::new();
    let fake = Fake::new(root.path(), &fleet, |side, _| Behavior {
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
fn a_digest_cache_keeps_the_most_recent_trees_only() {
    let dir = tempfile::tempdir().unwrap();
    for key in (0..10).map(|n| format!("key-{n}")) {
        DigestCache::open(dir.path(), &key).unwrap();
    }
    let kept = fs::read_dir(dir.path().join("same")).unwrap().count();
    assert_eq!(kept, 8);
    assert!(dir.path().join("same/key-9").is_dir());
}

#[test]
fn a_session_s_inputs_change_with_any_file_of_its_machine() {
    use crate::suite::Machine;
    use std::sync::Arc;
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
