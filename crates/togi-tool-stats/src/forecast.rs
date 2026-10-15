use std::path::Path;

use togi_journal::{SessionId, Skipped};
use togi_tool_forecast::{
    Metric, Record, Row, Score, Session, Status, read_dir_lenient, recovery_bias,
};

use crate::error::Error;
use crate::report::{optional, stamp};
use crate::table::{Table, row};

/// A rendered score and what reading the state directory skipped.
#[derive(Debug)]
pub struct Scored {
    pub text: String,
    /// Each session whose journal had events skipped or fields ignored, oldest first.
    pub skipped: Vec<(SessionId, Skipped)>,
}

/// Scores the run recorded in `state_dir` after the forecast's anchor and renders the score. The journals are
/// read leniently: `skipped` says what that left out.
pub fn score(state_dir: impl AsRef<Path>, forecast: impl AsRef<Path>) -> Result<Scored, Error> {
    let forecast = forecast.as_ref();
    score_with(
        forecast,
        || Record::read(forecast).map_err(Error::from),
        |record| read_dir_lenient(state_dir, Some(record.anchor.session)).map_err(Error::from),
    )
}

pub(crate) fn score_with(
    forecast: &Path,
    read_record: impl FnOnce() -> Result<Record, Error>,
    read_sessions: impl FnOnce(&Record) -> Result<Vec<Session>, Error>,
) -> Result<Scored, Error> {
    let record = read_record()?;
    let sessions = read_sessions(&record)?;
    let score = record.score(&sessions).map_err(|source| Error::Score {
        path: forecast.display().to_string(),
        source,
    })?;
    Ok(Scored {
        text: render_score(&score),
        skipped: sessions
            .iter()
            .filter(|session| !session.skipped().is_empty())
            .map(|session| (session.id(), session.skipped().clone()))
            .collect(),
    })
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
