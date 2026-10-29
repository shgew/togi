use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use super::*;
use crate::SuiteSource;
use crate::record;
use togi_sim::file::Source;

#[expect(
    dead_code,
    reason = "orchestration tests use only part of the shared memory fixture"
)]
#[path = "../../tests/bench/common/mod.rs"]
mod common;
#[expect(
    dead_code,
    reason = "orchestration tests inspect session events, not simulator outcomes"
)]
#[path = "../../tests/bench/session/mod.rs"]
mod session;

struct Bench {
    tree: common::Tree,
    calls: Arc<AtomicUsize>,
    events: Arc<[togi_journal::Event]>,
}

impl Bench {
    fn new() -> Self {
        let bench = Self {
            tree: common::Tree::new(),
            calls: Arc::new(AtomicUsize::new(0)),
            events: session::concluded().into(),
        };
        bench.write(bench.path("suite.json"), r#"{"scenarios":[{"name":"default","dev":[1,2,3],"holdout":[101]},{"name":"late","dev":[1]}]}"#);
        bench
    }
    fn path(&self, name: &str) -> PathBuf {
        self.tree.path().join(name)
    }
    fn write(&self, path: impl AsRef<Path>, text: impl AsRef<[u8]>) {
        self.tree.write(path, text);
    }
    fn text(&self, path: impl AsRef<Path>) -> String {
        self.tree.text(path)
    }
    fn simulator(&self, name: &str, exit: i32, log: &str, identity: &str) -> PathBuf {
        let path = self.path(name);
        self.write(
            &path,
            serde_json::json!({"exit": exit, "log": log, "identity": identity}).to_string(),
        );
        path
    }
    fn options(&self, simulator: PathBuf) -> MemoryOptions {
        MemoryOptions {
            options: Options {
                suite: self.path("suite.json"),
                selection: Selection::All,
                out: None,
                baseline_out: None,
                baseline: None,
                keep: None,
                jobs: NonZeroUsize::new(3).unwrap(),
                timeout: Duration::from_secs(10),
                max_boots: NonZeroU32::new(7).unwrap(),
                cache: Some(self.path("cache")),
                simulator: Some(simulator),
                tree: Tree {
                    commit: "abc1234".into(),
                    dirty: false,
                },
            },
            io: MemoryIo {
                tree: self.tree.clone(),
                calls: self.calls.clone(),
                events: self.events.clone(),
            },
        }
    }
    fn records(&self, path: &Path, what: &'static str) -> Vec<Record> {
        record::read_from(self.tree.read(path).unwrap().as_slice(), path, what).unwrap()
    }
}

#[derive(Clone)]
struct MemoryIo {
    tree: common::Tree,
    calls: Arc<AtomicUsize>,
    events: Arc<[togi_journal::Event]>,
}
#[derive(Clone)]
struct MemoryOptions {
    options: Options,
    io: MemoryIo,
}
impl Deref for MemoryOptions {
    type Target = Options;
    fn deref(&self) -> &Options {
        &self.options
    }
}
impl DerefMut for MemoryOptions {
    fn deref_mut(&mut self) -> &mut Options {
        &mut self.options
    }
}
impl Source for MemoryIo {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        self.tree.read_to_string(path)
    }
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        self.tree.canonicalize(path)
    }
}
impl SuiteSource for MemoryIo {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.tree.read(path)
    }
}

/// How many of the session's events the `unconcluded` simulator leaves: a prefix that never concludes.
const UNCONCLUDED: usize = 20;

