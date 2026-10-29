use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;

use flate2::Compression;
use flate2::write::GzEncoder;
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};
use test_case::test_case;

use super::*;

const CHECKOUT: &str = "cccccccccccccccccccccccccccccccccccccccc";
const REFERENCE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DEVELOPMENT: [&str; 6] = [
    "20260924T204352Z",
    "20260926T151414Z",
    "20260927T221954Z",
    "20260929T180308Z",
    "20261002T004254Z",
    "20261002T235548Z",
];
const CONFIRMATION: &str = "20261004T000701Z";
const SPENT: &str = "20261004T211726Z";
const FACTS: &str = "facts/original.jsonl.gz";
const SUITE: &str = "tools/bench/suites/research.json";
const RUN: &str = "/research/427/t1";
/// A commit outside the checkout's history.
const ORPHAN: &str = "dddddddddddddddddddddddddddddddddddddddd";

/// A checkout in memory: its files now, and the files at each commit.
struct Fake {
    head: String,
    files: BTreeMap<String, Vec<u8>>,
    commits: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
}

impl Workspace for Fake {
    fn root(&self) -> &std::path::Path {
        std::path::Path::new("/checkout")
    }

    fn head(&self) -> Result<String, WorkspaceError> {
        Ok(self.head.clone())
    }

    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool, WorkspaceError> {
        Ok(ancestor == descendant
            || (ancestor == REFERENCE && descendant == CHECKOUT)
            || (ancestor == CHECKOUT && descendant != ORPHAN))
    }

    fn changed(&self, base: &str) -> Result<BTreeSet<String>, WorkspaceError> {
        let base = &self.commits[base];
        Ok(base
            .keys()
            .chain(self.files.keys())
            .filter(|path| base.get(*path) != self.files.get(*path))
            .cloned()
            .collect())
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, WorkspaceError> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| WorkspaceError::Missing(path.to_owned()))
    }
}

impl Fake {
    fn edit(&mut self, path: &str, content: &str) {
        self.files
            .insert(path.to_owned(), content.as_bytes().to_vec());
    }

    fn revert(&mut self, path: &str) {
        match self.commits[CHECKOUT].get(path) {
            Some(content) => self.files.insert(path.to_owned(), content.clone()),
            None => self.files.remove(path),
        };
    }
}

fn rows(sessions: &[&str]) -> Vec<u8> {
    sessions
        .iter()
        .flat_map(|session| format!("{{\"session\":\"{session}\",\"core\":0}}\n").into_bytes())
        .collect()
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn sessions() -> Vec<&'static str> {
    DEVELOPMENT.iter().copied().chain([CONFIRMATION]).collect()
}

fn fake(facts: &[&str]) -> Fake {
    let files: BTreeMap<String, Vec<u8>> = [
        (FACTS, gzip(&rows(facts))),
        ("Cargo.lock", b"lock".to_vec()),
        ("flake.lock", b"flake".to_vec()),
        ("rust-toolchain.toml", b"toolchain".to_vec()),
        ("tools/fit/research/427.md", b"brief 427".to_vec()),
        ("tools/bench/research/503.md", b"brief 503".to_vec()),
        (SUITE, b"suite".to_vec()),
        ("crates/togi-sim/src/model.rs", b"model".to_vec()),
        ("crates/togi-tuner/src/lib.rs", b"tuner".to_vec()),
    ]
    .into_iter()
    .map(|(path, bytes)| (path.to_owned(), bytes))
    .collect();
    Fake {
        head: CHECKOUT.to_owned(),
        commits: [
            (CHECKOUT.to_owned(), files.clone()),
            (REFERENCE.to_owned(), files.clone()),
        ]
        .into(),
        files,
    }
}

fn pin(fake: &Fake, path: &str) -> Value {
    json!({"path": path, "sha256": sha256(&fake.files[path])})
}

fn model_manifest(fake: &Fake) -> Value {
    json!({
        "issue": 427,
        "mode": "model-development",
        "hypotheses": [
            {"id": "h1", "statement": "a shared core term"},
            {"id": "h2", "statement": "a structured singleton term"},
        ],
        "checkout": CHECKOUT,
        "reference": REFERENCE,
        "inputs": [{
            "path": FACTS,
            "sha256": sha256(&rows(&sessions())),
            "development": DEVELOPMENT,
            "confirmation": [CONFIRMATION],
        }],
        "suite": null,
        "frozen": (["Cargo.lock", "flake.lock", "rust-toolchain.toml", "tools/fit/research/427.md"]
            .map(|path| pin(fake, path))),
        "writable": ["crates/togi-sim/src"],
        "output": RUN,
        "confirmations": 1,
    })
}

fn observation_manifest(fake: &Fake) -> Value {
    json!({
        "issue": 503,
        "mode": "fixed-policy-observation",
        "hypotheses": [{"id": "o1", "statement": "count the hazard boots"}],
        "checkout": CHECKOUT,
        "reference": REFERENCE,
        "suite": {"path": SUITE, "sha256": sha256(b"suite"), "split": "dev"},
        "frozen": (["Cargo.lock", "flake.lock", "rust-toolchain.toml", "tools/bench/research/503.md"]
            .map(|path| pin(fake, path))),
        "writable": [],
        "output": "/research/503/o",
        "confirmations": 0,
    })
}

fn at(minutes: i64) -> Timestamp {
    Timestamp::from_second(1_800_000_000)
        .unwrap()
        .checked_add(SignedDuration::from_mins(minutes))
        .unwrap()
}

/// A started campaign that applies each action's records as the run directory would.
struct Live {
    campaign: Campaign,
    minute: i64,
}

impl Live {
    fn start(manifest: &Value, issue: u32, fake: &Fake) -> Self {
        let bytes = serde_json::to_vec(manifest).unwrap();
        let (manifest, record) = start(&bytes, issue, fake, at(0)).unwrap();
        let issue = manifest.validate(issue).unwrap();
        Self {
            campaign: Campaign {
                manifest,
                issue,
                ledger: Ledger {
                    records: vec![record],
                },
            },
            minute: 0,
        }
    }

    fn now(&mut self) -> Timestamp {
        self.minute += 1;
        at(self.minute)
    }

    fn apply(&mut self, result: Result<Vec<Record>, Halt>) -> Result<(), Halt> {
        match result {
            Ok(records) => {
                self.campaign.ledger.records.extend(records);
                Ok(())
            }
            Err(Halt::Stop { reason, detail }) => {
                self.campaign.ledger.records.push(Record::CampaignStopped {
                    at: at(self.minute),
                    reason,
                    detail: detail.clone(),
                });
                Err(Halt::Stop { reason, detail })
            }
            Err(refusal) => Err(refusal),
        }
    }

    fn begin(
        &mut self,
        fake: &Fake,
        candidate: &str,
        role: Role,
        command: &str,
    ) -> Result<Planned, Halt> {
        let now = self.now();
        let mut words = command.split_whitespace().map(str::to_owned);
        let recipe = words.next().unwrap();
        let args: Vec<String> = words.collect();
        let planned = self
            .campaign
            .plan(fake, now, candidate, role, &recipe, &args);
        match planned {
            Ok(planned) => {
                self.campaign
                    .ledger
                    .records
                    .push(Record::ExperimentStarted(planned.started.clone()));
                Ok(planned)
            }
            Err(halt) => self.apply(Err(halt)).map(|()| unreachable!()),
        }
    }

