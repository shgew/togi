use std::path::Path;

use togi_tool_forecast::{Metric, Record, Row, Score, Status, read_dir, recovery_bias};

use crate::error::Error;
use crate::report::{optional, stamp};
use crate::table::{Table, row};

/// Scores the run recorded in `state_dir` after the forecast's anchor and renders the score.
pub fn score(state_dir: impl AsRef<Path>, forecast: impl AsRef<Path>) -> Result<String, Error> {
    let forecast = forecast.as_ref();
    let record = Record::read(forecast)?;
    let sessions = read_dir(state_dir, Some(record.anchor.session))?;
    let score = record.score(&sessions).map_err(|source| Error::Score {
        path: forecast.display().to_string(),
        source,
    })?;
    Ok(render_score(&score))
}

fn status_name(status: Status) -> &'static str {
    match status {
        Status::Concluded => "concluded",
        Status::DeadEnd => "dead end",
        Status::Censored => "censored",
    }
}

pub(crate) fn render_score(score: &Score<'_>) -> String {
    let record = score.record;
    let summary = &record.summary;
    let mut table = Table::default();
    table.line(&format!(
        "Forecast: commit {}, ruleset {}, evidence epoch {}, {} runs: {} concluded, {} dead ends, {} censored",
        record.commit,
        record.ruleset,
        record.evidence,
        summary.runs,
        summary.concluded,
        summary.dead_ends,
        summary.censored
    ));
    table.line(&format!(
        "Anchor: session {} event {} at {}, unchanged",
        record.anchor.session,
        record.anchor.seq,
        stamp(record.anchor.time)
    ));
    let builds: Vec<String> = score
        .builds
        .iter()
        .map(|build| {
            format!(
                "rev {} ruleset {} epoch {}",
                build
                    .rev
                    .as_deref()
                    .filter(|rev| !rev.is_empty())
                    .unwrap_or("none"),
                build.ruleset,
                build.evidence
            )
        })
        .collect();
    table.line(&format!(
        "Run: {} {:.2} h after the anchor; builds after the anchor: {}",
        status_name(score.outcome.status),
        score.outcome.hours,
        if builds.is_empty() {
            "none".to_owned()
        } else {
            builds.join(", ")
        }
    ));
    for flag in &score.flags {
        table.line(&format!("Flag: {flag}"));
    }
    if score.outcome.status == Status::Censored {
        table.line(
            "Censored: the run has not concluded, so its hours, crashes and hunts are lower bounds, and offsets and depth are those it stopped at",
        );
    }
    table.line(&format!(
        "Note: forecast hours run to conclusion over concluded runs only; {}",
        recovery_bias()
    ));
    table.section(
        "Forecast score",
        &[
            "metric",
            "actual",
            "inside",
            "error",
            "percentile",
            "median",
            "p10..p90",
            "min..max",
        ],
    );
    for r in &score.rows {
        score_row(&mut table, r);
    }
    table.finish()
}

fn score_row(table: &mut Table, r: &Row) {
    let m: &Metric = &r.metric;
    let bound = if r.bound { ">=" } else { "" };
    let actual = format!("{bound}{}", m.format(r.actual));
    let Some(range) = m.range else {
        row!(table, m.kind, actual, r.placement, "-", "-", "-", "-", "-");
        return;
    };
    let error = r.error.map(|error| {
        let text = m.format(error);
        if error > 0.0 {
            format!("+{text}")
        } else {
            text
        }
    });
    row!(
        table,
        m.kind,
        actual,
        r.placement,
        format!("{bound}{}", optional(error)),
        format!(
            "{bound}{}",
            optional(r.percentile.map(|p| format!("{p:.0}")))
        ),
        m.format(range.median),
        format!("{}..{}", m.format(range.p10), m.format(range.p90)),
        format!("{}..{}", m.format(range.min), m.format(range.max))
    );
}
