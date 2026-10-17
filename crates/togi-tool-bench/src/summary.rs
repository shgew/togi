use std::io;

use togi_sim::saturating_count;

use crate::record::{Record, Status};
use crate::stats::{percentile, share};

/// Writes the per-scenario summary of `records`, scenarios in the order the records first name them, then the
/// total.
pub fn report(w: &mut impl io::Write, records: &[Record]) -> io::Result<()> {
    writeln!(
        w,
        "summary: time, crashes and depth include all run statuses; hazard is steady-state failures/hour with all cores loaded."
    )?;
    writeln!(
        w,
        "summary: censored runs reached the simulator's boot cap without concluding; their time and crashes are lower bounds."
    )?;
    writeln!(
        w,
        "scenario          runs concluded censored median_h mean_h median_crashes mean_depth max_hazard/h real_answer_share"
    )?;
    if records.iter().any(|r| r.worst_r7_hazard_per_h.is_some()) {
        writeln!(
            w,
            "summary: worst_r7_hazard/h is the maximum shared-voltage final-profile hazard over every R7 workload, full CCD loads, request-ordered partials with at least two cores, and all-core; absent on machines without a shared rail."
        )?;
    }
    let mut groups: Vec<(&str, Vec<&Record>)> = Vec::new();
    for record in records {
        match groups.iter_mut().find(|(name, _)| *name == record.scenario) {
            Some((_, rows)) => rows.push(record),
            None => groups.push((&record.scenario, vec![record])),
        }
    }
    for (name, rows) in &groups {
        row(w, name, rows)?;
    }
    row(w, "TOTAL", &records.iter().collect::<Vec<_>>())
}

pub(crate) fn row(w: &mut impl io::Write, name: &str, rows: &[&Record]) -> io::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut hours: Vec<f64> = rows.iter().map(|r| r.sim_hours).collect();
    let mut crashes: Vec<f64> = rows.iter().map(|r| f64::from(r.crashes)).collect();
    hours.sort_by(f64::total_cmp);
    crashes.sort_by(f64::total_cmp);
    let count = |status| rows.iter().filter(|r| r.status == status).count();
    let real: u32 = rows.iter().map(|r| r.real_answers).sum();
    let trials: u32 = rows.iter().map(|r| r.trials).sum();
    #[expect(clippy::cast_precision_loss, reason = "depths are small integers")]
    let depth = rows.iter().map(|r| r.depth as f64).sum::<f64>() / saturating_count(rows.len());
    let hazard = rows.iter().map(|r| r.hazard_max_per_h).fold(0.0, f64::max);
    writeln!(
        w,
        "{name:<17} {:>4} {:>9} {:>8} {:>8.3} {:>8.3} {:>14.1} {:>10.2} {:>12.6} {:>17.6}",
        rows.len(),
        count(Status::Concluded),
        count(Status::Censored),
        percentile(&hours, 0.5),
        hours.iter().sum::<f64>() / saturating_count(rows.len()),
        percentile(&crashes, 0.5),
        depth,
        hazard,
        share(real, trials),
    )?;
    let measured: Vec<f64> = rows
        .iter()
        .filter_map(|r| r.worst_r7_hazard_per_h)
        .collect();
    if !measured.is_empty() {
        writeln!(
            w,
            "{name} worst_r7_hazard/h={:.6} worst_r7_runs={}",
            measured.iter().copied().fold(0.0, f64::max),
            measured.len(),
        )?;
    }
    Ok(())
}