    fn run(
        &mut self,
        fake: &Fake,
        candidate: &str,
        role: Role,
        command: &str,
        end: End,
    ) -> Result<u32, Halt> {
        let planned = self.begin(fake, candidate, role, command)?;
        let now = self.now();
        let records = Campaign::finish(
            &planned.started,
            now,
            end,
            Some(sha256(b"out")),
            Some(sha256(b"")),
        );
        self.campaign.ledger.records.extend(records);
        Ok(planned.started.seq)
    }

    fn decide(&mut self, fake: &Fake, candidate: &str, outcome: Outcome) -> Result<(), Halt> {
        let now = self.now();
        let metrics = BTreeMap::from([("brier".to_owned(), 0.123_456_789_012_345_67)]);
        let result = self.campaign.decide(
            fake,
            now,
            candidate,
            outcome,
            "by the brief's keep rule",
            metrics,
        );
        self.apply(result)
    }

    fn stopped(&self) -> Option<StopReason> {
        self.campaign.ledger.stopped().map(|(reason, _)| reason)
    }
}

const FORWARD: &str = "forward --facts facts/original.jsonl.gz --seal 1";
const CONFIRM: &str = "forward --facts facts/original.jsonl.gz";

fn halt_text(halt: &Halt) -> String {
    let mut text = halt.to_string();
    let mut source = std::error::Error::source(halt);
    while let Some(cause) = source {
        text = format!("{text}: {cause}");
        source = cause.source();
    }
    text
}

#[test]
fn valid_manifest_starts_the_ledger_with_its_hash() {
    let fake = fake(&sessions());
    let manifest = model_manifest(&fake);
    let bytes = serde_json::to_vec(&manifest).unwrap();
    let (_, record) = start(&bytes, 427, &fake, at(0)).unwrap();
    assert_eq!(
        record,
        Record::CampaignStarted {
            at: at(0),
            manifest_sha256: sha256(&bytes),
            head: CHECKOUT.to_owned(),
        }
    );
}

#[test_case(|m| m["issue"] = json!(428), "names issue 428" ; "issue differs from the command")]
#[test_case(|m| m["mode"] = json!("fixed-policy-observation"), "runs in model development mode" ; "wrong mode")]
#[test_case(|m| m["seed"] = json!(7), "unknown field" ; "unknown field")]
#[test_case(|m| m["hypotheses"] = json!([]), "queue is empty" ; "empty queue")]
#[test_case(|m| m["checkout"] = json!("cccc"), "full lowercase commit" ; "short checkout")]
#[test_case(|m| m["inputs"][0]["development"][0] = json!(SPENT), "not an approved development session" ; "spent session")]
#[test_case(|m| m["inputs"][0]["confirmation"] = json!(["20261010T230611Z"]), "not an approved confirmation session" ; "forbidden confirmation")]
#[test_case(|m| { m["frozen"].as_array_mut().unwrap().remove(0); }, "must pin Cargo.lock" ; "environment unpinned")]
#[test_case(|m| { m["frozen"].as_array_mut().unwrap().remove(3); }, "must pin tools/fit/research/427.md" ; "brief unpinned")]
#[test_case(|m| m["writable"] = json!(["tools/fit/program.md"]), "overlaps tools/fit" ; "writable program")]
#[test_case(|m| m["writable"] = json!(["crates"]), "overlaps crates/togi-config" ; "writable covers the closed crates")]
#[test_case(|m| m["writable"] = json!(["facts"]), "overlaps facts/original.jsonl.gz" ; "writable covers an input")]
#[test_case(|m| m["writable"] = json!(["../outside"]), "plain relative path" ; "writable escapes")]
#[test_case(|m| m["output"] = json!("runs/research/427/t1"), "is not an absolute path" ; "relative output")]
#[test_case(|m| m["output"] = json!("/research/427/../x"), "is not an absolute path" ; "output escapes")]
#[test_case(|m| m["confirmations"] = json!(2), "allows 1 confirmations" ; "too many confirmations")]
#[test_case(|m| m["limits"] = json!({"wall_minutes": 0, "candidates": 12}), "must be positive" ; "zero wall limit")]
#[test_case(|m| m["limits"] = json!({"wall_minutes": 480, "candidates": 0}), "must be positive" ; "zero candidate limit")]
#[test_case(|m| m["suite"] = json!({"path": SUITE, "sha256": "x", "split": "dev"}), "names no bench suite" ; "suite in model mode")]
#[test_case(|m| m["limits"] = json!([480, 12]), "expected an object" ; "limits as an array")]
#[test_case(|m| m["frozen"][0] = json!(["Cargo.lock", sha256(b"lock")]), "expected an object" ; "pin as an array")]
#[test_case(|m| m["hypotheses"][0] = json!(["h1", "a shared core term"]), "expected an object" ; "hypothesis as an array")]
fn manifest_is_rejected(change: fn(&mut Value), expected: &str) {
    let fake = fake(&sessions());
    let mut manifest = model_manifest(&fake);
    change(&mut manifest);
    let error = start(&serde_json::to_vec(&manifest).unwrap(), 427, &fake, at(0)).unwrap_err();
    let text = format!(
        "{error}: {:?}",
        std::error::Error::source(&error).map(ToString::to_string)
    );
    assert!(text.contains(expected), "{text}");
}

#[test]
fn output_inside_the_checkout_is_rejected() {
    let fake = fake(&sessions());
    let mut manifest = model_manifest(&fake);
    manifest["output"] = json!("/checkout/runs/t1");
    let error = start(&serde_json::to_vec(&manifest).unwrap(), 427, &fake, at(0)).unwrap_err();
    assert!(matches!(error, Refusal::OutputInCheckout(_)), "{error}");
}

#[test]
fn issue_without_brief_is_rejected() {
    let fake = fake(&sessions());
    let mut manifest = model_manifest(&fake);
    manifest["issue"] = json!(428);
    let error = start(&serde_json::to_vec(&manifest).unwrap(), 428, &fake, at(0)).unwrap_err();
    assert!(
        matches!(error, Refusal::Manifest(ManifestError::NoBrief(428))),
        "{error}"
    );
}

#[test_case(480, |fake| { let mut m = model_manifest(fake); m["issue"] = json!(480); m["confirmations"] = json!(0); m }, "not an approved confirmation session" ; "480 reads no confirmation session")]
#[test_case(896, |fake| { let mut m = observation_manifest(fake); m["issue"] = json!(896); m }, "has no research brief" ; "896 is retired")]
#[test_case(490, |fake| { let mut m = observation_manifest(fake); m["issue"] = json!(490); m }, "has no research brief" ; "490 is retired")]
#[test_case(503, |fake| { let mut m = observation_manifest(fake); m["issue"] = json!(503); m["mode"] = json!("model-development"); m }, "runs in fixed-policy observation mode" ; "503 is observation")]
fn issue_table_is_enforced(issue: u32, manifest: fn(&Fake) -> Value, expected: &str) {
    let fake = fake(&sessions());
    let bytes = serde_json::to_vec(&manifest(&fake)).unwrap();
    let error = start(&bytes, issue, &fake, at(0)).unwrap_err();
    assert!(error.to_string().contains(expected), "{error}");
}

#[test]
fn reference_differing_outside_the_writable_paths_is_refused_at_start() {
    let mut fake = fake(&sessions());
    let manifest = serde_json::to_vec(&model_manifest(&fake)).unwrap();
    fake.commits.get_mut(REFERENCE).unwrap().insert(
        "crates/togi-tuner/src/lib.rs".to_owned(),
        b"older tuner".to_vec(),
    );
    let error = start(&manifest, 427, &fake, at(0)).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("outside the writable paths, at crates/togi-tuner/src/lib.rs"),
        "{error}"
    );
}

