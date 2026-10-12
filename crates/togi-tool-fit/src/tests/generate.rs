use std::fs;
use std::path::{Path, PathBuf};

use togi_facts::{Decisive, TrialClass};
use togi_machine::{BiosContext, Regime, Signal, TargetOffset, WorkloadId};
use togi_sim::definition::CcdDef;
use togi_sim::file;
use togi_tool_modelcheck::{Group, Interval, Status, check};
use togi_tool_trialfacts::{Class, Extracts, TrialFact};

use super::{context, extract, fact, failure, repeated};
use crate::encode::encode_machine;
use crate::error::{Error, EvidenceError};
use crate::evidence::initial_config;
use crate::generate::{Ensemble, generate};
use crate::signals::fit_signals;

struct Run {
    dir: tempfile::TempDir,
    extract: PathBuf,
}

fn run_dir(facts: &[TrialFact]) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let extract = extract(dir.path(), "facts.jsonl.gz", facts);
    Run { dir, extract }
}

fn pool(threads: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap()
}

fn ensemble(bootstrap: usize) -> Ensemble {
    Ensemble {
        seed: 263,
        bootstrap,
    }
}

fn member(out: &Path, n: usize) -> PathBuf {
    out.join(format!("target-fit-{n}.json"))
}

fn failures(sample: &[TrialFact]) -> usize {
    sample
        .iter()
        .filter(|f| matches!(f.outcome, Decisive::Failure { .. }))
        .count()
}

fn r1_trials(depth: i64, n: usize, failures: usize) -> Vec<TrialFact> {
    let template = fact(&[depth, 0], Regime::R1, &[0], 60, Decisive::Pass);
    let mut trials = repeated(&template, n, 0);
    for t in trials.iter_mut().take(failures) {
        t.outcome = failure(Signal::ComputationError);
    }
    trials
}

#[test]
fn an_ensemble_is_written_checked_against_the_original_evidence_and_reproducible() {
    let records = r1_trials(-10, 12, 6);
    let run = run_dir(&records);
    let mut first: Vec<Vec<u8>> = Vec::new();
    for (i, threads) in [1, 2].into_iter().enumerate() {
        let out = run.dir.path().join(i.to_string()).join("machines");
        let mut report = Vec::new();
        let generated = pool(threads)
            .install(|| generate(&run.extract, &out, ensemble(1), &mut report))
            .unwrap();
        for (n, fitted) in generated.fits.iter().enumerate() {
            let path = member(&out, n);
            let content = fs::read(&path).unwrap();
            let cfg = file::load(&path).unwrap();
            assert_eq!(cfg.facts.as_deref(), Some("../../facts.jsonl.gz"));
            let bios = cfg.bios_context.as_ref().unwrap();
            assert_eq!(bios.board.as_deref(), Some(context().board.as_str()));
            let checked = check(&path, &cfg, &mut Extracts::new()).unwrap();
            assert_eq!(checked.status, Status::Ok, "{checked:?}");
            assert_eq!(
                (
                    checked.groups.len(),
                    checked.groups[0].n,
                    checked.groups[0].k
                ),
                (1, 12, 6)
            );
            assert_eq!(checked.idle_failures, 0);
            assert_eq!(
                checked.groups, fitted.check.groups,
                "member {n}: the reported check differs from the written file's"
            );
            let k = failures(&fitted.sample);
            let p = checked.groups[0].mean_p;
            let want = f64::from(u32::try_from(k).unwrap()) / 12.0;
            assert!(
                (p - want).abs() < 0.02,
                "member {n}: resampled prediction {p}, want {k}/12"
            );
            let kf = f64::from(u32::try_from(k).unwrap());
            let loss = -kf * p.ln() - (12.0 - kf) * (-p).ln_1p();
            assert!(
                (fitted.loss - loss).abs() < 1e-3,
                "member {n}: loss {} want {loss}",
                fitted.loss
            );
            if i == 0 {
                first.push(content);
            } else {
                assert_eq!(first[n], content, "member {n} is not reproducible");
            }
        }
    }
}