struct MemoryRunner {
    digest: String,
    exit: i32,
    log_dead_end: bool,
    calls: Arc<AtomicUsize>,
    mode: String,
    tree: common::Tree,
    blocked: Option<PathBuf>,
    events: Arc<[togi_journal::Event]>,
}
impl SessionRunner for MemoryRunner {
    fn digest(&self) -> &str {
        &self.digest
    }
    fn run(
        &self,
        fleet: &Fleet,
        root: &Path,
        spec: &RunSpec,
        _: NonZeroU32,
        _: Duration,
    ) -> Result<Session, RunError> {
        if fleet.stopping() {
            return Err(RunError::Interrupted);
        }
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        std::thread::yield_now();
        let mut events = self.events.to_vec();
        if self.mode == "signal-first" && call == 0 {
            events.truncate(UNCONCLUDED);
            let mut event = serde_json::to_value(events.last().unwrap()).unwrap();
            event["cause"] = serde_json::json!([event["seq"].clone()]);
            event["seq"] = (event["seq"].as_u64().unwrap() + 1).into();
            event["kind"] = "shutdown".into();
            event["msg"] = "stopped by signal".into();
            event["data"] = serde_json::json!({"reason": "signal"});
            events.push(togi_journal::Event::decode(&event.to_string()).unwrap());
        }
        if self.mode == "no-journal" {
            events.clear();
        }
        if self.mode == "unconcluded" && spec.name() == "default/dev-2" {
            events.truncate(UNCONCLUDED);
        }
        if let Some(path) = &self.blocked {
            self.tree.write(path, "not a directory");
        }
        let directory = root.join(spec.name());
        let bytes: Vec<u8> = events
            .iter()
            .flat_map(|event| {
                let mut line = event.encode().unwrap().into_bytes();
                line.push(b'\n');
                line
            })
            .collect();
        let folded = togi_session::ReadState::replay(&events).unwrap();
        let prefix =
            togi_journal::snapshot::Prefix::of(&bytes, events.last().map(|event| event.seq));
        let saved = togi_journal::snapshot::seal(prefix, &folded.document()).unwrap();
        self.tree.fs.put(directory.join("events.jsonl"), bytes);
        self.tree.fs.put(
            directory.join("state.json"),
            serde_json::to_vec(&saved).unwrap(),
        );
        Ok(Session {
            dir: root.join(spec.name()),
            events,
            exit: self.exit,
            log_dead_end: self.log_dead_end,
            wall: Duration::from_millis(1),
        })
    }
    fn audit(
        &self,
        session: &Session,
        completion: togi_tool_audit::Completion,
    ) -> togi_tool_audit::DirectoryAudit {
        let findings = togi_tool_audit::audit(&session.events, completion)
            .into_iter()
            .map(|violation| togi_tool_audit::Finding {
                directory: session.dir.clone(),
                journal: Some("live".into()),
                seq: Some(violation.seq),
                check: togi_tool_audit::Kind::Audit(violation.check),
                reason: violation.reason,
            })
            .collect();
        togi_tool_audit::DirectoryAudit {
            journals: usize::from(!session.events.is_empty()),
            findings,
            skipped: Vec::new(),
        }
    }
    fn folded(
        &self,
        session: &Session,
    ) -> Result<togi_session::ReadState, togi_session::FoldError> {
        togi_session::ReadState::read_in(self.tree.fs.as_ref(), &session.dir, &session.events)
    }
    fn remove(&self, _: &Path) {}
}