#[test]
fn reference_differing_at_writable_paths_runs_at_the_reference_tree() {
    let mut fake = fake(&sessions());
    fake.commits.get_mut(REFERENCE).unwrap().insert(
        "crates/togi-sim/src/model.rs".to_owned(),
        b"older model".to_vec(),
    );
    let mut live = Live::start(&model_manifest(&fake), 427, &fake);
    fake.edit("crates/togi-sim/src/model.rs", "older model");
    live.run(
        &fake,
        "h1",
        Role::Reference,
        FORWARD,
        End::Exited { code: 0 },
    )
    .unwrap();
    assert_eq!(live.stopped(), None);
}

#[test]
fn missing_manifest_field_is_rejected() {
    let fake = fake(&sessions());
    let mut manifest = model_manifest(&fake);
    manifest.as_object_mut().unwrap().remove("frozen");
    let error = start(&serde_json::to_vec(&manifest).unwrap(), 427, &fake, at(0)).unwrap_err();
    assert!(
        matches!(error, Refusal::Manifest(ManifestError::Decode(_))),
        "{error}"
    );
}

#[test]
fn input_is_pinned_by_its_decompressed_rows() {
    let mut fake = fake(&sessions());
    let manifest = model_manifest(&fake);
    let recompressed = {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&rows(&sessions())).unwrap();
        encoder.finish().unwrap()
    };
    assert_ne!(recompressed, fake.files[FACTS]);
    fake.files.insert(FACTS.to_owned(), recompressed.clone());
    fake.commits.values_mut().for_each(|files| {
        files.insert(FACTS.to_owned(), recompressed.clone());
    });
    start(&serde_json::to_vec(&manifest).unwrap(), 427, &fake, at(0)).unwrap();
}

#[test]
fn input_holding_an_unnamed_session_is_rejected() {
    let with_spent: Vec<&str> = sessions().into_iter().chain([SPENT]).collect();
    let fake = fake(&with_spent);
    let mut manifest = model_manifest(&fake);
    manifest["inputs"][0]["sha256"] = json!(sha256(&rows(&with_spent)));
    let error = start(&serde_json::to_vec(&manifest).unwrap(), 427, &fake, at(0)).unwrap_err();
    assert!(error.to_string().contains("it holds sessions"), "{error}");
}

#[test]
fn changed_input_is_rejected_at_start() {
    let fake = fake(&sessions());
    let mut manifest = model_manifest(&fake);
    manifest["frozen"][3]["sha256"] = json!(sha256(b"an older brief"));
    let error = start(&serde_json::to_vec(&manifest).unwrap(), 427, &fake, at(0)).unwrap_err();
    assert!(
        matches!(error, Refusal::Manifest(ManifestError::Changed { .. })),
        "{error}"
    );
}

#[test]
fn start_needs_the_clean_checkout() {
    let mut fake = fake(&sessions());
    let manifest = serde_json::to_vec(&model_manifest(&fake)).unwrap();
    fake.edit("crates/togi-sim/src/model.rs", "edited early");
    let dirty = start(&manifest, 427, &fake, at(0)).unwrap_err();
    assert!(matches!(dirty, Refusal::Dirty(_)), "{dirty}");
    fake.revert("crates/togi-sim/src/model.rs");
    fake.head = REFERENCE.to_owned();
    let elsewhere = start(&manifest, 427, &fake, at(0)).unwrap_err();
    assert!(
        matches!(elsewhere, Refusal::NotAtCheckout { .. }),
        "{elsewhere}"
    );
}

#[test]
fn replaced_manifest_is_refused() {
    let fake = fake(&sessions());
    let live = Live::start(&model_manifest(&fake), 427, &fake);
    let mut other = model_manifest(&fake);
    other["hypotheses"][0]["statement"] = json!("something else");
    let error = Campaign::open(
        &serde_json::to_vec(&other).unwrap(),
        427,
        live.campaign.ledger,
    )
    .unwrap_err();
    assert!(matches!(error, Refusal::ManifestReplaced), "{error}");
}

#[test]
fn changed_frozen_file_stops_the_campaign_and_nothing_refreshes_it() {
    let mut fake = fake(&sessions());
    let mut live = Live::start(&model_manifest(&fake), 427, &fake);
    live.run(
        &fake,
        "h1",
        Role::Reference,
        FORWARD,
        End::Exited { code: 0 },
    )
    .unwrap();
    fake.edit("tools/fit/research/427.md", "a loosened brief");
    let halt = live
        .begin(&fake, "h1", Role::Candidate, FORWARD)
        .unwrap_err();
    assert!(
        halt_text(&halt).contains("tools/fit/research/427.md changed"),
        "{halt}"
    );
    assert_eq!(live.stopped(), Some(StopReason::EvidenceChanged));
    fake.revert("tools/fit/research/427.md");
    let after = live
        .begin(&fake, "h1", Role::Candidate, FORWARD)
        .unwrap_err();
    assert!(
        matches!(
            after,
            Halt::Refuse(Refusal::Stopped(StopReason::EvidenceChanged))
        ),
        "{after}"
    );
}

#[test]
fn deleted_input_stops_the_campaign() {
    let mut fake = fake(&sessions());
    let mut live = Live::start(&model_manifest(&fake), 427, &fake);
    fake.files.remove(FACTS);
    live.begin(&fake, "h1", Role::Reference, FORWARD)
        .unwrap_err();
    assert_eq!(live.stopped(), Some(StopReason::EvidenceChanged));
}

#[test_case("bench --suite x --split dev", "only as the F5 finalization check" ; "bench in a model development run")]
#[test_case("sim", "not an evaluator" ; "unlisted recipe")]
#[test_case("forward --seal 1", "--facts must name a manifest input" ; "default extract")]
#[test_case("forward --facts tools/bench/facts/target.jsonl.gz --seal 1", "is not a manifest input" ; "other facts")]
#[test_case("forward --facts facts/original.jsonl.gz", "seal exactly the input's 1 confirmation" ; "unsealed development run")]
#[test_case("forward --facts facts/original.jsonl.gz --seal 2", "seal exactly" ; "sealing development sessions away")]
#[test_case("forward --facts facts/original.jsonl.gz --seal 1 --reference-machine m", "--reference-machine is not allowed" ; "reference machine")]
#[test_case("forward --facts facts/original.jsonl.gz --seal 1 --seal 1", "given twice" ; "repeated flag")]
#[test_case("fit --facts facts/original.jsonl.gz --out /research/427/t1/fit", "does not serve this role" ; "fit in a candidate run")]
#[test_case("forward --facts facts/original.jsonl.gz --seal 1 stray", "is not a --flag" ; "positional argument")]
#[test_case("forward --facts facts/original.jsonl.gz --seal 1 --records runs/elsewhere.jsonl", "--records must name a path inside /research/427/t1" ; "records outside the run")]
#[test_case("forward --facts facts/original.jsonl.gz --seal 1 --records /research/427/t1/experiments.jsonl", "outside its record" ; "records over the ledger")]
fn model_evaluator_is_refused(command: &str, expected: &str) {
    let fake = fake(&sessions());
    let mut live = Live::start(&model_manifest(&fake), 427, &fake);
    let halt = live
        .begin(&fake, "h1", Role::Candidate, command)
        .unwrap_err();
    assert!(matches!(halt, Halt::Refuse(_)), "{halt}");
    assert!(halt_text(&halt).contains(expected), "{}", halt_text(&halt));
    assert_eq!(live.stopped(), None);
}

