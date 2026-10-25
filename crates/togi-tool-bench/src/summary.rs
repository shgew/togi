use std::io;

use togi_sim::saturating_count;

use crate::record::{Record, Status};
use crate::stats::{percentile, share};

/// Writes the per-scenario summary of `records`, scenarios in the order the records first name them, then the
/// total.
pub fn report(w: &mut impl io::Write, records: &[Record]) -> io::Result<()> {
    writeln!(
        w,
        "summary: time and crashes include all run statuses; hazard is steady-state failures/hour with all cores loaded."
    )?;
    writeln!(
        w,
        "summary: censored runs reached the simulator's boot cap without concluding; their time and crashes are lower bounds."
    )?;
    writeln!(
        w,
        "scenario          runs concluded censored median_h mean_h median_crashes max_hazard/h real_answer_share"
    )?;
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
    let hazard = rows.iter().map(|r| r.hazard_max_per_h).fold(0.0, f64::max);
    writeln!(
        w,
        "{name:<17} {:>4} {:>9} {:>8} {:>8.3} {:>8.3} {:>14.1} {:>12.6} {:>17.6}",
        rows.len(),
        count(Status::Concluded),
        count(Status::Censored),
        percentile(&hours, 0.5),
        hours.iter().sum::<f64>() / saturating_count(rows.len()),
        percentile(&crashes, 0.5),
        hazard,
        share(real, trials),
    )?;
    Ok(())
}