struct MemoryRoot {
    path: PathBuf,
    kept: bool,
}
impl RunRoot for MemoryRoot {
    fn path(&self) -> &Path {
        &self.path
    }
    fn kept(&self) -> Option<PathBuf> {
        self.kept.then(|| self.path.clone())
    }
    fn discard(&self) {}
}
impl ExecutionIo for MemoryIo {
    type Simulator = MemoryRunner;
    type Root = MemoryRoot;
    fn simulator(&self, options: &Options) -> Result<MemoryRunner, Error> {
        use sha2::Digest as _;
        let bytes = self.read(options.simulator.as_deref().unwrap()).unwrap();
        let config: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let digest = crate::suite::hex(&sha2::Sha256::digest(&bytes));
        let mode = config["identity"].as_str().unwrap().to_owned();
        let blocked = (mode == "block-cache").then(|| {
            options
                .cache
                .as_ref()
                .unwrap()
                .join("runs")
                .join(cache::tree_key(&digest, "bench", options.max_boots.get()))
                .join("default")
        });
        Ok(MemoryRunner {
            digest,
            exit: i32::try_from(config["exit"].as_i64().unwrap()).unwrap(),
            log_dead_end: config["log"].as_str().unwrap().contains("sim: dead end "),
            calls: self.calls.clone(),
            mode,
            tree: self.tree.clone(),
            blocked,
            events: self.events.clone(),
        })
    }
    fn cache(
        &self,
        options: &Options,
        simulator: &MemoryRunner,
    ) -> Result<(Option<RunCache>, Option<Costs>), Error> {
        Ok(match &options.cache {
            None => (None, None),
            Some(root) => (
                Some(RunCache::with_fs(
                    root,
                    &cache::tree_key(simulator.digest(), "bench", options.max_boots.get()),
                    self.tree.fs.clone(),
                )),
                Some(Costs::with_fs(
                    root.join("costs.json"),
                    self.tree.fs.clone(),
                )),
            ),
        })
    }
    fn root(&self, keep: Option<&Path>) -> Result<MemoryRoot, Error> {
        Ok(MemoryRoot {
            path: keep.unwrap_or_else(|| Path::new("/runs")).to_path_buf(),
            kept: keep.is_some(),
        })
    }
    fn write_records(&self, path: &Path, records: &[Record], baseline: bool) -> Result<(), Error> {
        let mut bytes = Vec::new();
        if baseline {
            record::encode_baseline(&mut bytes, records).unwrap();
        } else {
            record::encode(&mut bytes, records).unwrap();
        }
        self.tree.write(path, bytes);
        Ok(())
    }
}

fn execute(options: &MemoryOptions, fleet: &Fleet) -> Result<Report, Error> {
    execute_with(options, fleet, &options.io)
}
fn run(options: &MemoryOptions) -> Report {
    execute(options, &Fleet::new()).unwrap()
}
fn report_text(report: &Report) -> String {
    let mut out = Vec::new();
    report.write(&mut out).unwrap();
    String::from_utf8(out).unwrap()
}
fn without_wall(report: &Report) -> Vec<String> {
    report
        .records
        .iter()
        .map(|record| {
            let mut record = record.clone();
            record.wall_s = None;
            serde_json::to_string(&record).unwrap()
        })
        .collect()
}
#[test]
fn every_run_gets_one_record_in_suite_order_stamped_with_the_tree() {
    let bench = Bench::new();
    let report = run(&bench.options(bench.simulator("sim", 0, "", "")));
    let names: Vec<_> = report
        .records
        .iter()
        .map(|r| format!("{}/{}-{}", r.scenario, r.split, r.seed))
        .collect();
    assert_eq!(
        names,
        [
            "default/dev-1",
            "default/dev-2",
            "default/dev-3",
            "default/holdout-101",
            "late/dev-1"
        ]
    );
    assert!(
        report
            .records
            .iter()
            .all(|r| r.status == Status::Concluded && r.exit_code == 0)
    );
    assert!(
        report
            .records
            .iter()
            .all(|r| r.commit == "abc1234" && !r.dirty && r.ruleset == togi_tuner::RULESET)
    );
    assert!(
        report
            .records
            .iter()
            .all(|r| r.sim_hours > 0.0 && r.wall_s.is_some_and(|wall| wall >= 0.0))
    );
    assert_eq!(report.cached, 0);
}
#[test_case::test_case((0, "", Status::Concluded); "0 Status Concluded")]
#[test_case::test_case((3, "", Status::Censored); "3 Status Censored")]
#[test_case::test_case((1, "sim: dead end stock: unstable", Status::DeadEnd); "1 sim dead end stock unstable Status DeadEnd")]
#[test_case::test_case((1, "sim: failed to read file", Status::Error); "1 sim failed to read file Status Error")]
#[test_case::test_case((2, "", Status::Error); "2 Status Error")]
fn a_status_follows_the_exit_status_and_the_log(case: (i32, &str, Status)) {
    let (exit, log, want) = case;
    let bench = Bench::new();

    let sim = bench.simulator(&format!("sim-{exit}"), exit, log, "");
    let mut options = bench.options(sim);
    options.cache = None;
    let report = run(&options);
    assert!(
        report.records.iter().all(|r| r.status == want),
        "{exit} {log}"
    );
    assert!(report.records.iter().all(|r| r.exit_code == exit));
}

