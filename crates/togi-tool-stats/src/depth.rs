use std::collections::BTreeMap;

use jiff::Timestamp;
use togi_machine::TargetOffset;

use crate::project::{Class, Projection, Trial};
use crate::report::selected;
use crate::table::{Table, row};

/// A multi-core class's trials counted by the shallowest loaded offset.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct DepthRate {
    pub(crate) class: Class,
    pub(crate) depth: i64,
    pub(crate) trials: u32,
    pub(crate) failures: u32,
}

/// The trials of one class by depth, and whether any failed.
#[derive(Default)]
struct DepthClass {
    depths: BTreeMap<i64, (u32, u32)>,
    failing: bool,
}

pub(crate) fn depth(p: &Projection<'_>, since: Option<Timestamp>) -> Vec<DepthRate> {
    let mut classes: BTreeMap<Class, DepthClass> = BTreeMap::new();
    for t in p
        .trials
        .iter()
        .filter(|t| selected(t.time, since) && t.started && t.class.cores.len() >= 2)
    {
        let Some(depth) = shallowest(t) else {
            continue;
        };
        let class = classes.entry(t.class).or_default();
        let (trials, failures) = class.depths.entry(depth).or_default();
        *trials += 1;
        if t.failed() {
            *failures += 1;
            class.failing = true;
        }
    }
    classes
        .into_iter()
        .filter(|(_, class)| class.failing)
        .flat_map(|(class, depths)| {
            depths
                .depths
                .into_iter()
                .map(move |(depth, (trials, failures))| DepthRate {
                    class,
                    depth,
                    trials,
                    failures,
                })
        })
        .collect()
}

/// The offset of the loaded core closest to zero; `None` when the profile misses a loaded core.
fn shallowest(t: &Trial<'_>) -> Option<i64> {
    t.class
        .cores
        .iter()
        .map(|core| t.intent.profile.get(core).map(TargetOffset::get))
        .try_fold(i64::MIN, |deepest, offset| Some(deepest.max(offset?)))
}

pub(crate) fn render_depth(table: &mut Table, rates: &[DepthRate]) {
    table.section(
        "Failure rate by depth",
        &[
            "regime / workload / loaded cores / duration",
            "shallowest offset",
            "trials",
            "failures",
            "rate",
        ],
    );
    for r in rates {
        row!(
            table,
            r.class.label(),
            r.depth,
            r.trials,
            r.failures,
            format!(
                "{:.1}%",
                100.0 * f64::from(r.failures) / f64::from(r.trials)
            )
        );
    }
}
