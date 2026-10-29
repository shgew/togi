//! Test-only builder for a synthetic successor registration document.
//!
//! Self-contained (only `serde_json`) so a binary test can path-include it. The caller passes
//! the scenario ids; scenario roles are assigned from each id's name, with an optional
//! `successor-` prefix: benign (`edge-deep`, `no-write-limit-failure`,
//! `within-ccd-heterogeneous`), retention (`stock-background`), the 44-depth floor
//! (`no-write-limit-failure`), moving (`moving-cap`), firmware (`firmware-change`) and the
//! adversarial families. Source identities and scenario parameters are synthetic.

use serde_json::{Value, json};

pub(crate) const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
pub(crate) const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const REDUCED_DEV_LABEL: &str = "EXPLORATORY REDUCED DEV — NOT A REGISTERED SCORE";

fn family(name: &str) -> &str {
    name.strip_prefix("successor-").unwrap_or(name)
}

pub(crate) fn scenario(name: &str) -> Value {
    let family = family(name);
    // A moving-limit epoch is invisible to a policy and keeps the context; a firmware change opens a new one.
    let (transition, epochs) = match family {
        "moving-cap" => ("moving", vec![(1, "before", "ctx"), (400, "after", "ctx")]),
        "firmware-change" => (
            "firmware",
            vec![(1, "before", "ctx-before"), (181, "after", "ctx-after")],
        ),
        _ => ("fixed", vec![(1, "only", "ctx")]),
    };
    let adversarial = matches!(
        family,
        "hidden-daily"
            | "delayed-onset"
            | "thermal-history"
            | "partial-only"
            | "weak-core-hidden"
            | "weak-core-delayed"
            | "combined-adversary"
    );
    json!({
        "name": name,
        "description": "synthetic fixture scenario",
        "topology": {"ccds": 2, "cores_per_ccd": 8},
        "daily_mix": [{"class": "compute", "seconds": 14400}, {"class": "idle-heavy", "seconds": 14400}],
        "epochs": epochs.iter().map(|(day, id, context)| json!({
            "id": id, "start_day": day, "context": context, "description": "synthetic epoch",
        })).collect::<Vec<_>>(),
        "transition": transition,
        "parameters": [{"name": "edge", "value": 30.0}],
        "coverage": if adversarial { "adversarial" } else { "covered" },
        "benign": matches!(family, "no-write-limit-failure" | "edge-deep" | "within-ccd-heterogeneous"),
        "retention": family == "stock-background",
        "unit_depth_floor": if family == "no-write-limit-failure" { json!(44) } else { Value::Null },
        "modes": Value::Null,
    })
}

pub(crate) fn policy(name: &str, role: &str) -> Value {
    json!({
        "name": name, "identity": format!("{name}-identity"), "role": role,
        "source": {"kind": "pinned", "revision": REVISION, "sha256": HASH},
        "parameters_sha256": HASH,
        "parameters": [{"name": "allowance", "value": 2.0}],
    })
}

/// A valid ccd-uniform registration document with one scenario per id, in order.
pub(crate) fn value(scenario_ids: &[&str]) -> Value {
    let scenarios: Vec<_> = scenario_ids.iter().copied().map(scenario).collect();
    json!({
        "id": "day-night-successor", "version": 1,
        "lambda_per_hour": {"numerator": 1, "denominator": 3000},
        "slopes": [1.5, 2.0],
        "seeds": {"dev": {"first": 20001, "count": 200}, "holdout": {"first": 30001, "count": 200}},
        "holdout": {
            "authorized_by": "registration custodian", "procedure": "unseal once per candidate and slope after freeze",
            "command": "just bench-day-night --registration R --run-profile registered --split holdout --policy successor --slope 1.5 --out h.jsonl",
            "confirmations_per_slope": 1, "spent_when_started": true, "inspection_before_freeze": false,
        },
        "calendar": {
            "first_day": 1, "days": 730, "daily_active_hours": 8, "night_hours": 8,
            "night_weekdays": [0, 2, 4], "year_days": 365, "firmware_change_day": 181,
            "sleep": "remaining wall hours; thermal state cools",
        },
        "scenarios": scenarios,
        "criteria": {
            "final_in_admissible_fraction": {"numerator": 1, "denominator": 1},
            "held_after_finish_fraction": {"numerator": 95, "denominator": 100},
            "safety_grace_days": 0,
            "post_finish_rate_of_lambda": {"numerator": 1, "denominator": 1},
            "poisson_diagnostic_confidence": {"numerator": 95, "denominator": 100},
            "allowance_limit": 2, "first_step_clean_stock_hours": 50, "clean_hold_hours": 50,
            "restoration_clean_stock_hours": 50, "ceiling_margin": 5,
            "operational_max_depth": 45, "physical_max_depth": 50,
            "useful_fraction": {"numerator": 90, "denominator": 100},
            "useful_depth_of_reference": {"numerator": 80, "denominator": 100},
            "min_unit_depth": 5,
            "settled_fraction": {"numerator": 90, "denominator": 100},
            "settled_horizon_day": 730, "benign_median_settled_day": 180,
            "retention_fraction": {"numerator": 90, "denominator": 100}, "retention_max_loss": 1,
        },
        "run_profiles": [
            {"profile": "reduced-dev", "splits": ["dev"], "seeds": {"kind": "first", "count": 10},
             "slopes_per_invocation": 1, "gate_verdict": false, "label": REDUCED_DEV_LABEL},
            {"profile": "registered", "splits": ["dev", "holdout"], "seeds": {"kind": "all"},
             "slopes_per_invocation": 1, "gate_verdict": true, "label": null},
        ],
        "budgets": {"reduced_dev_seconds": 1800, "registered_seconds": 43200, "holdout_seconds": 43200, "compilation_excluded": true},
        "resources": {"jobs": 8, "resource_class": "workstation"},
        "tolerance": {"admissibility": 1e-9, "hours": 1e-9},
        "unit_modes": [{"mode": "ccd-uniform", "grouping": "one unit per CCD; lambda/n each"}],
        "sources": {
            "simulator_revision": REVISION, "runner_revision": REVISION, "report_revision": REVISION,
            "binary_sha256": Value::Null, "input_sha256": Value::Null,
            "policies": [policy("successor", "candidate"), policy("shipped", "comparator")],
        },
        "commands": {
            "reduced_dev": "just bench-day-night --registration R --run-profile reduced-dev --split dev --policy successor --slope 1.5 --out r.jsonl",
            "registered": "just bench-day-night --registration R --run-profile registered --split dev --policy successor --slope 1.5 --out d.jsonl",
        },
        "streams": {
            "domains": [{"name": "truth", "tag": "T"}, {"name": "policy", "tag": "P"}],
            "topology_permutation": "seed-keyed shuffle of physical-core identities",
        },
        "continuation": {"dev_rule": "resume missing cells in ascending scenario, seed, slope order", "holdout_resumable": false},
        "notes": ["synthetic"],
    })
}
