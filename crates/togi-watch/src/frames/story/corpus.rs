//! What the narration hands the narration corpus (#904): the story and the resting band as built, and the outcome
//! branches before they merge, the rows after, and the lines as fitted to their width.

use ratatui::text::Line;
use togi_tuner::{Branch, Premise, Uncertainty};

use super::outcome::{OutcomeRow, Phrase, fit_phrases};
use super::{RestingBand, Story};
use crate::corpus::{lines, shape, text};
use crate::screen::Colours;

fn premise(premise: Premise) -> &'static str {
    match premise {
        Premise::Pass => "pass",
        Premise::AllPass => "all pass",
        Premise::Named(_) => "named",
        Premise::Unnamed => "unnamed",
        Premise::Inconclusive => "inconclusive",
    }
}

fn uncertainty(uncertainty: Uncertainty) -> &'static str {
    match uncertainty {
        Uncertainty::NeedsRanking => "needs ranking",
        Uncertainty::NeedsHistory => "needs history",
        Uncertainty::RecurringPlan => "recurring plan",
        Uncertainty::Stopped { .. } => "stopped",
    }
}

/// The story as built: its label, its lines and its one brief line, before the narrator wraps or cuts them.
pub(super) fn said(said: &Story) {
    let form = shape(&said.label);
    text("story.label", "", &said.label);
    for each in &said.lines {
        text("story.line", &form, each);
    }
    text("story.brief", &form, &said.brief);
}

/// The resting band as built: its title and its rows, before the band is cut to its width.
pub(super) fn band(band: &RestingBand) {
    text("band.title", "", band.title);
    for each in &band.lines {
        text("band.line", band.title, each);
    }
}

/// One branch of the forecast in words, before branches that say the same merge: each phrase, the whole line and
/// the compact form, under the premise and the uncertainty that shaped it.
pub(super) fn branch(
    branch: &Branch,
    phrases: &[Phrase],
    compact: &str,
    basis: Option<&'static str>,
) {
    let mut form = premise(branch.premise).to_owned();
    if let Some(unclear) = branch.uncertainty {
        form.push(' ');
        form.push_str(uncertainty(unclear));
    }
    for each in phrases {
        text("outcome.phrase", &form, &each.text);
    }
    text(
        "outcome.branch",
        &form,
        &fit_phrases(phrases, usize::MAX >> 1, Colours::None),
    );
    text("outcome.branch.compact", &form, compact);
    if let Some(basis) = basis {
        text("outcome.basis", "", basis);
    }
}

/// The rows after merging and the row limit, each in all its forms.
pub(super) fn rows(rows: &[OutcomeRow]) {
    for row in rows {
        text("outcome.row.label", "", &row.label);
        text("outcome.row.short", "", &row.short);
        text("outcome.row.compact", &row.short, &row.compact);
        text("outcome.row.brief", &row.short, &row.brief);
        text(
            "outcome.row.full",
            &row.label,
            &fit_phrases(&row.phrases, usize::MAX >> 1, Colours::None),
        );
    }
}

/// The outcome lines as fitted to their width: the detailed rows, or the compact line.
pub(super) fn fitted(fitted: &[Line<'static>], compact: bool) {
    lines(
        "outcome.fitted",
        if compact { "compact" } else { "detail" },
        fitted,
    );
}
