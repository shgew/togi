use std::fmt::Write as _;

use jiff::Timestamp;

use super::campaign::Campaign;
use super::ledger::{End, Record};

/// Renders `report.md`: the campaign's limits and stop, every candidate with its runs and outcome, the
/// confirmations, the incomplete runs and the researcher's notes on what remains uncertain.
pub fn render(campaign: &Campaign, notes: &str) -> String {
    let manifest = &campaign.manifest;
    let ledger = &campaign.ledger;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Research campaign {}: #{}, {}\n",
        manifest.run_id(),
        manifest.issue,
        manifest.mode.name()
    );
    let _ = writeln!(out, "- Brief: `{}`", campaign.issue.brief());
    let _ = writeln!(
        out,
        "- Checkout: `{}`; reference: `{}`",
        manifest.checkout, manifest.reference
    );
    if let Some(sha) = ledger.manifest_sha256() {
        let _ = writeln!(out, "- Manifest SHA-256: `{sha}`");
    }
    match ledger.stopped() {
        Some((reason, detail)) => {
            let _ = writeln!(out, "- Stop: {} ({detail})", reason.name());
        }
        None => {
            let _ = writeln!(out, "- Stop: none recorded; the campaign is not finished");
        }
    }
    let used = wall_used(campaign);
    let _ = writeln!(
        out,
        "- Wall time: {used} of {} minutes",
        manifest.limits.wall_minutes
    );
    let _ = writeln!(
        out,
        "- Candidates: {} of {}; workers: {}",
        ledger.candidates().len(),
        manifest.limits.candidates,
        manifest.limits.jobs
    );
    let confirmations = ledger.confirmations();
    let _ = writeln!(
        out,
        "- Confirmations spent: {} of {}",
        confirmations.len(),
        manifest.confirmations
    );
    let resumes = ledger
        .records
        .iter()
        .filter(|record| matches!(record, Record::CampaignResumed { .. }))
        .count();
    let _ = writeln!(out, "- Resumed: {resumes} times");

    let _ = writeln!(out, "\n## Candidates\n");
    let _ = writeln!(out, "| Candidate | Statement | Runs | Outcome | Reason |");
    let _ = writeln!(out, "|---|---|---|---|---|");
    for hypothesis in &manifest.hypotheses {
        let runs: Vec<String> = ledger
            .starts()
            .filter(|started| started.candidate == hypothesis.id)
            .map(|started| started.seq.to_string())
            .collect();
        let decisions: Vec<_> = ledger
            .decisions()
            .filter(|decision| decision.candidate == hypothesis.id)
            .collect();
        let outcome = if decisions.is_empty() {
            if runs.is_empty() {
                "not run".to_owned()
            } else {
                "no outcome".to_owned()
            }
        } else {
            decisions
                .iter()
                .map(|decision| decision.outcome.name())
                .collect::<Vec<_>>()
                .join(", then ")
        };
        let reason = decisions
            .iter()
            .map(|decision| decision.reason.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        let _ = writeln!(
            out,
            "| {} | {} | {} | {outcome} | {} |",
            hypothesis.id,
            cell(&hypothesis.statement),
            if runs.is_empty() {
                "–".to_owned()
            } else {
                runs.join(", ")
            },
            cell(&reason)
        );
    }

    let decisions: Vec<_> = ledger.decisions().collect();
    if !decisions.is_empty() {
        let _ = writeln!(out, "\n## Metrics\n");
        for decision in decisions {
            let runs: Vec<String> = decision.runs.iter().map(u32::to_string).collect();
            let _ = writeln!(
                out,
                "### {}: {} (runs {})\n",
                decision.candidate,
                decision.outcome.name(),
                runs.join(", ")
            );
            if decision.metrics.is_empty() {
                let _ = writeln!(out, "No metrics.\n");
                continue;
            }
            let _ = writeln!(out, "| Metric | Value |");
            let _ = writeln!(out, "|---|---|");
            for (name, value) in &decision.metrics {
                let _ = writeln!(out, "| {} | {value:?} |", cell(name));
            }
            let _ = writeln!(out);
        }
    } else {
        let _ = writeln!(out);
    }

    let _ = writeln!(out, "## Runs\n");
    let _ = writeln!(
        out,
        "| Run | Candidate | Role | Command | End | Tree | Output |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|");
    let mut incomplete = Vec::new();
    for started in ledger.starts() {
        let end = ledger.end(started.seq).map(|ended| ended.end);
        if !end.is_some_and(super::ledger::End::complete) {
            incomplete.push(started.seq);
        }
        let _ = writeln!(
            out,
            "| {} | {} | {} | `just {} {}` | {} | `{}` | `{}`, `{}` |",
            started.seq,
            started.candidate,
            started.role.name(),
            started.recipe.name(),
            started.args.join(" "),
            end.map_or_else(|| "never ended".to_owned(), End::describe),
            short(&started.tree_sha256),
            started.stdout,
            started.stderr
        );
    }
    let _ = writeln!(out);
    if incomplete.is_empty() {
        let _ = writeln!(out, "Every run completed.");
    } else {
        let list: Vec<String> = incomplete.iter().map(u32::to_string).collect();
        let _ = writeln!(
            out,
            "Incomplete runs: {}. No outcome rests on an incomplete run.",
            list.join(", ")
        );
    }
    if !confirmations.is_empty() {
        let _ = writeln!(out, "\n## Confirmations\n");
        for started in confirmations {
            let result = ledger
                .decisions()
                .filter(|decision| decision.candidate == started.candidate)
                .find(|decision| decision.runs.contains(&started.seq))
                .map_or("no recorded result", |decision| decision.outcome.name());
            let end = ledger
                .end(started.seq)
                .map_or_else(|| "never ended".to_owned(), |ended| ended.end.describe());
            let _ = writeln!(
                out,
                "- {} (run {}, {end}): {result}; candidate tree `{}`, reference `{}`",
                started.candidate,
                started.seq,
                short(&started.tree_sha256),
                started.reference
            );
        }
        let _ = writeln!(
            out,
            "\nThe confirmation data were scored before this campaign; this is a historical, non-blind check."
        );
    }
    let _ = writeln!(out, "\n## Remaining uncertainty\n");
    if notes.trim().is_empty() {
        let _ = writeln!(out, "No notes supplied.");
    } else {
        let _ = writeln!(out, "{}", notes.trim_end());
    }
    out
}

fn wall_used(campaign: &Campaign) -> i64 {
    let ledger = &campaign.ledger;
    let last = ledger.records.iter().map(at).max();
    match (ledger.started_at(), last) {
        (Some(start), Some(last)) => last.duration_since(start).as_secs() / 60,
        _ => 0,
    }
}

fn at(record: &Record) -> Timestamp {
    match record {
        Record::CampaignStarted { at, .. }
        | Record::CampaignResumed { at, .. }
        | Record::CampaignStopped { at, .. } => *at,
        Record::ExperimentStarted(started) => started.at,
        Record::ExperimentEnded(ended) => ended.at,
        Record::Outcome(decision) => decision.at,
    }
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}