#[test_case(None ; "evaluator default")]
#[test_case(Some("8") ; "explicit evaluator workers")]
fn evaluator_arguments_pass_through_unchanged(jobs: Option<&str>) {
    let fake = fake(&sessions());
    let mut live = Live::start(&model_manifest(&fake), 427, &fake);
    let mut command = FORWARD.to_owned();
    let mut expected = ["--facts", FACTS, "--seal", "1"]
        .map(str::to_owned)
        .to_vec();
    if let Some(jobs) = jobs {
        command.push_str(" --jobs ");
        command.push_str(jobs);
        expected.extend(["--jobs".to_owned(), jobs.to_owned()]);
    }
    let planned = live.begin(&fake, "h1", Role::Reference, &command).unwrap();
    assert_eq!(planned.started.args, expected);
    assert_eq!(planned.budget, std::time::Duration::from_mins(479));
    assert_eq!(planned.started.stdout, "raw/0001-h1-reference.stdout");
}

#[test]
fn forward_records_are_written_inside_the_run() {
    let fake = fake(&sessions());
    let mut live = Live::start(&model_manifest(&fake), 427, &fake);
    let records = format!("{RUN}/records/h1.jsonl");
    let planned = live
        .begin(
            &fake,
            "h1",
            Role::Reference,
            &format!("{FORWARD} --records {records}"),
        )
        .unwrap();
    assert_eq!(
        planned.started.args,
        ["--facts", FACTS, "--seal", "1", "--records", &records].map(str::to_owned)
    );
}

#[test_case("forward --facts facts/original.jsonl.gz --seal 1", "not an evaluator of fixed-policy observation" ; "model evaluator")]
#[test_case("bench --split dev", "--suite must name the manifest's suite" ; "default suite")]
#[test_case("bench --suite tools/bench/suites/research.json --split holdout", "--split must be the manifest's dev" ; "other split")]
#[test_case("bench --suite tools/bench/suites/research.json --split dev --out runs/elsewhere", "--out must name a path inside /research/503/o" ; "output outside the run")]
#[test_case("bench --suite tools/bench/suites/research.json --split dev --out /research/503/o/experiments.jsonl", "outside its record" ; "output over the ledger")]
#[test_case("bench --suite tools/bench/suites/research.json --split dev --keep /research/503/o/raw/k", "outside its record" ; "output under the raw output")]
#[test_case("bench --suite tools/bench/suites/research.json --split dev --out /research/503/o/../x", "outside its record" ; "output escaping the run")]
#[test_case("bench --suite tools/bench/suites/research.json --split dev --baseline-out b.json", "--baseline-out is not allowed" ; "baseline refresh")]
fn observation_evaluator_is_refused(command: &str, expected: &str) {
    let fake = fake(&sessions());
    let mut live = Live::start(&observation_manifest(&fake), 503, &fake);
    let halt = live
        .begin(&fake, "o1", Role::Candidate, command)
        .unwrap_err();
    assert!(halt_text(&halt).contains(expected), "{}", halt_text(&halt));
}

#[test]
fn observation_run_keeps_the_simulator_timeout_and_boot_cap() {
    let fake = fake(&sessions());
    let mut live = Live::start(&observation_manifest(&fake), 503, &fake);
    let command = "bench --suite tools/bench/suites/research.json --split dev --out /research/503/o/b --timeout 120 --max-boots 400";
    let seq = live
        .run(
            &fake,
            "o1",
            Role::Candidate,
            command,
            End::Exited { code: 0 },
        )
        .unwrap();
    live.decide(&fake, "o1", Outcome::Kept).unwrap();
    let decision = live.campaign.ledger.outcome("o1").unwrap();
    assert_eq!(decision.runs, [seq]);
    let started = live.campaign.ledger.starts().next().unwrap();
    assert!(
        started
            .args
            .windows(2)
            .any(|pair| pair == ["--timeout", "120"])
    );
    assert!(
        started
            .args
            .windows(2)
            .any(|pair| pair == ["--max-boots", "400"])
    );
}

#[test]
fn observation_manifest_cannot_write_the_simulator() {
    let fake = fake(&sessions());
    let mut manifest = observation_manifest(&fake);
    manifest["writable"] = json!(["crates/togi-sim/src"]);
    let error = start(&serde_json::to_vec(&manifest).unwrap(), 503, &fake, at(0)).unwrap_err();
    assert!(
        error.to_string().contains("overlaps crates/togi-sim"),
        "{error}"
    );
}

#[test]
fn change_outside_writable_paths_stops_the_campaign() {
    let mut fake = fake(&sessions());
    let mut live = Live::start(&model_manifest(&fake), 427, &fake);
    fake.edit("crates/togi-sim/src/model.rs", "candidate");
    live.run(
        &fake,
        "h1",
        Role::Candidate,
        FORWARD,
        End::Exited { code: 0 },
    )
    .unwrap();
    fake.edit("crates/togi-tuner/src/lib.rs", "a policy change");
    let halt = live
        .begin(&fake, "h1", Role::Candidate, FORWARD)
        .unwrap_err();
    assert!(
        halt.to_string().contains("crates/togi-tuner/src/lib.rs"),
        "{halt}"
    );
    assert_eq!(live.stopped(), Some(StopReason::ForbiddenDiff));
}

#[test]
fn reference_run_needs_the_reference_tree() {
    let mut fake = fake(&sessions());
    let mut live = Live::start(&model_manifest(&fake), 427, &fake);
    fake.edit("crates/togi-sim/src/model.rs", "candidate");
    let halt = live
        .begin(&fake, "h1", Role::Reference, FORWARD)
        .unwrap_err();
    assert!(
        halt.to_string().contains("needs the tree at the reference"),
        "{halt}"
    );
    assert_eq!(live.stopped(), None);
}

#[test]
fn wall_limit_stops_the_campaign() {
    let fake = fake(&sessions());
    let mut live = Live::start(&model_manifest(&fake), 427, &fake);
    live.minute = 480;
    let halt = live
        .begin(&fake, "h1", Role::Reference, FORWARD)
        .unwrap_err();
    assert!(
        matches!(
            halt,
            Halt::Stop {
                reason: StopReason::WallLimit,
                ..
            }
        ),
        "{halt}"
    );
    assert_eq!(live.stopped(), Some(StopReason::WallLimit));
}

#[test]
fn a_backwards_campaign_time_is_refused_without_resetting_its_budget() {
    let fake = fake(&sessions());
    let live = Live::start(&model_manifest(&fake), 427, &fake);
    let before = live.campaign.ledger.records.len();
    let halt = live
        .campaign
        .guard(&fake, at(0) - SignedDuration::from_nanos(1))
        .unwrap_err();
    assert!(matches!(halt, Halt::Refuse(Refusal::Elapsed(_))), "{halt}");
    assert_eq!(live.campaign.ledger.records.len(), before);
    assert_eq!(
        live.campaign.guard(&fake, at(0)).unwrap(),
        std::time::Duration::from_hours(8),
    );
    assert_eq!(
        live.campaign
            .guard(&fake, at(0) + SignedDuration::from_nanos(1))
            .unwrap(),
        std::time::Duration::from_hours(8) - std::time::Duration::from_nanos(1),
    );
}

