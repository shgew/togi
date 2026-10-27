// A machine file that predates a model change: absent keys decode as their defaults, and a singleton
// machine without new structure keeps every hazard and failure probability it had (#427's absent-key
// fixture).

use togi_machine::{LEGACY_REGIMES, Regime, pick_workload};
use togi_sim::file;

use crate::common::{load, model, offsets};

/// Every single-core feature the model has: distinct alone and together limits per regime, a flat rate,
/// an idle limit, a workload override, a near-limit rate and an onset boost, on the default past-limit
/// rate and growth.
const SINGLETON: &str = r#"{
  "cores": 4,
  "model": {"near_limit_rate": 0.0001, "onset_boost": 1.5},
  "core": [
    {"id": 0, "alone": [-30, -28, -26, -24, -22], "together": [-27, -25, -23, -21, -19, -17, -15]},
    {"id": 1, "alone": [-20, -20, -20, -20, -20], "together": [-18, -18, -18, -18, -18, -18, -16], "flat": 0.00005},
    {"id": 2, "alone": [-35, -35, -35, -35, -35], "together": [-33, -33, -33, -33, -33, -33, -30], "idle": -12},
    {"id": 3, "alone": [-25, -25, -25, -25, -25], "together": [-24, -24, -24, -24, -24, -24, -20], "workload": {"mprime-sse-4k-21k": -10}}
  ]
}"#;

/// [`SINGLETON`] with every key it leaves out written at its default.
const EXPLICIT: &str = r#"{
  "description": "",
  "notes": [],
  "cores": 4,
  "facts": null,
  "bios": null,
  "ranking": null,
  "old_kernel": false,
  "bios_context": null,
  "model": {
    "past_limit_rate": 0.025584278811044955, "growth": 4, "near_limit_rate": 0.0001, "onset_boost": 1.5,
    "crash_mce": 0.5, "core_local_bank": 0.8, "onset_s": 100,
    "signals": {"computation_error": 4, "stall": 1, "unexpected_exit": 1, "corrected_mce": 2, "crash": 2},
    "regime_signals": {}, "reset": {"watchdog": 1}
  },
  "ccd": null,
  "core": [
    {"id": 0, "alone": [-30, -28, -26, -24, -22], "together": [-27, -25, -23, -21, -19, -17, -15], "flat": 0, "idle": null, "workload": {}},
    {"id": 1, "alone": [-20, -20, -20, -20, -20], "together": [-18, -18, -18, -18, -18, -18, -16], "flat": 0.00005, "idle": null, "workload": {}},
    {"id": 2, "alone": [-35, -35, -35, -35, -35], "together": [-33, -33, -33, -33, -33, -33, -30], "flat": 0, "idle": -12, "workload": {}},
    {"id": 3, "alone": [-25, -25, -25, -25, -25], "together": [-24, -24, -24, -24, -24, -24, -20], "flat": 0, "idle": null, "workload": {"mprime-sse-4k-21k": -10}}
  ],
  "joint": [],
  "script": [],
  "shared_voltage": null
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

/// One probe group: a regime, alone (other cores at 0) or together (the next core at -13, one count
/// past core 2's idle limit), and one of the regime's first two workloads.
#[derive(Debug, PartialEq)]
struct Probe {
    regime: Regime,
    together: bool,
    workload: &'static str,
    /// Sums over every loaded core and its offsets 0, -12, -19 and -23 of the steady-state hazard, which
    /// never saturates, and of the failure probability of a 60-second trial, inside the onset window,
    /// and of a 300-second one, past it. Every core's sources count, the unloaded cores' too.
    sums: [f64; 3],
}

fn singleton_probes(text: &str) -> Vec<Probe> {
    let model = model(&file::decode(text).expect("decodes"));
    let mut probes = Vec::new();
    for regime in LEGACY_REGIMES {
        for together in [false, true] {
            for index in 0..2 {
                let workload = pick_workload(regime, index).id;
                let mut sums = [0.0; 3];
                for core in 0..4_i64 {
                    for offset in [0, -12, -19, -23] {
                        let mut values = [0; 4];
                        if together {
                            values[usize::try_from((core + 1) % 4).expect("a core")] = -13;
                        }
                        values[usize::try_from(core).expect("a core")] = offset;
                        let profile = offsets(&values);
                        let short = load(regime, workload, &[core], 60.0);
                        let long = load(regime, workload, &[core], 300.0);
                        sums[0] += model.hazard(&profile, &short).expect("a 4-core profile");
                        sums[1] += model
                            .failure_probability(&profile, &short)
                            .expect("a 4-core profile");
                        sums[2] += model
                            .failure_probability(&profile, &long)
                            .expect("a 4-core profile");
                    }
                }
                probes.push(Probe {
                    regime,
                    together,
                    workload,
                    sums,
                });
            }
        }
    }
    probes
}

#[expect(
    clippy::unreadable_literal,
    reason = "the sums as the assertion prints them, so a deliberate change pastes in"
)]
#[test]
fn a_singleton_machine_without_new_keys_keeps_its_failure_probabilities() {
    let want: [[f64; 3]; 28] = [
        [430910.17614886124, 4.193377914139232, 4.570569740436946],
        [0.4109984609767192, 1.238042310773512, 1.7025772949376465],
        [430916.44439717, 7.120685841855479, 7.481082801020593],
        [6.679246769682731, 4.16535023848976, 4.613090355521294],
        [0.4109984609767192, 1.238042310773512, 1.7025772949376465],
        [0.4109984609767192, 1.238042310773512, 1.7025772949376465],
        [6.679246769682731, 4.16535023848976, 4.613090355521294],
        [6.679246769682731, 4.16535023848976, 4.613090355521294],
        [0.4109984609767192, 1.238042310773512, 1.7025772949376465],
        [0.4109984609767192, 1.238042310773512, 1.7025772949376465],
        [6.679246769682731, 4.16535023848976, 4.613090355521294],
        [6.679246769682731, 4.16535023848976, 4.613090355521294],
        [0.4109984609767192, 1.238042310773512, 1.7025772949376465],
        [0.4109984609767192, 1.238042310773512, 1.7025772949376465],
        [6.781483884926911, 5.1431012618494085, 5.547818076137321],
        [6.781483884926911, 5.1431012618494085, 5.547818076137321],
        [0.43648273978776414, 2.201609903476256, 2.6585647767707448],
        [0.43648273978776414, 2.201609903476256, 2.6585647767707448],
        [8.31654061358961, 5.143101475683096, 5.547818076137321],
        [8.31654061358961, 5.143101475683096, 5.547818076137321],
        [32.87714827219279, 4.164633843008183, 4.549290201707503],
        [32.87714827219279, 4.164633843008183, 4.549290201707503],
        [32.97968538743697, 6.120852499042745, 6.482545796753349],
        [32.97968538743697, 6.120852499042745, 6.482545796753349],
        [526.4233708160608, 5.171129366776036, 5.505297461052975],
        [526.4233708160608, 5.171129366776036, 5.505297461052975],
        [526.525907931305, 7.106425343193124, 7.438543278684223],
        [526.525907931305, 7.106425343193124, 7.438543278684223],
    ];
    let got = singleton_probes(SINGLETON);
    assert_eq!(got.len(), want.len(), "{got:?}");
    for (probe, want) in got.iter().zip(want) {
        assert_eq!(
            probe.sums.map(f64::to_bits),
            want.map(f64::to_bits),
            "{probe:?} != {want:?}"
        );
    }
    assert_eq!(singleton_probes(EXPLICIT), got);
}
