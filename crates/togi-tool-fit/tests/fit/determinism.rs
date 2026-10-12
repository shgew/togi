use std::fs;
use std::thread::available_parallelism;

use togi_tool_fit::{Ensemble, ForwardCheck, forward_check, generate, report_forward};
use togi_tool_trialfacts::read;

use crate::support::{evidence, write_extract};

struct Output {
    machines: Vec<String>,
    report: String,
    forward: ForwardCheck,
}

fn ensemble_on(threads: usize, dir: &std::path::Path, extract: &std::path::Path) -> Output {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap();
    let out = dir.join(threads.to_string()).join("machines");
    let mut report = Vec::new();
    pool.install(|| {
        generate(
            extract,
            &out,
            Ensemble {
                seed: 263,
                bootstrap: 3,
            },
            &mut report,
        )
    })
    .unwrap();
    let machines = (0..4)
        .map(|n| fs::read_to_string(out.join(format!("target-fit-{n}.json"))).unwrap())
        .collect();
    let facts = read(extract).unwrap();
    let forward = pool.install(|| forward_check(&facts, 0)).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let extract = write_extract(dir.path(), &evidence());
    let all = available_parallelism().map_or(4, std::num::NonZeroUsize::get);
    let reference = ensemble_on(1, dir.path(), &extract);
    assert!(
        reference.machines[0].contains("\"joint\""),
        "the fixture must exercise the joint search: {}",
        reference.machines[0]
    );
    assert_eq!(reference.forward.rows.len(), 2);
    let mut rendered = Vec::new();
    report_forward(&mut rendered, &reference.forward, 0).unwrap();
    for threads in [2, all] {
        let got = ensemble_on(threads, dir.path(), &extract);
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