#[test]
fn the_ensemble_report_and_files_are_the_same_on_one_thread_and_on_many() {
    struct Case {
        records: Vec<TrialFact>,
        seed: u64,
        constrained_somewhere: bool,
        unconstrained: bool,
    }
    let mut deep = r1_trials(-10, 30, 0);
    deep.extend(r1_trials(-5, 30, 15));
    let cases = [
        Case {
            records: r1_trials(-10, 30, 1),
            seed: 263,
            constrained_somewhere: true,
            unconstrained: false,
        },
        Case {
            records: deep,
            seed: 263,
            constrained_somewhere: false,
            unconstrained: true,
        },
    ];
    for case in cases {
        let run = run_dir(&case.records);
        let mut outputs = Vec::new();
        for threads in [1, 5] {
            let out = run.dir.path().join(threads.to_string()).join("machines");
            let mut report = Vec::new();
            let generated = pool(threads)
                .install(|| {
                    generate(
                        &run.extract,
                        &out,
                        Ensemble {
                            seed: case.seed,
                            bootstrap: 4,
                        },
                        &mut report,
                    )
                })
                .unwrap();
            let files: Vec<String> = (0..5)
                .map(|n| fs::read_to_string(member(&out, n)).unwrap())
                .collect();
            let report = String::from_utf8(report)
                .unwrap()
                .replace(out.to_str().unwrap(), "OUT");
            outputs.push((report, files, generated));
        }
        let (serial, parallel) = (&outputs[0], &outputs[1]);
        assert_eq!(
            serial.0, parallel.0,
            "the parallel report differs from the serial one"
        );
        assert_eq!(
            serial.1, parallel.1,
            "the parallel machines differ from the serial ones"
        );
        let fits = &serial.2.fits;
        let unseeded = serial
            .0
            .lines()
            .filter(|l| l.contains("written unconstrained"))
            .count();
        let constrained = fits.iter().filter(|f| !f.constrained.is_empty()).count();
        assert_eq!(constrained > 0, case.constrained_somewhere, "{}", serial.0);
        if case.unconstrained {
            assert_eq!(unseeded, 4, "{}", serial.0);
            assert!(fits.iter().all(|f| f.constrained.is_empty()));
            assert!(fits.iter().all(|f| f.flagged.len() == 1));
        } else {
            assert_eq!(unseeded, 0);
        }
        for (n, fitted) in fits.iter().enumerate() {
            let flagged: Vec<&Group> = fitted.check.flagged().collect();
            assert_eq!(
                flagged.len(),
                fitted.flagged.len(),
                "member {n}: flags before writing differ from the written file's"
            );
            let notes = &file::decode(&serial.1[n]).unwrap().notes;
            let header: Vec<&String> = notes
                .iter()
                .filter(|note| note.starts_with("Model check") || note.starts_with("Flagged:"))
                .collect();
            assert_eq!(header.len(), 1 + flagged.len(), "member {n}: {notes:?}");
            if flagged.is_empty() {
                assert_eq!(
                    header[0],
                    "Model check against the original extract: ok, no flagged groups."
                );
            } else {
                assert_eq!(
                    header[0],
                    "Model check against the original extract: flagged."
                );
            }
            if !fitted.constrained.is_empty() {
                let base = fits[0].check.groups[0].mean_p;
                let p = fitted.check.groups[0].mean_p;
                assert!(
                    p < base,
                    "refit {n} kept the all-facts prediction {base}, want below it: {p}"
                );
                assert_eq!(
                    failures(&fitted.sample),
                    0,
                    "only a failure-free resample is flagged here"
                );
                assert!(
                    notes
                        .iter()
                        .any(|note| note == "Signal mix: no failures; default weights."),
                    "refit {n} keeps a signal mix its failure-free resample never recorded: {notes:?}"
                );
            }
        }
    }
}