#[test]
fn run_stopped_at_the_wall_limit_stops_the_campaign() {
    let fake = fake(&sessions());
    let mut live = Live::start(&model_manifest(&fake), 427, &fake);
    live.run(&fake, "h1", Role::Reference, FORWARD, End::TimedOut)
        .unwrap();
    assert_eq!(live.stopped(), Some(StopReason::WallLimit));
    let torn = without_last_record(&live);
    assert_eq!(
        torn.stopped().map(|(reason, _)| reason),
        Some(StopReason::WallLimit)
    );
}

#[test]
fn candidate_limit_stops_the_campaign() {
    let mut fake = fake(&sessions());
    let mut manifest = model_manifest(&fake);
    manifest["limits"] = json!({"wall_minutes": 480, "candidates": 1});
    let mut live = Live::start(&manifest, 427, &fake);
    fake.edit("crates/togi-sim/src/model.rs", "candidate one");
    live.run(
        &fake,
        "h1",
        Role::Candidate,
        FORWARD,
        End::Exited { code: 1 },
    )
    .unwrap();
    live.run(
        &fake,
        "h1",
        Role::Candidate,
        FORWARD,
        End::Exited { code: 0 },
    )
    .unwrap();
    live.decide(&fake, "h1", Outcome::Rejected).unwrap();
    fake.edit("crates/togi-sim/src/model.rs", "candidate two");
    let halt = live
        .begin(&fake, "h2", Role::Candidate, FORWARD)
        .unwrap_err();
    assert!(
        matches!(
            halt,
            Halt::Stop {
                reason: StopReason::CandidateLimit,
                ..
            }
        ),
        "{halt}"
    );
    assert_eq!(live.stopped(), Some(StopReason::CandidateLimit));
}

#[test]
fn unknown_candidate_is_refused() {
    let fake = fake(&sessions());
    let mut live = Live::start(&model_manifest(&fake), 427, &fake);
    let halt = live
        .begin(&fake, "h9", Role::Candidate, FORWARD)
        .unwrap_err();
    assert!(
        matches!(halt, Halt::Refuse(Refusal::UnknownCandidate(_))),
        "{halt}"
    );
}

#[test]
fn outcomes_append_and_never_replace() {
    let mut fake = fake(&sessions());
    let mut live = Live::start(&model_manifest(&fake), 427, &fake);
    fake.edit("crates/togi-sim/src/model.rs", "candidate");
    live.run(
        &fake,
        "h1",
        Role::Candidate,
        FORWARD,
        End::Exited { code: 0 },
    )
    .unwrap();
    live.decide(&fake, "h1", Outcome::Rejected).unwrap();
    let before = live.campaign.ledger.records.clone();
    let again = live.decide(&fake, "h1", Outcome::Kept).unwrap_err();
    assert!(
        again.to_string().contains("already has an outcome"),
        "{again}"
    );
    let rerun = live
        .begin(&fake, "h1", Role::Candidate, FORWARD)
        .unwrap_err();
    assert!(
        rerun.to_string().contains("already has an outcome"),
        "{rerun}"
    );
    assert_eq!(live.campaign.ledger.records, before);
    let rejected = live.campaign.ledger.outcome("h1").unwrap();
    assert_eq!(rejected.outcome, Outcome::Rejected);
    assert_eq!(
        rejected.metrics["brier"].to_bits(),
        0.123_456_789_012_345_67_f64.to_bits()
    );
}

#[test]
fn outcome_needs_metrics_unless_invalid() {
    let fake = fake(&sessions());
    let mut live = Live::start(&model_manifest(&fake), 427, &fake);
    let now = live.now();
    let missing = live
        .campaign
        .decide(
            &fake,
            now,
            "h1",
            Outcome::Rejected,
            "worse",
            BTreeMap::new(),
        )
        .unwrap_err();
    assert!(
        matches!(missing, Halt::Refuse(Refusal::NoMetrics)),
        "{missing}"
    );
    let invalid = live
        .campaign
        .decide(
            &fake,
            now,
            "h1",
            Outcome::Invalid,
            "edited a frozen file",
            BTreeMap::new(),
        )
        .unwrap();
    assert_eq!(invalid.len(), 1);
}

#[test]
fn keep_without_a_paired_reference_stops_the_campaign() {
    let mut fake = fake(&sessions());
    let mut live = Live::start(&model_manifest(&fake), 427, &fake);
    live.run(
        &fake,
        "h1",
        Role::Reference,
        &format!("{FORWARD} --jobs 2"),
        End::Exited { code: 0 },
    )
    .unwrap();
    fake.edit("crates/togi-sim/src/model.rs", "candidate");
    live.run(
        &fake,
        "h1",
        Role::Candidate,
        FORWARD,
        End::Exited { code: 0 },
    )
    .unwrap();
    let halt = live.decide(&fake, "h1", Outcome::Kept).unwrap_err();
    assert!(
        matches!(
            halt,
            Halt::Stop {
                reason: StopReason::UnpairedComparison,
                ..
            }
        ),
        "{halt}"
    );
    assert_eq!(live.stopped(), Some(StopReason::UnpairedComparison));
}

#[test]
fn keep_needs_a_complete_candidate_run() {
    let mut fake = fake(&sessions());
    let mut live = Live::start(&model_manifest(&fake), 427, &fake);
    live.run(
        &fake,
        "h1",
        Role::Reference,
        FORWARD,
        End::Exited { code: 0 },
    )
    .unwrap();
    fake.edit("crates/togi-sim/src/model.rs", "candidate");
    live.run(
        &fake,
        "h1",
        Role::Candidate,
        FORWARD,
        End::Exited { code: 101 },
    )
    .unwrap();
    live.decide(&fake, "h1", Outcome::Kept).unwrap_err();
    assert_eq!(live.stopped(), Some(StopReason::UnpairedComparison));
}

/// Runs h1 through a kept development comparison, leaving the tree at the kept candidate.
fn kept(fake: &mut Fake) -> Live {
    let mut live = Live::start(&model_manifest(fake), 427, fake);
    live.run(
        fake,
        "h1",
        Role::Reference,
        FORWARD,
        End::Exited { code: 0 },
    )
    .unwrap();
    fake.edit("crates/togi-sim/src/model.rs", "candidate");
    live.run(
        fake,
        "h1",
        Role::Candidate,
        FORWARD,
        End::Exited { code: 0 },
    )
    .unwrap();
    live.decide(fake, "h1", Outcome::Kept).unwrap();
    live
}

#[test]
fn runs_that_write_their_own_records_still_pair() {
    let mut fake = fake(&sessions());
    let mut live = Live::start(&model_manifest(&fake), 427, &fake);
    let complete = End::Exited { code: 0 };
    let reference = live
        .run(
            &fake,
            "h1",
            Role::Reference,
            &format!("{FORWARD} --records {RUN}/reference.jsonl"),
            complete,
        )
        .unwrap();
    fake.edit("crates/togi-sim/src/model.rs", "candidate");
    let candidate = live
        .run(
            &fake,
            "h1",
            Role::Candidate,
            &format!("{FORWARD} --records={RUN}/candidate.jsonl"),
            complete,
        )
        .unwrap();
    live.decide(&fake, "h1", Outcome::Kept).unwrap();
    assert_eq!(
        live.campaign.ledger.outcome("h1").unwrap().runs,
        [reference, candidate]
    );
    let confirmation = live
        .run(
            &fake,
            "h1",
            Role::Confirmation,
            &format!("{CONFIRM} --records {RUN}/confirmation.jsonl"),
            complete,
        )
        .unwrap();
    fake.revert("crates/togi-sim/src/model.rs");
    let confirmation_reference = live
        .run(
            &fake,
            "h1",
            Role::ConfirmationReference,
            &format!("{CONFIRM} --records {RUN}/confirmation-reference.jsonl"),
            complete,
        )
        .unwrap();
    fake.edit("crates/togi-sim/src/model.rs", "candidate");
    live.decide(&fake, "h1", Outcome::ConfirmationPassed)
        .unwrap();
    assert_eq!(
        live.campaign.ledger.outcome("h1").unwrap().runs,
        [confirmation_reference, confirmation]
    );
}

