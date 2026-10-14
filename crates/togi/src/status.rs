//! `togi status`: the session at a glance, rendered from the journal.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::LazyLock;

use clap::Args as ClapArgs;
use togi_config::FULL_CYCLE;
use togi_journal::{
    CorePhase, DefectDirection, Event, Payload, ResetTarget, Seq, State as JournalState,
};
use togi_machine::{CoreId, CoreSet, Regime};
use togi_render::escape_text;
use togi_tuner::{PhaseNo, Projection};
use togi_watch::model::Snapshot;
use togi_watch::words::{candidate_word, group_word, noun, phase_word};

use crate::cli::{Context, examples};
use crate::exit::Exit;
use crate::read;
use crate::report::fail;
use crate::table::{Row, table, wrap};

/// The widest a hunt group's core list may be and stay in its table cell: the other columns take 37
/// of the 80 cells, so a wider list moves to a note under the row.
const GROUP_CORES_MAX: usize = 43;

static EXAMPLES: LazyLock<String> = LazyLock::new(|| {
    examples(&[
        ("togi status", "The session in the default state directory"),
        (
            "togi --state-dir <dir> status",
            "The session in <dir>, such as a copied state directory",
        ),
    ])
});

/// Show the session at a glance
///
/// The search, hunt, phase 2 round or checking activity and the phase; the BIOS profile to enter,
/// marked * while unconfirmed; then each core's offset, BIOS offset, failure point, phase, queued
/// work and last decision, and its combinations. In phase 2 it lists the candidates with their
/// remaining gaps and the most rounds left if none fails, then one full cycle. An open hunt shows
/// groups and trials; an open phase 2 round shows checks and passes. Evidence lists the workloads'
/// valid trials and the Tctl peak since the last profile change, and between-trial MCEs without
/// treating them as failures. It shows each core's observed self-sufficiency for each R7 workload
/// and the CCD's current top requesters, with offsets standing in only when request telemetry is
/// absent, and lists reset commands for too-cautious defects until each affected core is reset.
/// Read-only, rendered from the journal. A different ruleset warns before rendering; a different
/// schema is refused.
///
/// An event of a kind this build does not know is skipped and an unknown field is ignored; stderr
/// says which, after the output.
#[derive(ClapArgs, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {}

/// Runs `togi status`.
pub fn run(_args: &Args, ctx: &mut Context<'_>) -> Exit {
    let recording = match read::open(ctx, "status") {
        Ok(recording) => recording,
        Err(exit) => return exit,
    };
    let events = recording.events();
    let text = match render(events) {
        Ok(text) => text,
        Err(Problem::NoSession) => {
            let message = format!("no session in {}", ctx.state_dir.display());
            return fail(ctx, "status", message);
        }
        Err(Problem::Replay(message)) => {
            return fail(ctx, "status", message);
        }
    };
    if let Err(error) = ctx
        .out
        .write_all(text.as_bytes())
        .and_then(|()| ctx.out.flush())
    {
        return fail(ctx, "status", format!("write output: {error}"));
    }
    read::report(ctx, "status", &recording);
    Exit::Ok
}

enum Problem {
    NoSession,
    Replay(String),
}

/// Where a hunt's parked offsets came from, as the journal's own message tells them apart.
#[derive(Clone)]
enum Parked {
    Zero,
    CycleEnd(Seq),
    Held(CoreSet),
}

/// What the events say that no fold keeps: notes, hunt and combination sequence numbers, defects.
#[derive(Default)]
struct Scan {
    carried: Vec<(Seq, String)>,
    between: Vec<(Seq, String)>,
    hunt_start: BTreeMap<u32, (Seq, Parked)>,
    combination_seq: BTreeMap<u32, Seq>,
    defects: Vec<Defect>,
}

struct Defect {
    id: u32,
    title: String,
    pr: u32,
    decisions: Vec<Seq>,
    cores: CoreSet,
}

