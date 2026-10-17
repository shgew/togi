// A machine file that predates a model change: absent keys decode as their defaults, and a singleton
// machine without new structure keeps every failure probability it had (#427's absent-key fixture).

use togi_machine::{REGIMES, pick_workload};
use togi_sim::file;

use crate::common::{load, model, offsets};

/// Every single-core feature the model has: distinct alone and together limits per regime, a flat rate,
/// an idle limit, a workload override, a near-limit rate and an onset boost.
const SINGLETON: &str = r#"{
  "cores": 4,
  "model": {"past_limit_rate": 0.02, "growth": 2.5, "near_limit_rate": 0.0001, "onset_boost": 1.5},
  "core": [
    {"id": 0, "alone": [-30, -28, -26, -24, -22], "together": [-27, -25, -23, -21, -19, -17, -15]},
    {"id": 1, "alone": [-20, -20, -20, -20, -20], "together": [-18, -18, -18, -18, -18, -18, -16], "flat": 0.00005},
    {"id": 2, "alone": [-35, -35, -35, -35, -35], "together": [-33, -33, -33, -33, -33, -33, -30], "idle": -12},
    {"id": 3, "alone": [-25, -25, -25, -25, -25], "together": [-24, -24, -24, -24, -24, -24, -20], "workload": {"mprime-sse-4k-21k": -10}}
  ]
}"#;

/// [`SINGLETON`] with every optional key written at its default.
const EXPLICIT: &str = r#"{
  "description": "",
  "notes": [],
  "cores": 4,
  "old_kernel": false,
  "model": {
    "past_limit_rate": 0.02, "growth": 2.5, "near_limit_rate": 0.0001, "onset_boost": 1.5,
    "crash_mce": 0.5, "core_local_bank": 0.8, "onset_s": 100,
    "signals": {"computation_error": 4, "stall": 1, "unexpected_exit": 1, "corrected_mce": 2, "crash": 2},
    "regime_signals": {}, "reset": {"watchdog": 1}
  },
  "core": [
    {"id": 0, "alone": [-30, -28, -26, -24, -22], "together": [-27, -25, -23, -21, -19, -17, -15], "flat": 0, "idle": null, "workload": {}},
    {"id": 1, "alone": [-20, -20, -20, -20, -20], "together": [-18, -18, -18, -18, -18, -18, -16], "flat": 0.00005, "idle": null, "workload": {}},
    {"id": 2, "alone": [-35, -35, -35, -35, -35], "together": [-33, -33, -33, -33, -33, -33, -30], "flat": 0, "idle": -12, "workload": {}},
    {"id": 3, "alone": [-25, -25, -25, -25, -25], "together": [-24, -24, -24, -24, -24, -24, -20], "flat": 0, "workload": {"mprime-sse-4k-21k": -10}}
  ],
  "joint": [],
  "script": []
}"#;

#[test]
fn optional_keys_left_out_decode_as_their_defaults() {
    let absent = file::decode(SINGLETON).expect("decodes");
    let explicit = file::decode(EXPLICIT).expect("decodes");
    assert_eq!(absent, explicit);
    assert_eq!(
        file::encode(&explicit).expect("encodes"),
        file::encode(&absent).expect("encodes")
    );
}

/// For each regime, alone (other cores at 0) and together (the next core at -40), and the first two
/// workloads: the sum over every core and offset of the loaded core's 60-second failure probability.
fn singleton_sums(text: &str) -> Vec<f64> {
    let model = model(&file::decode(text).expect("decodes"));
    let mut sums = Vec::new();
    for regime in REGIMES {
        for together in [false, true] {
            for index in 0..2 {
                let workload = pick_workload(regime, index).id;
                let mut sum = 0.0;
                for core in 0..4_i64 {
                    for offset in [0, -12, -19, -23] {
                        let mut profile = [0; 4];
                        if together {
                            profile[usize::try_from((core + 1) % 4).expect("a core")] = -40;
                        }
                        profile[usize::try_from(core).expect("a core")] = offset;
                        sum += model
                            .failure_probability(
                                &offsets(&profile),
                                &load(regime, workload, &[core], 60.0),
                            )
                            .expect("a 4-core profile");
                    }
                }
                sums.push(sum);
            }
        }
    }
    sums
}

#[expect(
    clippy::unreadable_literal,
    reason = "the sums as the assertion prints them, so a deliberate change pastes in"
)]
#[test]
fn a_singleton_machine_without_new_keys_keeps_its_failure_probabilities() {
    let want: [f64; 28] = [
        4.192825038072174,
        1.238042303633133,
        7.162882268841193,
        4.208099534402153,
        1.238042303633133,
        1.238042303633133,
        4.208099534402153,
        4.208099534402153,
        1.238042303633133,
        1.238042303633133,
        4.208099534402153,
        4.208099534402153,
        1.238042303633133,
        1.238042303633133,
        5.185301819841435,
        5.185301819841435,
        2.1733671748683316,
        2.1733671748683316,
        5.185850771595489,
        5.185850771595489,
        4.136049280680522,
        4.136049280680522,
        6.163053057034771,
        6.163053057034771,
        5.171129352441524,
        5.171129352441524,
        7.148713941197754,
        7.148713941197754,
    ];
    let got = singleton_sums(SINGLETON);
    assert_eq!(got.len(), want.len(), "{got:?}");
    for (index, (got, want)) in got.iter().zip(want).enumerate() {
        assert_eq!(
            got.to_bits(),
            want.to_bits(),
            "sum {index}: {got:?} != {want:?}"
        );
    }
    assert_eq!(singleton_sums(EXPLICIT), got);
}