#[test]
fn confirmation_runs_once_then_finalizes() {
    let mut fake = fake(&sessions());
    let mut live = kept(&mut fake);
    let confirmation = live
        .run(
            &fake,
            "h1",
            Role::Confirmation,
            CONFIRM,
            End::Exited { code: 0 },
        )
        .unwrap();
    fake.revert("crates/togi-sim/src/model.rs");
    let reference = live
        .run(
            &fake,
            "h1",
            Role::ConfirmationReference,
            CONFIRM,
            End::Exited { code: 0 },
        )
        .unwrap();
    fake.edit("crates/togi-sim/src/model.rs", "candidate");
    live.decide(&fake, "h1", Outcome::ConfirmationPassed)
        .unwrap();
    assert_eq!(
        live.campaign.ledger.outcome("h1").unwrap().runs,
        [reference, confirmation]
    );
    let again = live
        .begin(&fake, "h1", Role::Confirmation, CONFIRM)
        .unwrap_err();
    assert!(again.to_string().contains("already spent"), "{again}");
    live.run(
        &fake,
        "h1",
        Role::Finalization,
        "fit --facts facts/original.jsonl.gz --out /research/427/t1/fit",
        End::Exited { code: 0 },
    )
    .unwrap();
    let outside = live
        .begin(
            &fake,
            "h1",
            Role::Finalization,
            "fit --facts facts/original.jsonl.gz --out tools/bench/fit",
        )
        .unwrap_err();
    assert!(
        outside.to_string().contains("inside /research/427/t1"),
        "{outside}"
    );
    live.run(
        &fake,
        "h1",
        Role::Finalization,
        "bench --split all --baseline tools/bench/baseline.jsonl --out /research/427/t1/f5.jsonl",
        End::Exited { code: 0 },
    )
    .unwrap();
    let unwritten = live
        .begin(
            &fake,
            "h1",
            Role::Finalization,
            "fit --facts facts/original.jsonl.gz",
        )
        .unwrap_err();
    assert!(
        unwritten
            .to_string()
            .contains("--out must name a directory inside"),
        "{unwritten}"
    );
}

/// Runs h1 through a passed confirmation, leaving the tree at the confirmed candidate.
fn confirmed(fake: &mut Fake) -> Live {
    let mut live = kept(fake);
    live.run(
        fake,
        "h1",
        Role::Confirmation,
        CONFIRM,
        End::Exited { code: 0 },
    )
    .unwrap();
    fake.revert("crates/togi-sim/src/model.rs");
    live.run(
        fake,
        "h1",
        Role::ConfirmationReference,
        CONFIRM,
        End::Exited { code: 0 },
    )
    .unwrap();
    fake.edit("crates/togi-sim/src/model.rs", "candidate");
    live.decide(fake, "h1", Outcome::ConfirmationPassed)
        .unwrap();
    live
}

#[test_case(Role::Candidate, "bench --split all --baseline tools/bench/baseline.jsonl", "only as the F5 finalization check" ; "F5 in a candidate run")]
#[test_case(Role::Finalization, "bench --split dev --baseline tools/bench/baseline.jsonl", "F5 runs the committed suite" ; "F5 on another split")]
#[test_case(Role::Finalization, "bench --split all --baseline other.jsonl", "F5 runs the committed suite" ; "F5 against another baseline")]
#[test_case(Role::Finalization, "bench --suite tools/bench/suites/research.json --split all --baseline tools/bench/baseline.jsonl", "F5 runs the committed suite" ; "F5 on another suite")]
fn model_development_benches_only_f5(role: Role, command: &str, expected: &str) {
    let mut fake = fake(&sessions());
    let mut live = confirmed(&mut fake);
    let halt = live.begin(&fake, "h1", role, command).unwrap_err();
    assert!(matches!(halt, Halt::Refuse(_)), "{halt}");
    assert!(halt_text(&halt).contains(expected), "{}", halt_text(&halt));
}

#[test]
fn confirmation_outcome_is_recorded_once() {
    let mut fake = fake(&sessions());
    let mut live = confirmed(&mut fake);
    let before = live.campaign.ledger.records.clone();
    let again = live
        .decide(&fake, "h1", Outcome::ConfirmationFailed)
        .unwrap_err();
    assert!(
        again
            .to_string()
            .contains("not a kept candidate awaiting its confirmation"),
        "{again}"
    );
    assert_eq!(live.campaign.ledger.records, before);
}

#[test]
fn confirmation_without_its_reference_run_stops_unpaired() {
    let mut fake = fake(&sessions());
    let mut live = kept(&mut fake);
    live.run(
        &fake,
        "h1",
        Role::Confirmation,
        CONFIRM,
        End::Exited { code: 0 },
    )
    .unwrap();
    let halt = live
        .decide(&fake, "h1", Outcome::ConfirmationPassed)
        .unwrap_err();
    assert!(
        halt.to_string().contains("no complete reference run"),
        "{halt}"
    );
    assert_eq!(live.stopped(), Some(StopReason::UnpairedComparison));
}

#[test]
fn head_leaving_the_checkout_history_stops_the_campaign() {
    let mut fake = fake(&sessions());
    let mut live = Live::start(&model_manifest(&fake), 427, &fake);
    fake.head = ORPHAN.to_owned();
    let halt = live
        .begin(&fake, "h1", Role::Reference, FORWARD)
        .unwrap_err();
    assert!(
        halt.to_string()
            .contains("does not descend from the checkout"),
        "{halt}"
    );
    assert_eq!(live.stopped(), Some(StopReason::ForbiddenDiff));
}

#[test]
fn confirmation_scores_exactly_the_kept_candidate() {
    let mut fake = fake(&sessions());
    let mut live = kept(&mut fake);
    fake.edit("crates/togi-sim/src/model.rs", "tuned after the keep");
    let halt = live
        .begin(&fake, "h1", Role::Confirmation, CONFIRM)
        .unwrap_err();
    assert!(
        halt.to_string().contains("is not the kept candidate"),
        "{halt}"
    );
}

#[test]
fn confirmation_needs_a_kept_candidate_and_comes_before_its_reference() {
    let mut fake = fake(&sessions());
    let mut live = Live::start(&model_manifest(&fake), 427, &fake);
    let early = live
        .begin(&fake, "h1", Role::ConfirmationReference, CONFIRM)
        .unwrap_err();
    assert!(early.to_string().contains("has not started"), "{early}");
    fake.edit("crates/togi-sim/src/model.rs", "candidate");
    let unkept = live
        .begin(&fake, "h1", Role::Confirmation, CONFIRM)
        .unwrap_err();
    assert!(
        unkept.to_string().contains("not a kept candidate"),
        "{unkept}"
    );
    let sealed = live
        .begin(&fake, "h1", Role::Confirmation, FORWARD)
        .unwrap_err();
    assert!(sealed.to_string().contains("unsealed"), "{sealed}");
}