impl Scan {
    fn of(events: &[Event]) -> Self {
        let mut scan = Self::default();
        for event in events {
            match &event.payload {
                Payload::SessionCarried(_) => scan.carried.push((event.seq, event.msg.clone())),
                Payload::Mce(mce) if mce.between_trials => {
                    scan.between.push((event.seq, event.msg.clone()));
                }
                Payload::HuntStart(start) => {
                    let parked = if let Some(seq) = start.parked_seq {
                        Parked::CycleEnd(seq)
                    } else if start.parked.iter().any(|(_, offset)| offset.get() != 0) {
                        Parked::Held(start.cores)
                    } else {
                        Parked::Zero
                    };
                    scan.hunt_start
                        .insert(start.hunt.get(), (event.seq, parked));
                }
                Payload::Combination(combination) => {
                    scan.combination_seq
                        .insert(combination.combination.get(), event.seq);
                }
                Payload::DefectFound(found) if found.direction == DefectDirection::TooCautious => {
                    scan.defects.push(Defect {
                        id: found.id.get(),
                        title: found.title.clone(),
                        pr: found.pr,
                        decisions: found.decisions.clone(),
                        cores: found.cores,
                    });
                }
                Payload::CommandReset(reset) => match reset.target {
                    ResetTarget::All => scan.defects.clear(),
                    ResetTarget::Core(core) => {
                        for defect in &mut scan.defects {
                            defect.cores.remove(core);
                        }
                    }
                },
                _ => {}
            }
        }
        scan
    }
}

fn render(events: &[Event]) -> Result<String, Problem> {
    let model = togi_watch::model::project(events);
    if let Some(problem) = model.problem() {
        return Err(Problem::Replay(problem.to_string()));
    }
    let journal = JournalState::replay(events).map_err(|e| Problem::Replay(e.to_string()))?;
    let tuner = togi_tuner::replay(events).map_err(|e| Problem::Replay(e.to_string()))?;
    let projection = tuner.project();
    if journal.session.is_none() {
        return Err(Problem::NoSession);
    }
    let scan = Scan::of(events);
    let mut out = String::new();
    Status {
        out: &mut out,
        journal: &journal,
        projection: &projection,
        snap: model.snapshot(),
        scan: &scan,
    }
    .write();
    Ok(out)
}

struct Status<'a> {
    out: &'a mut String,
    journal: &'a JournalState,
    projection: &'a Projection,
    snap: Snapshot<'a>,
    scan: &'a Scan,
}

fn esc(text: &str) -> String {
    escape_text(text).to_string()
}

fn cores_of(cores: impl IntoIterator<Item = CoreId>) -> String {
    let ids: Vec<String> = cores
        .into_iter()
        .map(|core| format!("{:02}", core.get()))
        .collect();
    if ids.is_empty() {
        "-".to_owned()
    } else {
        ids.join(" ")
    }
}

/// `count` rounds with the noun in agreement.
fn rounds(count: u32) -> String {
    format!("{count} {}", noun(count as usize, "round"))
}

/// What a cycle of `steps` lacks of the coverage a full cycle needs, one entry per regime.
fn missing_coverage(steps: &[Regime]) -> Vec<String> {
    FULL_CYCLE
        .iter()
        .filter_map(|&(regime, least)| {
            let got = steps.iter().filter(|&&step| step == regime).count();
            (got < least).then(|| format!("{regime}: {} more steps", least - got))
        })
        .collect()
}