#[test]
fn the_records_do_not_depend_on_how_many_simulators_run_at_once() {
    let bench = Bench::new();
    let sim = bench.simulator("sim", 0, "", "");
    let mut one = bench.options(sim);
    one.jobs = NonZeroUsize::MIN;
    one.cache = None;
    let mut many = one.clone();
    many.jobs = NonZeroUsize::new(5).unwrap();
    assert_eq!(without_wall(&run(&one)), without_wall(&run(&many)));
}

#[test]
fn a_second_run_over_the_same_inputs_comes_from_the_cache() {
    let bench = Bench::new();
    let options = bench.options(bench.simulator("sim", 0, "", ""));
    let first = run(&options);
    let second = run(&options);
    assert_eq!((first.cached, second.cached), (0, 5));
    assert_eq!(first.records, second.records);
    assert_eq!(bench.calls.load(Ordering::SeqCst), 5);
}

#[test]
fn another_simulator_binary_misses_the_cache() {
    let bench = Bench::new();
    run(&bench.options(bench.simulator("sim", 0, "", "")));
    let other = bench.simulator("other", 0, "", "true");
    assert_eq!(run(&bench.options(other)).cached, 0);
}

#[test]
fn another_boot_cap_misses_the_cache() {
    let bench = Bench::new();
    let mut options = bench.options(bench.simulator("sim", 0, "", ""));
    run(&options);
    options.max_boots = NonZeroU32::new(8).unwrap();
    assert_eq!(run(&options).cached, 0);
}

#[test]
fn a_changed_suite_file_misses_the_cache() {
    let bench = Bench::new();
    let options = bench.options(bench.simulator("sim", 0, "", ""));
    run(&options);
    bench.write(
        bench.path("suite.json"),
        r#"{"scenarios":[{"name":"default","dev":[1,2,3],"holdout":[101]},{"name":"late","dev":[1],"smoke":[1]}]}"#,
    );
    assert_eq!(run(&options).cached, 0);
}