#[test]
fn failed_confirmation_stops_the_campaign() {
    let mut fake = fake(&sessions());
    let mut live = kept(&mut fake);
    live.run(
        &fake,
        "h1",
        Role::Confirmation,
        CONFIRM,
        End::Exited { code: 0 },
    )
    .unwrap();
    fake.revert("crates/togi-sim/src/model.rs");
    live.run(
        &fake,
        "h1",
        Role::ConfirmationReference,
        CONFIRM,
        End::Exited { code: 0 },
    )
    .unwrap();
    live.decide(&fake, "h1", Outcome::ConfirmationFailed)
        .unwrap();
    assert_eq!(live.stopped(), Some(StopReason::ConfirmationFailed));
    let torn = without_last_record(&live);
    assert_eq!(
        torn.stopped().map(|(reason, _)| reason),
        Some(StopReason::ConfirmationFailed)
    );
}

/// The ledger as a torn append that lost its last line leaves it.
fn without_last_record(live: &Live) -> Ledger {
    let records = &live.campaign.ledger.records;
    assert!(matches!(
        records.last(),
        Some(Record::CampaignStopped { .. })
    ));
    Ledger {
        records: records[..records.len() - 1].to_vec(),
    }
}

#[test]
fn issue_without_confirmations_never_confirms() {
    let fake = fake(&sessions());
    let mut manifest = model_manifest(&fake);
    manifest["confirmations"] = json!(0);
    let mut live = Live::start(&manifest, 427, &fake);
    live.campaign.ledger.records.push(Record::Outcome(Decision {
        at: at(1),
        candidate: "h1".to_owned(),
        outcome: Outcome::Kept,
        reason: "kept".to_owned(),
        metrics: BTreeMap::new(),
        runs: Vec::new(),
    }));
    let halt = live
        .begin(&fake, "h1", Role::Confirmation, CONFIRM)
        .unwrap_err();
    assert!(
        halt.to_string().contains("all 0 confirmations are spent"),
        "{halt}"
    );
}

#[test]
fn interrupted_confirmation_is_incomplete_on_resume_and_stays_spent() {
    let mut fake = fake(&sessions());
    let mut live = kept(&mut fake);
    let confirmation = live
        .begin(&fake, "h1", Role::Confirmation, CONFIRM)
        .unwrap()
        .started
        .seq;
    let dangling = live
        .begin(&fake, "h1", Role::Confirmation, CONFIRM)
        .unwrap_err();
    assert!(
        matches!(&dangling, Halt::Refuse(Refusal::Dangling(runs)) if runs == &[confirmation]),
        "{dangling}"
    );
    let now = live.now();
    let records = live.campaign.resume(&fake, now).unwrap();
    assert!(matches!(
        records.first(),
        Some(Record::ExperimentEnded(Ended { end: End::Incomplete, seq, .. })) if *seq == confirmation
    ));
    live.apply(Ok(records)).unwrap();
    let again = live
        .begin(&fake, "h1", Role::Confirmation, CONFIRM)
        .unwrap_err();
    assert!(again.to_string().contains("already spent"), "{again}");
    let unpaired = live
        .decide(&fake, "h1", Outcome::ConfirmationPassed)
        .unwrap_err();
    assert!(
        matches!(
            unpaired,
            Halt::Stop {
                reason: StopReason::UnpairedComparison,
                ..
            }
        ),
        "{unpaired}"
    );
}

#[test]
fn stopped_campaign_does_not_resume() {
    let fake = fake(&sessions());
    let mut live = Live::start(&model_manifest(&fake), 427, &fake);
    let now = live.now();
    let record = live
        .campaign
        .stop(now, StopReason::QueueExhausted, "all tried")
        .unwrap();
    live.apply(Ok(vec![record])).unwrap();
    let error = live.campaign.resume(&fake, now).unwrap_err();
    assert!(
        matches!(
            error,
            Halt::Refuse(Refusal::Stopped(StopReason::QueueExhausted))
        ),
        "{error}"
    );
    let twice = live
        .campaign
        .stop(now, StopReason::Interrupted, "")
        .unwrap_err();
    assert!(matches!(twice, Refusal::Stopped(_)), "{twice}");
}

#[test]
fn ledger_round_trips_through_json_lines() {
    let mut fake = fake(&sessions());
    let live = kept(&mut fake);
    let mut bytes = Vec::new();
    for record in &live.campaign.ledger.records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    assert_eq!(Ledger::parse(&bytes).unwrap(), live.campaign.ledger);
    let headless = bytes.splitn(2, |&byte| byte == b'\n').nth(1).unwrap();
    assert!(matches!(Ledger::parse(headless), Err(LedgerError::NoStart)));
}

#[test]
fn report_names_the_stop_outcomes_incomplete_runs_and_notes() {
    let mut fake = fake(&sessions());
    let mut live = kept(&mut fake);
    live.begin(&fake, "h1", Role::Confirmation, CONFIRM)
        .unwrap();
    let now = live.now();
    let records = live.campaign.resume(&fake, now).unwrap();
    live.apply(Ok(records)).unwrap();
    let now = live.now();
    let record = live
        .campaign
        .stop(now, StopReason::Interrupted, "owner stopped it")
        .unwrap();
    live.apply(Ok(vec![record])).unwrap();
    let report = render_report(&live.campaign, "The confirmation never completed.\n");
    for expected in [
        "# Research campaign t1: #427, model development",
        "- Stop: interrupted (owner stopped it)",
        "- Confirmations spent: 1 of 1",
        "- Resumed: 1 times",
        "| h1 | a shared core term | 1, 2, 3 | kept | by the brief's keep rule |",
        "| h2 | a structured singleton term | – | not run |  |",
        "| brier | 0.12345678901234566 |",
        "incomplete: found unfinished on resume",
        "Incomplete runs: 3.\nNo outcome rests on an incomplete run.",
        "historical, non-blind check",
        "## Remaining uncertainty\n\nThe confirmation never completed.",
    ] {
        assert!(
            report.contains(expected),
            "missing {expected:?} in\n{report}"
        );
    }
}

#[test]
fn report_lists_outcomes_on_incomplete_runs_and_hides_the_run_directory() {
    let fake = fake(&sessions());
    let mut live = Live::start(&observation_manifest(&fake), 503, &fake);
    live.run(
        &fake,
        "o1",
        Role::Candidate,
        "bench --suite tools/bench/suites/research.json --split dev --out /research/503/o/b.jsonl",
        End::Exited { code: 1 },
    )
    .unwrap();
    live.decide(&fake, "o1", Outcome::Rejected).unwrap();
    let report = render_report(&live.campaign, "");
    assert!(report.contains("--out $RUN/b.jsonl"), "{report}");
    assert!(!report.contains("/research/503/o"), "{report}");
    assert!(
        report
            .contains("Incomplete runs: 1.\nOutcomes that list an incomplete run: o1 (rejected)."),
        "{report}"
    );
}

fn lines(records: &[Record]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    bytes
}

#[test]
fn torn_tail_is_no_record_and_the_next_append_keeps_it() {
    let mut fake = fake(&sessions());
    let live = kept(&mut fake);
    let records = &live.campaign.ledger.records;
    let mut bytes = lines(records);
    let fragment = br#"{"event":"campaign_resumed","at":"#;
    bytes.extend_from_slice(fragment);
    assert_eq!(Ledger::parse(&bytes).unwrap().records, *records);
    let later = Record::CampaignResumed {
        at: at(30),
        head: CHECKOUT.to_owned(),
    };
    bytes.extend(super::ledger::end_torn_tail(&bytes, at(30)).unwrap());
    bytes.extend(lines(std::slice::from_ref(&later)));
    let torn = Record::LedgerTorn {
        at: at(30),
        discarded: crate::suite::hex(fragment),
    };
    let expected: Vec<Record> = records.iter().cloned().chain([torn, later]).collect();
    assert_eq!(Ledger::parse(&bytes).unwrap().records, expected);
    assert!(
        super::ledger::end_torn_tail(&bytes, at(31))
            .unwrap()
            .is_empty()
    );
}

