use std::path::Path;

use togi_tool_fit::{
    Ensemble, Error, ForwardCheck, forward_check, generate_records, report_forward,
    shared_voltage_anchor_records,
};
use togi_tool_trialfacts::TrialFact;

use crate::output::MemoryOutput;
use crate::support::{evidence, sixteen_core_evidence};

fn roundtrip(records: &[TrialFact]) -> Vec<TrialFact> {
    let mut bytes = Vec::new();
    togi_tool_trialfacts::encode(&mut bytes, records).unwrap();
    togi_tool_trialfacts::decode(bytes.as_slice()).unwrap()
}

struct Output {
    machines: Vec<String>,
    report: String,
    forward: ForwardCheck,
}

fn ensemble_on(threads: usize, dir: &Path, extract: &Path, records: &[TrialFact]) -> Output {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap();
    let out = dir.join(threads.to_string()).join("machines");
    let mut report = Vec::new();
    let mut output = MemoryOutput::default();
    pool.install(|| {
        generate_records(
            records,
            extract,
            &out,
            Ensemble {
                seed: 263,
                bootstrap: 3,
            },
            &mut report,
            &mut output,
        )
    })
    .unwrap();
    let machines = (0..4)
        .map(|n| {
            output
                .read_to_string(&out.join(format!("target-fit-{n}.json")))
                .unwrap()
        })
        .collect();
    let forward = pool.install(|| forward_check(records, 0)).unwrap();
    let report = String::from_utf8(report)
        .unwrap()
        .replace(out.to_str().unwrap(), "OUT");
    Output {
        machines,
        report,
        forward,
    }
}

#[test]
fn the_same_input_and_seed_give_byte_identical_output_on_every_pool_size() {
    let dir = Path::new("/fit");
    let extract = dir.join("facts.jsonl.gz");
    let records = roundtrip(&evidence());
    let reference = ensemble_on(1, dir, &extract, &records);
    assert!(
        reference.machines[0].contains("\"joint\""),
        "the fixture must exercise the joint search: {}",
        reference.machines[0]
    );
    assert_eq!(reference.forward.rows.len(), 2);
    let mut rendered = Vec::new();
    report_forward(&mut rendered, &reference.forward, 0).unwrap();
    for threads in [2, 5] {
        let got = ensemble_on(threads, dir, &extract, &records);
        assert_eq!(
            reference.machines, got.machines,
            "machine files differ on {threads} threads"
        );
        assert_eq!(
            reference.report, got.report,
            "the report differs on {threads} threads"
        );
        assert_eq!(
            reference.forward, got.forward,
            "the forward-chained check differs on {threads} threads"
        );
        let mut again = Vec::new();
        report_forward(&mut again, &got.forward, 0).unwrap();
        assert_eq!(rendered, again);
    }
}

struct Anchored {
    machine: String,
    report: String,
}

fn anchor_on(threads: usize, dir: &Path, extract: &Path, records: &[TrialFact]) -> Anchored {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap();
    let out = dir.join(threads.to_string()).join("machines");
    let mut report = Vec::new();
    let mut output = MemoryOutput::default();
    let fitted = pool.install(|| {
        shared_voltage_anchor_records(records, extract, &out, &mut report, &mut output)
    });
    let report = String::from_utf8(report)
        .unwrap()
        .replace(out.to_str().unwrap(), "OUT");
    if let Err(error) = fitted {
        let flagged = matches!(error, Error::AnchorFlagged);
        panic!("the anchor was not written (flagged: {flagged}): {error}\n{report}");
    }
    Anchored {
        machine: output
            .read_to_string(&out.join("target-shared-voltage.json"))
            .unwrap(),
        report,
    }
}

#[test]
fn the_shared_voltage_anchor_is_byte_identical_on_every_pool_size() {
    let dir = Path::new("/fit");
    let extract = dir.join("facts.jsonl.gz");
    let records = roundtrip(&sixteen_core_evidence());
    let reference = anchor_on(1, dir, &extract, &records);
    assert!(
        reference.report.contains("Wrote IN-SAMPLE anchor: "),
        "{}",
        reference.report
    );
    assert!(
        reference.machine.contains("shared_voltage") && reference.machine.contains("\"alone\""),
        "the fixture must fit both the rail model and the single-core limits: {}",
        reference.machine
    );
    for threads in [2, 5] {
        let got = anchor_on(threads, dir, &extract, &records);
        assert_eq!(
            reference.machine, got.machine,
            "the anchor file differs on {threads} threads"
        );
        assert_eq!(
            reference.report, got.report,
            "the anchor report differs on {threads} threads"
        );
    }
}