#[test]
fn a_changed_machine_file_misses_the_cache_and_another_run_hits() {
    let bench = Bench::new();
    bench.write(bench.path("m.json"), r#"{"cores": 16}"#);
    bench.write(
        bench.path("suite.json"),
        r#"{"scenarios":[{"name":"m","machine":"m.json","dev":[1]},{"name":"default","dev":[1]}]}"#,
    );
    let options = bench.options(bench.simulator("sim", 0, "", ""));
    run(&options);
    assert_eq!(run(&options).cached, 2);
    bench.write(
        bench.path("m.json"),
        r#"{"cores": 16, "description": "changed"}"#,
    );
    assert_eq!(run(&options).cached, 1);
}
#[test]
fn the_records_file_is_written_and_a_baseline_is_compared() {
    let bench = Bench::new();
    let mut options = bench.options(bench.simulator("sim", 0, "", ""));
    options.out = Some(bench.path("out.jsonl"));
    let first = run(&options);
    let written = bench.records(&bench.path("out.jsonl"), "records");
    assert_eq!(written, first.records);
    options.baseline = Some(bench.path("out.jsonl"));
    let report = run(&options);
    let mut text = Vec::new();
    report.write(&mut text).unwrap();
    let text = String::from_utf8(text).unwrap();
    assert!(text.contains("verdict: NEUTRAL"), "{text}");
    assert!(text.contains("cache: 5 of 5 runs cached"), "{text}");
    assert!(text.contains("harness_wall_s="));
}

#[test]
fn the_baseline_file_holds_the_runs_records_without_wall_times_and_compares_as_they_do() {
    let bench = Bench::new();
    let mut options = bench.options(bench.simulator("sim", 0, "", ""));
    options.out = Some(bench.path("out.jsonl"));
    options.baseline_out = Some(bench.path("slim.jsonl"));
    let first = run(&options);
    let slim = bench.records(&bench.path("slim.jsonl"), "baseline");
    let want: Vec<_> = first
        .records
        .iter()
        .map(togi_tool_bench::Record::baseline)
        .collect();
    assert_eq!(slim, want);
    assert!(first.records.iter().all(|r| r.wall_s.is_some()));
    let text = bench.text(bench.path("slim.jsonl"));
    assert!(
        !text.contains("wall_s") && !text.contains("model_check"),
        "{text}"
    );

    options.out = None;
    options.baseline_out = None;
    let mut report_against = |file: &str| {
        options.baseline = Some(bench.path(file));
        report_text(&run(&options))
            .lines()
            .filter(|line| !line.starts_with("harness_wall_s="))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let against_full = report_against("out.jsonl");
    assert!(against_full.contains("verdict: NEUTRAL"), "{against_full}");
    assert_eq!(report_against("slim.jsonl"), against_full);
}
#[test]
fn a_cancelled_fleet_runs_nothing_and_reports_the_interrupt() {
    let bench = Bench::new();
    let fleet = Fleet::new();
    fleet.cancel();
    let options = bench.options(bench.simulator("sim", 0, "", ""));
    assert!(matches!(execute(&options, &fleet), Err(Error::Interrupted)));
    assert_eq!(bench.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn a_suite_with_no_selected_runs_is_refused_before_any_simulator_runs() {
    let bench = Bench::new();
    bench.write(
        bench.path("suite.json"),
        r#"{"scenarios":[{"name":"x","holdout":[1]}]}"#,
    );
    let mut options = bench.options(bench.simulator("sim", 0, "", ""));
    options.selection = Selection::Dev;
    let error = execute(&options, &Fleet::new()).unwrap_err();
    let chain = togi_machine::error_chain(&error);
    assert!(chain.contains("no selected runs"), "{chain}");
    assert_eq!(bench.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_baseline_with_a_run_twice_is_refused() {
    let bench = Bench::new();
    let mut options = bench.options(bench.simulator("sim", 0, "", ""));
    options.out = Some(bench.path("out.jsonl"));
    run(&options);
    let text = bench.text(bench.path("out.jsonl"));
    let first = text.lines().next().unwrap();
    bench.write(bench.path("dup.jsonl"), format!("{first}\n{first}\n"));
    options.baseline = Some(bench.path("dup.jsonl"));
    let error = execute(&options, &Fleet::new()).unwrap_err();
    assert!(
        error.to_string().contains("duplicate baseline run"),
        "{error}"
    );
}

#[test]
fn the_report_lists_the_summary_for_each_scenario_and_the_cache_line() {
    let bench = Bench::new();
    let report = run(&bench.options(bench.simulator("sim", 0, "", "")));
    let mut text = Vec::new();
    report.write(&mut text).unwrap();
    let text = String::from_utf8(text).unwrap();
    for needle in ["default ", "late ", "TOTAL ", "cache: 0 of 5 runs cached"] {
        assert!(text.contains(needle), "{needle}: {text}");
    }
}

#[test]
fn a_journal_that_ends_in_a_signal_is_an_error_record_and_is_not_cached() {
    let bench = Bench::new();
    let mut options = bench.options(bench.simulator("sim", 0, "", "signal-first"));
    options.jobs = NonZeroUsize::MIN;
    let first = run(&options);
    assert_eq!(first.records[0].status, Status::Error);
    assert_eq!(first.records[0].exit_code, 0);
    assert!(
        first.records[1..]
            .iter()
            .all(|r| r.status == Status::Concluded)
    );
    let second = run(&options);
    assert_eq!(second.cached, 4);
    assert!(second.records.iter().all(|r| r.status == Status::Concluded));
}

#[test]
fn a_simulator_that_leaves_no_journal_is_an_error_record() {
    let bench = Bench::new();
    let report =
        run(&bench.options(bench.simulator("sim", 1, "sim: failed to read file", "no-journal")));
    assert_eq!(report.records.len(), 5);
    assert!(
        report
            .records
            .iter()
            .all(|r| r.status == Status::Error && r.exit_code == 1 && r.sim_hours == 0.0)
    );
}

#[test_case::test_case(0, "", Status::Concluded; "concluded")]
#[test_case::test_case(1, "sim: dead end stock: unstable", Status::DeadEnd; "dead end")]
#[test_case::test_case(3, "", Status::Censored; "censored")]
#[test_case::test_case(2, "", Status::Error; "failed")]
fn a_run_of_any_status_passes_the_audit_of_a_clean_journal(exit: i32, log: &str, status: Status) {
    let bench = Bench::new();
    let mut options = bench.options(bench.simulator("sim", exit, log, ""));
    options.cache = None;
    let report = run(&options);
    assert!(report.records.iter().all(|r| r.status == status));
    assert_eq!(report.audited, 5);
    assert_eq!(report.violations, Vec::new());
    assert!(report_text(&report).contains("audit: audited=5 violations=0\n"));
}

#[test_case::test_case(0, "", Status::Concluded, true; "a concluded run must have stopped")]
#[test_case::test_case(1, "sim: dead end stock: unstable", Status::DeadEnd, true; "a dead end must have stopped")]
#[test_case::test_case(3, "", Status::Censored, false; "a censored run may be a prefix")]
#[test_case::test_case(2, "", Status::Error, false; "a failed run may be a prefix")]
fn an_unconcluded_journal_is_a_violation_only_when_its_run_finished(
    exit: i32,
    log: &str,
    status: Status,
    violates: bool,
) {
    let bench = Bench::new();
    let mut options = bench.options(bench.simulator("sim", exit, log, "unconcluded"));
    options.cache = None;
    let report = run(&options);
    assert!(report.records.iter().all(|r| r.status == status));
    assert_eq!(report.audited, 5);
    let mut runs: Vec<&str> = report.violations.iter().map(|v| v.run.as_str()).collect();
    runs.dedup();
    let want: &[&str] = if violates { &["default/dev-2"] } else { &[] };
    assert_eq!(runs, want, "{:?}", report.violations);
}

#[test]
fn a_run_that_breaks_a_check_is_reported_by_name_and_not_cached() {
    let bench = Bench::new();
    let options = bench.options(bench.simulator("sim", 0, "", "unconcluded"));
    let first = run(&options);
    assert_eq!(first.audited, 5);
    assert!(!first.violations.is_empty());
    assert!(
        first
            .violations
            .iter()
            .all(|violation| violation.run == "default/dev-2"),
        "{:?}",
        first.violations
    );
    let last = bench.events[UNCONCLUDED - 1].seq;
    let text = report_text(&first);
    assert!(
        text.contains(&format!(
            "audit: audited=5 violations={}\n",
            first.violations.len()
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "audit: default/dev-2 live seq {last} termination: finished run has neither a conclusion nor a dead end\n"
        )),
        "{text}"
    );
    let second = run(&options);
    assert_eq!((second.cached, second.audited), (4, 1));
    assert_eq!(second.violations, first.violations);
}
#[test]
fn a_failed_cache_write_is_a_warning_and_the_records_are_kept() {
    let bench = Bench::new();
    let mut options = bench.options(bench.simulator("sim", 0, "", "block-cache"));
    options.jobs = NonZeroUsize::MIN;
    let report = run(&options);
    assert_eq!(report.records.len(), 5);
    assert_eq!(report.warnings.len(), 4, "{:?}", report.warnings);
    assert!(
        report.warnings[0].starts_with("write the run cache for default/dev-1: "),
        "{:?}",
        report.warnings
    );
    assert!(report.warnings[3].contains("default/holdout-101"));
    assert_eq!(run(&options).cached, 1);
}

#[test]
fn a_model_check_whose_extract_cannot_be_read_names_the_machine_then_each_cause_once() {
    let definition = togi_sim::file::decode(include_str!(
        "../../../../tools/bench/machines/target-fit-0.json"
    ))
    .unwrap();
    let machine = crate::suite::Machine {
        name: "machines/target-fit-0.json".to_owned(),
        path: PathBuf::from("/suite/machines/target-fit-0.json"),
        definition,
        inputs: Vec::new(),
    };
    let error = check_machine_with(&machine, &mut Extracts::new(), |_| {
        Err(togi_tool_trialfacts::ReadError::Io(io::Error::other(
            "disk gone",
        )))
    })
    .unwrap_err();
    assert_eq!(
        togi_machine::error_chain(&error),
        "check model machines/target-fit-0.json: load replay oracle: read extract: disk gone"
    );
}