#[test_case(1 ; "torn after the newline that ends the fragment")]
#[test_case(12 ; "torn inside the ledger_torn line")]
fn torn_repair_is_kept_with_the_fragment_it_ended(written: usize) {
    let start = Record::CampaignStarted {
        at: at(0),
        manifest_sha256: sha256(b"manifest"),
        head: CHECKOUT.to_owned(),
    };
    let mut bytes = lines(std::slice::from_ref(&start));
    let clean = bytes.len();
    bytes.extend_from_slice(br#"{"event":"campaign_res"#);
    let repair = super::ledger::end_torn_tail(&bytes, at(1)).unwrap();
    bytes.extend_from_slice(&repair[..written]);
    assert_eq!(
        Ledger::parse(&bytes).unwrap().records,
        std::slice::from_ref(&start)
    );
    let region = bytes[clean..]
        .strip_suffix(b"\n")
        .unwrap_or(&bytes[clean..]);
    let torn = Record::LedgerTorn {
        at: at(2),
        discarded: crate::suite::hex(region),
    };
    let later = Record::CampaignResumed {
        at: at(2),
        head: CHECKOUT.to_owned(),
    };
    bytes.extend(super::ledger::end_torn_tail(&bytes, at(2)).unwrap());
    bytes.extend(lines(std::slice::from_ref(&later)));
    assert_eq!(Ledger::parse(&bytes).unwrap().records, [start, torn, later]);
}

#[test_case("" ; "alone")]
#[test_case("{\"event\":\"campaign_res\n" ; "after a cut-off line")]
fn record_that_lost_only_its_newline_stays_discarded(before: &str) {
    let start = Record::CampaignStarted {
        at: at(0),
        manifest_sha256: sha256(b"manifest"),
        head: CHECKOUT.to_owned(),
    };
    let mut bytes = lines(std::slice::from_ref(&start));
    let clean = bytes.len();
    bytes.extend_from_slice(before.as_bytes());
    let lost = Record::CampaignResumed {
        at: at(1),
        head: CHECKOUT.to_owned(),
    };
    serde_json::to_writer(&mut bytes, &lost).unwrap();
    assert_eq!(
        Ledger::parse(&bytes).unwrap().records,
        std::slice::from_ref(&start)
    );
    let torn = Record::LedgerTorn {
        at: at(2),
        discarded: crate::suite::hex(&bytes[clean..]),
    };
    let later = Record::CampaignResumed {
        at: at(2),
        head: CHECKOUT.to_owned(),
    };
    bytes.extend(super::ledger::end_torn_tail(&bytes, at(2)).unwrap());
    bytes.extend(lines(std::slice::from_ref(&later)));
    assert_eq!(Ledger::parse(&bytes).unwrap().records, [start, torn, later]);
}

#[test_case(r#"{"event":"campaign_resumed","at":"2027-01-15T08:00:00Z"}"# ; "missing field")]
#[test_case(r#"{"event":"campaign_resumed","at":"2027-01-15T08:00:00Z","head":"c","note":"x"}"# ; "unknown field")]
#[test_case(r#"["campaign_resumed","2027-01-15T08:00:00Z","c"]"# ; "record as an array")]
#[test_case(r#"{"event":"experiment_ended","seq":1,"at":"2027-01-15T08:00:00Z","end":["exited",0],"stdout_sha256":null,"stderr_sha256":null}"# ; "end as an array")]
#[test_case(r#"{"event":"campaign_resumed","at":"# ; "fragment not followed by its ledger_torn")]
fn ledger_line_is_refused(line: &str) {
    let mut bytes = lines(&[Record::CampaignStarted {
        at: at(0),
        manifest_sha256: sha256(b"manifest"),
        head: CHECKOUT.to_owned(),
    }]);
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    bytes.extend(lines(&[Record::CampaignResumed {
        at: at(1),
        head: CHECKOUT.to_owned(),
    }]));
    let error = Ledger::parse(&bytes).unwrap_err();
    assert!(
        matches!(error, LedgerError::Decode { line: 2, .. }),
        "{error}"
    );
}

#[test_case(r#"{"event":"campaign_resumed","at":"2027-01-15T08:00:00Z","head":"c","note":"x"}"# ; "unknown field")]
#[test_case(r#"["campaign_resumed","2027-01-15T08:00:00Z","c"]"# ; "record as an array")]
#[test_case(r#"{"event":"campaign_stopped","at":"2027-01-15T08:00:00Z","reason":"wall-limit","detail":1}"# ; "damaged stop line")]
fn invalid_last_line_is_refused_not_torn(line: &str) {
    let mut bytes = lines(&[Record::CampaignStarted {
        at: at(0),
        manifest_sha256: sha256(b"manifest"),
        head: CHECKOUT.to_owned(),
    }]);
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    let error = Ledger::parse(&bytes).unwrap_err();
    assert!(
        matches!(error, LedgerError::Decode { line: 2, .. }),
        "{error}"
    );
    assert!(
        super::ledger::end_torn_tail(&bytes, at(1))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn real_fs_run_dir_appends_and_holds_its_lock() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("research/427/t1");
    let dir = RunDir::create(&path).unwrap();
    let first = Record::CampaignStarted {
        at: at(0),
        manifest_sha256: sha256(b"manifest"),
        head: CHECKOUT.to_owned(),
    };
    dir.append(std::slice::from_ref(&first)).unwrap();
    let written = std::fs::read(path.join("experiments.jsonl")).unwrap();
    let second = Record::CampaignResumed {
        at: at(1),
        head: CHECKOUT.to_owned(),
    };
    dir.append(std::slice::from_ref(&second)).unwrap();
    let grown = std::fs::read(path.join("experiments.jsonl")).unwrap();
    assert!(grown.starts_with(&written));
    assert_eq!(dir.ledger().unwrap().records, [first, second]);
    assert!(matches!(RunDir::open(&path), Err(ResearchError::Busy(_))));
    assert!(matches!(
        RunDir::create(&path),
        Err(ResearchError::Exists(_))
    ));
    drop(dir);
    let _reopened = RunDir::open(&path).unwrap();
}

#[test]
fn real_fs_append_ends_a_torn_line_and_keeps_it() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("t1");
    let dir = RunDir::create(&path).unwrap();
    let first = Record::CampaignStarted {
        at: at(0),
        manifest_sha256: sha256(b"manifest"),
        head: CHECKOUT.to_owned(),
    };
    dir.append(std::slice::from_ref(&first)).unwrap();
    let fragment = br#"{"event":"campaign_res"#;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path.join("experiments.jsonl"))
        .unwrap();
    file.write_all(fragment).unwrap();
    drop(file);
    assert_eq!(dir.ledger().unwrap().records, std::slice::from_ref(&first));
    let second = Record::CampaignResumed {
        at: at(1),
        head: CHECKOUT.to_owned(),
    };
    dir.append(std::slice::from_ref(&second)).unwrap();
    let records = dir.ledger().unwrap().records;
    assert_eq!(records.len(), 3, "{records:?}");
    assert_eq!(records[0], first);
    assert!(
        matches!(&records[1], Record::LedgerTorn { discarded, .. } if *discarded == crate::suite::hex(fragment)),
        "{records:?}"
    );
    assert_eq!(records[2], second);
}