impl Status<'_> {
    fn line(&mut self, first: &str, rest: &str, text: &str) {
        wrap(self.out, first, rest, text);
    }

    fn write(&mut self) {
        let header = self.header();
        self.line("", "  ", &header);
        let Some(session) = &self.journal.session else {
            return;
        };
        let _ = writeln!(
            self.out,
            "session {} started {}",
            esc(&session.id.to_string()),
            session.start
        );
        if let Some(context) = &session.bios_context {
            let text = format!(
                "BIOS {} on {}, {}, microcode {}, boost limit {} MHz",
                esc(&context.bios_version),
                esc(&context.board),
                esc(&context.cpu_model),
                esc(&context.microcode),
                context.boost_limit_mhz
            );
            self.line("", "  ", &text);
        }
        match self.journal.in_flight() {
            Some(flight) => {
                let text = format!("in flight: [#{}] {}", flight.seq, esc(&flight.msg));
                self.line("", "  ", &text);
            }
            None => self.out.push_str("in flight: none\n"),
        }
        if let Some(dead_end) = self.journal.dead_end {
            let text = format!(
                "dead end: {} [#{}]",
                esc(dead_end.condition.as_str()),
                dead_end.seq
            );
            self.line("", "  ", &text);
        }
        for (seq, msg) in &self.scan.carried {
            let text = format!("carried: [#{seq}] {}", esc(msg));
            self.line("", "  ", &text);
        }
        self.out.push('\n');
        self.cores();
        self.bios_note();
        self.combinations();
        self.r7();
        for (seq, msg) in &self.scan.between {
            let text = format!("between-trial evidence [#{seq}]: {}", esc(msg));
            self.line("\n", "  ", &text);
        }
        self.hunt();
        self.phase2();
        self.defects();
        self.evidence();
    }

    fn header(&self) -> String {
        let searching = self
            .projection
            .cores
            .iter()
            .filter(|core| core.phase == Some(CorePhase::Search))
            .count();
        let plan = &self.projection.cycle_plan;
        let activity = if searching > 0 {
            format!("search: {searching} cores left")
        } else if let Some(hunt) = &self.projection.hunt_plan {
            let group = hunt.groups.last().map_or(0, |group| group.number);
            format!("hunt {}, group {group}", hunt.number)
        } else if let Some(round) = &self.projection.deepening {
            let done = round
                .checks
                .iter()
                .filter(|check| check.passed >= check.needed)
                .count();
            format!(
                "phase 2 round {}, checks {done}/{}",
                round.round,
                round.checks.len()
            )
        } else if let Some(number) = plan.number.filter(|_| !plan.steps.is_empty()) {
            let done = plan.steps.iter().filter(|step| step.done).count();
            let mut text = format!("checking cycle {number}, steps {done}/{}", plan.steps.len());
            let steps: Vec<_> = plan.steps.iter().map(|step| step.regime).collect();
            let missing = missing_coverage(&steps);
            if !missing.is_empty() {
                text.push_str(", not full: ");
                text.push_str(&missing.join("; "));
            }
            text
        } else {
            "checking not started".to_owned()
        };
        match self.phase_clause() {
            Some(clause) => format!("{activity} | {clause}"),
            None => activity,
        }
    }

    fn phase_clause(&self) -> Option<String> {
        let plan = self.projection.phase_plan.as_ref()?;
        Some(match plan.phase {
            PhaseNo::One => "phase 1".to_owned(),
            PhaseNo::Concluded => format!(
                "phase 2 concluded: clean cycles {}, latest clean cycle {}",
                self.projection.clean_cycles,
                self.projection
                    .last_clean_cycle
                    .map_or(0, std::num::NonZero::get)
            ),
            PhaseNo::Two if plan.confirming => "phase 2: confirmation cycle".to_owned(),
            PhaseNo::Two => format!(
                "phase 2: at most {} left, then one full cycle",
                rounds(plan.rounds_left)
            ),
        })
    }

    fn bios_cell(&self, core: CoreId) -> String {
        let Some(bios) = self.snap.bios else {
            return "-".to_owned();
        };
        let Some(offset) = bios.shown.get(core) else {
            return "-".to_owned();
        };
        if bios.unconfirmed.iter().any(|mark| mark.core == core) {
            format!("{}*", offset.get())
        } else {
            offset.get().to_string()
        }
    }

    fn cores(&mut self) {
        let mut rows = vec![Row::new(
            [
                "CORE", "CCD", "SLOT", "OFFSET", "BIOS", "PHASE", "FAILURE", "QUEUED",
            ]
            .map(String::from),
        )];
        for view in &self.projection.cores {
            let facts = self.journal.core(view.core);
            let model = self.snap.cores.iter().find(|core| core.id == view.core);
            let mut row = Row::new([
                format!("{:02}", view.core.get()),
                facts.map_or("-".to_owned(), |facts| facts.ccd.to_string()),
                (view.core.get() % 8).to_string(),
                view.offset.get().to_string(),
                self.bios_cell(view.core),
                view.phase
                    .map_or("-".to_owned(), |phase| esc(phase_word(phase))),
                view.failure_point
                    .map_or("-".to_owned(), |point| point.get().to_string()),
                model
                    .and_then(|core| core.queued)
                    .map_or("-".to_owned(), |_| "reset".to_owned()),
            ]);
            let combos: Vec<String> = self
                .snap
                .combos
                .iter()
                .filter(|combo| combo.members.iter().any(|member| member.core == view.core))
                .map(|combo| format!("C{}", combo.id.get()))
                .collect();
            if !combos.is_empty() {
                row = row.note("combinations", combos.join(", "));
            }
            if let Some(decision) = facts.and_then(|facts| facts.last_decision.as_ref()) {
                row = row.note(
                    "last decision",
                    format!("[#{}] {}", decision.seq, esc(&decision.msg)),
                );
            }
            rows.push(row);
        }
        table(self.out, &rows);
    }

    fn bios_note(&mut self) {
        let phase_one = self
            .projection
            .phase_plan
            .as_ref()
            .is_some_and(|plan| plan.phase == PhaseNo::One);
        let profile = self.projection.bios_profile.as_ref();
        let text = match profile.and_then(|bios| bios.confirmed.map(|confirmed| (bios, confirmed)))
        {
            None if phase_one => {
                "BIOS profile: none confirmed yet; phase 1 has not passed a full cycle".to_owned()
            }
            None => return,
            Some((bios, confirmed)) if bios.unconfirmed.is_empty() => format!(
                "BIOS profile confirmed by the passed full cycle [#{}]",
                confirmed.seq
            ),
            Some((bios, confirmed)) => format!(
                "* unconfirmed since [#{}]: cores {} show a stepped-back offset, the profile confirmed by the passed full cycle [#{}] otherwise; a passed full cycle confirms it, except a held failure's stepped-back offset, which stays marked until the hold ends",
                bios.since().map_or(0, Seq::get),
                cores_of(bios.unconfirmed.iter().map(|mark| mark.core)),
                confirmed.seq
            ),
        };
        self.line("", "  ", &text);
    }

    fn combinations(&mut self) {
        if self.snap.combos.is_empty() {
            return;
        }
        self.out.push_str("\ncombinations:\n");
        for combo in self.snap.combos {
            let members: Vec<String> = combo
                .members
                .iter()
                .map(|member| format!("core {:02} {}", member.core.get(), member.offset.get()))
                .collect();
            let source = if combo.fallback {
                format!("fallback over every candidate of hunt {}", combo.hunt)
            } else {
                format!("observed in hunt {}", combo.hunt)
            };
            let seq = self
                .scan
                .combination_seq
                .get(&combo.id.get())
                .map_or(0, |seq| seq.get());
            let text = format!(
                "C{} {}, {source} [#{seq}]",
                combo.id.get(),
                members.join(" + ")
            );
            self.line("  ", "      ", &text);
        }
    }

    fn r7(&mut self) {
        let mut status: Vec<_> = self.snap.r7.to_vec();
        if status.is_empty() {
            return;
        }
        status.sort_by_key(|entry| (entry.workload, entry.core));
        self.line(
            "\n",
            "  ",
            "R7 self-sufficiency (passed as top requester at equal or deeper offsets; not a guarantee)",
        );
        let mut rows = vec![Row::new(
            ["CORE", "CCD", "SELF-SUFFICIENT", "TOP REQUESTER"].map(String::from),
        )];
        for entry in status {
            let own = if entry.self_sufficient {
                format!("observed ({} passing trials)", entry.passes)
            } else {
                "not yet demonstrated".to_owned()
            };
            let mut top = if entry.top_requester { "yes" } else { "no" }.to_owned();
            if entry.offset_proxy {
                top.push_str(" (offset fallback)");
            }
            rows.push(
                Row::new([
                    format!("{:02}", entry.core.get()),
                    entry.ccd.to_string(),
                    own,
                    top,
                ])
                .note("workload", esc(entry.workload.as_str())),
            );
        }
        table(self.out, &rows);
    }

    fn hunt(&mut self) {
        let Some(hunt) = self.snap.hunt else {
            return;
        };
        let plan = hunt.plan;
        let (start_seq, parked) = self
            .scan
            .hunt_start
            .get(&plan.number)
            .cloned()
            .unwrap_or((plan.failure, Parked::Zero));
        let parked = match parked {
            Parked::Zero => "parked offsets all-zero".to_owned(),
            Parked::CycleEnd(seq) => format!("parked offsets cycle end #{seq}"),
            Parked::Held(loaded) => format!(
                "loaded cores {} held at their failing offsets, the rest parked at 0",
                cores_of(loaded)
            ),
        };
        let signal = hunt
            .facts
            .cause
            .signal
            .map_or("failure", |signal| signal.as_str());
        let trial = plan.trial.as_ref().map_or("-", |trial| trial.as_str());
        let text = format!(
            "hunt {} [#{start_seq}]: unattributed {} in {} trial {}; {parked}",
            plan.number,
            esc(signal),
            esc(plan.regime.as_str()),
            esc(trial),
        );
        self.line("\n", "  ", &text);
        self.line(
            "  ",
            "    ",
            &format!("candidates {}", cores_of(plan.candidates)),
        );
        let mut rows = vec![Row::new(
            ["GROUP", "CORES", "OUTCOME", "PASS EVIDENCE", "NEED"].map(String::from),
        )];
        for group in &plan.groups {
            let mut cores = cores_of(group.cores);
            let mut note = None;
            if let Some(probe) = group.probe {
                let held: Vec<String> = group
                    .held
                    .iter()
                    .map(|member| format!("{:02} at {}", member.core.get(), member.offset.get()))
                    .collect();
                "probe".clone_into(&mut cores);
                note = Some((
                    "probe",
                    format!(
                        "{:02} at {} with {}",
                        probe.core.get(),
                        probe.offset.get(),
                        held.join(", ")
                    ),
                ));
            } else if cores.len() > GROUP_CORES_MAX {
                let listed = std::mem::replace(&mut cores, format!("{} cores", group.cores.len()));
                note = Some(("cores", listed));
            }
            let mut row = Row::new([
                format!("G{}", group.number),
                cores,
                group_word(group.outcome).to_owned(),
                group.passed.to_string(),
                group.needed.to_string(),
            ]);
            if let Some((label, text)) = note {
                row = row.note(label, text);
            }
            rows.push(row);
        }
        table(self.out, &rows);
    }

    fn phase2(&mut self) {
        let plan = self.projection.phase_plan.as_ref();
        let round = self.projection.deepening.as_ref();
        if let Some(round) = round {
            let text = format!("phase 2 round {} [#{}]", round.round, round.seq);
            self.line("\n", "  ", &text);
        }
        if let Some(plan) = plan.filter(|plan| plan.phase == PhaseNo::Two) {
            if !plan.candidates.is_empty() {
                let mut rows = vec![Row::new(
                    ["CANDIDATE", "OFFSET", "SOLO", "GAP", "STATE"].map(String::from),
                )];
                for candidate in &plan.candidates {
                    let state = if !candidate.moving && !candidate.carried {
                        "waits"
                    } else {
                        candidate_word(candidate.moving)
                    };
                    rows.push(Row::new([
                        format!("{:02}", candidate.core.get()),
                        candidate.offset.get().to_string(),
                        candidate
                            .solo_limit
                            .map_or("-".to_owned(), |limit| limit.get().to_string()),
                        candidate.gap.to_string(),
                        state.to_owned(),
                    ]));
                }
                self.out.push('\n');
                table(self.out, &rows);
            }
            let text = if plan.confirming && round.is_none() {
                "no core can move: the confirmation cycle is all that is left".to_owned()
            } else {
                format!(
                    "worst case if no round fails: {} left, then one full cycle",
                    rounds(plan.rounds_left)
                )
            };
            self.line("\n", "  ", &text);
        }
        if let Some(round) = round {
            let mut rows = vec![Row::new(["CHECK", "CORES", "PASSES"].map(String::from))];
            for check in &round.checks {
                rows.push(Row::new([
                    format!(
                        "{} {}",
                        esc(check.regime.as_str()),
                        esc(check.workload.as_str())
                    ),
                    cores_of(check.cores),
                    format!("{}/{}", check.passed, check.needed),
                ]));
            }
            table(self.out, &rows);
        }
    }

    fn defects(&mut self) {
        for defect in &self.scan.defects {
            if defect.cores.is_empty() {
                continue;
            }
            let decisions: Vec<String> = defect
                .decisions
                .iter()
                .map(|seq| format!("#{seq}"))
                .collect();
            let text = format!(
                "defect {}: {} (fixed by pull request #{}); decisions {} affected cores {}",
                defect.id,
                esc(&defect.title),
                defect.pr,
                decisions.join(" "),
                cores_of(defect.cores)
            );
            wrap(self.out, "\n", "  ", &text);
            for core in defect.cores {
                let _ = writeln!(self.out, "  togi reset --core {}", core.get());
            }
        }
    }

    fn evidence(&mut self) {
        let checking = &self.projection.checking;
        if checking.profile.is_none() {
            return;
        }
        self.out.push('\n');
        let mut rows = vec![Row::new(["REGIME", "WORKLOAD", "TRIALS"].map(String::from))];
        for (workload, trials) in &checking.exposure {
            rows.push(Row::new([
                esc(workload.regime().as_str()),
                esc(workload.as_str()),
                trials.to_string(),
            ]));
        }
        table(self.out, &rows);
        if let Some(peak) = checking.peak {
            let _ = writeln!(
                self.out,
                "Tctl max since last profile change {}°C [#{}]",
                peak.celsius, peak.seq
            );
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod corpus;