#[test]
fn evidence_with_no_decisive_trial_is_refused_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("machines");
    let mut report = Vec::new();
    let missing = dir.path().join("facts.jsonl.gz");
    let error = generate(&missing, &out, ensemble(0), &mut report).unwrap_err();
    assert!(matches!(error, Error::Read(_)), "{error}");
    let mut idle = r1_trials(-10, 1, 1).remove(0);
    idle.class = Class::Idle {
        loaded: super::cores(&[0]),
    };
    let only_idle = extract(dir.path(), "idle.jsonl.gz", &[idle]);
    let error = generate(&only_idle, &out, ensemble(0), &mut report).unwrap_err();
    assert!(
        matches!(error, Error::Evidence(EvidenceError::NoTrials)),
        "{error}"
    );
    assert!(
        !out.exists(),
        "refused evidence created the output directory"
    );
    assert!(report.is_empty(), "refused evidence reported a fit");
}

#[test]
fn a_destination_that_conflicts_is_refused_and_left_as_it_was() {
    for directory in [false, true] {
        let run = run_dir(&r1_trials(-10, 1, 0));
        let out = run.dir.path().join("machines");
        let (conflict, action) = if directory {
            (member(&out, 0), "write fitted machine")
        } else {
            (out.clone(), "create output directory")
        };
        if directory {
            fs::create_dir_all(&conflict).unwrap();
        } else {
            fs::write(&conflict, "preserve").unwrap();
        }
        let mut report = Vec::new();
        let error = generate(&run.extract, &out, ensemble(0), &mut report).unwrap_err();
        assert!(
            matches!(&error, Error::Io { action: a, .. } if *a == action),
            "{error}"
        );
        assert!(report.is_empty(), "a refused run printed a report");
        assert_eq!(conflict.is_dir(), directory);
        if !directory {
            assert_eq!(fs::read_to_string(&conflict).unwrap(), "preserve");
        }
    }
}

#[test]
fn an_encoded_machine_retains_the_fit_evidence_and_parameters() {
    let ctx = BiosContext {
        bios_version: "A".into(),
        board: "fixture".into(),
        cpu_model: "Zen 5 fixture".into(),
        microcode: "0x1".into(),
        boost_limit_mhz: 5600,
    };
    let mut first = fact(&[0, 0], Regime::R1, &[0], 60, Decisive::Pass);
    first.context = Some(ctx);
    let mut cfg = initial_config(&[first]);
    cfg.ccd = Some(CcdDef {
        log_rate: -9.0,
        slope: 0.1,
        effect: [0.2, -0.3],
    });
    cfg.facts = Some("../facts/extract.jsonl.gz".into());
    cfg.core[0].idle = Some(-30);
    cfg.core[0].workload = [("z".to_owned(), -24), ("a".to_owned(), -20)].into();
    let on = |regime, signal| fact(&[0, 0], regime, &[0], 60, failure(signal));
    fit_signals(
        &mut cfg,
        &[
            on(Regime::R7, Signal::Crash),
            on(Regime::R7, Signal::UncorrectedMce),
            on(Regime::R7, Signal::ComputationError),
            on(Regime::R2, Signal::ComputationError),
            fact(&[0, 0], Regime::R1, &[0], 60, Decisive::Pass),
        ],
    );
    let class = |duration_s| TrialClass {
        workload: WorkloadId::new(Regime::R7, 0).unwrap(),
        loaded: super::cores(&[0, 1]),
        duration_s,
    };
    let group = |duration_s, n, k, interval, flagged| Group {
        context: None,
        class: class(duration_s),
        depth: TargetOffset::new(-24).unwrap(),
        n,
        k,
        interval,
        mean_p: 0.0,
        flagged,
    };
    let constrained = [group(120, 45, 5, Interval { low: 0, high: 0 }, false)];
    let flagged = [group(300, 24, 7, Interval { low: 0, high: 2 }, true)];
    let content = encode_machine(&cfg, 2, 263, 45, 12.5, &constrained, &flagged).unwrap();
    insta::assert_snapshot!("encoded_fit", content);
    let mut decoded = file::decode(&content).unwrap();
    assert_eq!(decoded.notes.len(), 6);
    decoded.notes.clear();
    assert_eq!(decoded, cfg);
}
