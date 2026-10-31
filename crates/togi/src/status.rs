//! `togi status`: the session at a glance, rendered from the journal.

use std::fmt::Write as _;
use std::sync::LazyLock;

use clap::Args as ClapArgs;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use togi_journal::{
    DayQuestions, DeadEndCondition, DefectDirection, Event, Payload, ResetTarget, Seq,
    State as JournalState,
};
use togi_machine::{BootId, CoreId, CoreSet};
use togi_render::escape_text;
use togi_tuner::policy::Projection;
use togi_watch::day;
use togi_watch::model::FoldError;

use crate::cli::{Context, examples};
use crate::exit::Exit;
use crate::read;
use crate::report::fail;
use crate::table::wrap;

mod climb;
#[cfg(test)]
mod climb_tests;
mod evidence;
mod policy;

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
/// The activity (the open night with its start, elapsed observed time and the task it runs, the
/// day, a stop or a dead end);
/// session and BIOS context; then one table row per tuning unit with its physical cores,
/// applied D, hard ceiling C, remaining D-C, hold hours and climbing/held/finished state.
/// Tagged U retains observed-failure or censored write-limit witnesses. Statistical S is
/// diagnostic and never authorizes a step. Day policy shows experimental hold + exact
/// night qualification, applied/read-back versus proposed and paused-saved profiles,
/// machine allowance and every classified event, finish/held reasons and current exact
/// profile daily evidence under the 95% fixed-rate model. A Night part shows elapsed
/// observed time, queue shares, mapping, outstanding qualification and reproductions.
/// Status shows policy facts; it computes no ceiling or eligibility itself.
/// A defect names its recorded decisions and affected cores. A too-cautious one says the session
/// continues; a too-aggressive one names the `togi reset --all` that starts fresh.
/// After daily observations exist, a Day section lists boots, checkpointed hours per read-back
/// profile, clean/unclean ends and the last unclean boot's reset reason.
/// Each unclean end of a day-observed boot becomes a question until answered: what togi saw, the
/// four answers and the `togi day answer` command that records each; an answered one shows the
/// current answer.
/// Read-only, rendered from the journal. A different ruleset warns before rendering; a different
/// schema is refused.
/// With --evidence, also show exact-profile daily observations beside stock, provenance and
/// unknown intervals, numeric cap/slope witnesses, qualification exposure, applied-step history
/// and comparable same-class night clocks and temperatures. No safety verdict or ETA is given.
///
/// An event of a kind this build does not know is skipped and an unknown field is ignored; stderr
/// says which, after the output.
#[derive(ClapArgs, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {
    /// Show exact-profile observations, cap witnesses and recorded night comparisons
    #[arg(long)]
    pub evidence: bool,
}

/// Runs `togi status`.
pub fn run(args: &Args, ctx: &mut Context<'_>) -> Exit {
    let recording = match read::open(ctx, "status") {
        Ok(recording) => recording,
        Err(exit) => return exit,
    };
    let events = recording.events();
    let model = match ctx.system.journal_fs() {
        Some(fs) => togi_watch::model::Model::read_in(&*fs, ctx.state_dir, events),
        None => togi_watch::model::Model::read(ctx.state_dir, events),
    };
    let mut text = match render_model(events, &model, &ctx.system.zone()) {
        Ok(text) => text,
        Err(Problem::NoSession) => {
            let message = format!("no session in {}", ctx.state_dir.display());
            return fail(ctx, "status", message);
        }
        Err(Problem::Replay(error)) => {
            return fail(ctx, "status", togi_render::error_chain(&error));
        }
    };
    if args.evidence {
        evidence::write(
            &mut text,
            events,
            model.snapshot().policy,
            model.tuner_state(),
        );
    }
    if let Err(error) = ctx
        .out
        .write_all(text.as_bytes())
        .and_then(|()| ctx.out.flush())
    {
        return fail(
            ctx,
            "status",
            format!("write output: {}", togi_render::error_chain(&error)),
        );
    }
    read::report(ctx, "status", &recording);
    Exit::Ok
}

enum Problem {
    NoSession,
    Replay(FoldError),
}

/// What the events say that no fold keeps: notes and defects.
#[derive(Default)]
struct Scan<'a> {
    between: Vec<(Seq, &'a str)>,
    defects: Vec<Defect<'a>>,
}

struct Defect<'a> {
    id: u32,
    title: &'a str,
    pr: u32,
    direction: DefectDirection,
    decisions: &'a [Seq],
    cores: CoreSet,
}

impl<'a> Scan<'a> {
    fn of(events: &'a [Event]) -> Self {
        let mut scan = Self::default();
        for event in events {
            match &event.payload {
                Payload::Mce(mce) if mce.between_trials => {
                    scan.between.push((event.seq, &event.msg));
                }
                Payload::DefectFound(found) => scan.defects.push(Defect {
                    id: found.id.get(),
                    title: &found.title,
                    pr: found.pr,
                    direction: found.direction,
                    decisions: &found.decisions,
                    cores: found.cores,
                }),
                Payload::CommandReset(reset) => match reset.target {
                    ResetTarget::All => scan.defects.clear(),
                    ResetTarget::Core(_) => {}
                },
                Payload::SessionCarried(_)
                | Payload::HuntStart(_)
                | Payload::Combination(_)
                | Payload::SessionStart(_)
                | Payload::DayBootStart(_)
                | Payload::DayCheckpoint(_)
                | Payload::DayOffsetsChanged(_)
                | Payload::DayCleanStop(_)
                | Payload::DayUncleanEnd(_)
                | Payload::DayAnswer(_)
                | Payload::DayRecord(_)
                | Payload::DayFault(_)
                | Payload::SessionImported(_)
                | Payload::ExposureObserved(_)
                | Payload::CcdEdge(_)
                | Payload::CcdCap(_)
                | Payload::NightPlan(_)
                | Payload::NightQualification(_)
                | Payload::NightReproduction(_)
                | Payload::DayStep(_)
                | Payload::DayApplied(_)
                | Payload::DayEvent(_)
                | Payload::DayVeto(_)
                | Payload::DayHold(_)
                | Payload::DayFreeze(_)
                | Payload::DayInterval(_)
                | Payload::DayPause(_)
                | Payload::StockAttested(_)
                | Payload::DayImported(_)
                | Payload::SessionContext(_)
                | Payload::SessionBaseline(_)
                | Payload::SessionNotice(_)
                | Payload::SessionWarning(_)
                | Payload::SessionArchived(_)
                | Payload::ConfigLoaded(_)
                | Payload::PreflightCheck(_)
                | Payload::SmuIntent(_)
                | Payload::SmuWrite(_)
                | Payload::SmuReadback(_)
                | Payload::SmuError(_)
                | Payload::ProfileApplied(_)
                | Payload::ProfileChange(_)
                | Payload::ProfileRestored(_)
                | Payload::TrialIntent(_)
                | Payload::TrialStart(_)
                | Payload::TrialProgress(_)
                | Payload::TrialSignal(_)
                | Payload::TrialSample(_)
                | Payload::TrialEnd(_)
                | Payload::TrialCarried(_)
                | Payload::Failure(_)
                | Payload::FailureCarried(_)
                | Payload::Mce(_)
                | Payload::CrashDetected(_)
                | Payload::TunerDecision(_)
                | Payload::CorePhase(_)
                | Payload::CheckingCycle(_)
                | Payload::CheckingStep(_)
                | Payload::CheckingChain(_)
                | Payload::HostRanking(_)
                | Payload::HuntGroup(_)
                | Payload::HuntEnd(_)
                | Payload::HuntSkipped(_)
                | Payload::DeepeningRound(_)
                | Payload::TunerWarning(_)
                | Payload::BackendRetry(_)
                | Payload::TaskStart(_)
                | Payload::DefectAnswered(_)
                | Payload::DeadEnd(_)
                | Payload::BootSavedEntry(_)
                | Payload::BootLeaveReason(_)
                | Payload::Shutdown(_)
                | Payload::JournalTorn(_)
                | Payload::UnitMapping(_)
                | Payload::UnitEdge(_)
                | Payload::ClimbDecision(_)
                | Payload::UnitStop(_)
                | Payload::CampaignStart(_)
                | Payload::CampaignCharge(_)
                | Payload::CampaignReclassified(_)
                | Payload::FirmwareRestoration(_)
                | Payload::SuccessorImported(_)
                | Payload::StateRebuilt(_) => {}
            }
        }
        scan
    }
}

fn render_model(
    events: &[Event],
    model: &togi_watch::model::Model,
    zone: &TimeZone,
) -> Result<String, Problem> {
    if let Some(problem) = model.problem() {
        return Err(Problem::Replay(problem.clone()));
    }
    let journal = model.journal_state();
    if journal.session.is_none() {
        return Err(Problem::NoSession);
    }
    let scan = Scan::of(events);
    let snapshot = model.snapshot();
    let projection = snapshot.policy;
    let closed = night_closed_at(events, projection);
    let mut out = String::new();
    Status {
        out: &mut out,
        journal,
        policy: projection,
        stopped: snapshot.stopped,
        scan: &scan,
        at: policy::Where { zone, closed },
    }
    .write();
    render_day(&mut out, events);
    render_questions(&mut out, events);
    if let Some(report) = crate::morning::Report::of(events) {
        out.push_str("\nMorning report\n");
        wrap(&mut out, "", "  ", &report.text);
    }
    Ok(out)
}

/// When the `day.boot_start` that closed the latest night was written, when the journal holds it.
fn night_closed_at(events: &[Event], projection: &Projection) -> Option<Timestamp> {
    let closed_by = projection
        .night
        .as_ref()
        .and_then(|night| night.closed_by)?;
    events
        .iter()
        .find(|event| event.seq == closed_by)
        .map(|event| event.time)
}

/// The line `togi status` opens with, which says what the tuner is doing; `None` when the journal has no
/// session yet. `togi night` shows it as what the night will do.
pub(crate) fn plan(
    events: &[Event],
    model: &togi_watch::model::Model,
    zone: &TimeZone,
) -> Result<Option<String>, FoldError> {
    if let Some(problem) = model.problem() {
        return Err(problem.clone());
    }
    let journal = model.journal_state();
    if journal.session.is_none() {
        return Ok(None);
    }
    let snapshot = model.snapshot();
    let projection = snapshot.policy;
    let scan = Scan::of(events);
    let mut out = String::new();
    let line = Status {
        out: &mut out,
        journal,
        policy: projection,
        stopped: snapshot.stopped,
        scan: &scan,
        at: policy::Where {
            zone,
            closed: night_closed_at(events, projection),
        },
    }
    .header();
    Ok(Some(line))
}

/// The unclean ends still waiting for an answer and the ones answered, in journal order. A pending question
/// shows what togi saw and the four choices with the command that records each; an answered one shows the
/// current answer and how to change it.
fn render_questions(out: &mut String, events: &[Event]) {
    let questions = DayQuestions::of(events);
    let mut boots: Vec<&BootId> = Vec::new();
    for event in events {
        if let Payload::DayUncleanEnd(end) = &event.payload {
            if !boots.contains(&&end.boot) {
                boots.push(&end.boot);
            }
        }
    }
    if boots.is_empty() {
        return;
    }
    let _ = writeln!(out, "\nDay questions");
    for boot in boots {
        let Some(end) = questions.get(boot) else {
            continue;
        };
        let name = esc(boot.as_str());
        if let Some(answer) = questions.answer(boot) {
            let text = format!(
                "boot {name}: answered \"{}\" (change it with: togi day answer {name} <frozen|graphics|power-off|unsure>)",
                day::choice_words(answer.answer)
            );
            wrap(out, "", "  ", &text);
            continue;
        }
        let text = format!("boot {name} ended without a clean stop. What happened?");
        wrap(out, "", "  ", &text);
        out.push_str("  togi saw:\n");
        for line in day::observed(end) {
            wrap(out, "    ", "      ", &esc(&line));
        }
        let reading = format!(
            "until you answer, togi reads this as {}",
            day::class_words(end.class())
        );
        wrap(out, "  ", "    ", &reading);
        for (number, kind) in day::ANSWERS.into_iter().enumerate() {
            let text = format!(
                "{}. {}: {}",
                number + 1,
                day::choice_words(kind),
                day::answer_command(boot, kind).replace(boot.as_str(), &name)
            );
            wrap(out, "  ", "     ", &text);
        }
    }
}

fn render_day(out: &mut String, events: &[Event]) {
    let ledger = crate::day::Ledger::of(events);
    let totals = togi_watch::day_totals::DayTotals::of(events);
    if ledger.boots.is_empty() && totals.is_empty() {
        return;
    }
    let clean = ledger.boots.values().filter(|boot| boot.clean).count();
    let unclean = ledger.boots.values().filter(|boot| boot.unclean).count();
    let _ = writeln!(out, "\nDay");
    let _ = writeln!(
        out,
        "boots observed: {}; clean ends: {clean}; unclean ends: {unclean}",
        ledger.boots.len()
    );
    let watchdog = match &ledger.watchdog {
        Some(state) => format!(
            "hardware watchdog at the last observer check: {} ({})",
            if state.is_armed() {
                "armed"
            } else {
                "not armed"
            },
            esc(state.detail())
        ),
        None => "hardware watchdog at the last observer check: not recorded".to_owned(),
    };
    wrap(out, "", "  ", &watchdog);
    out.push_str("Applied hours per read-back profile (changed intervals excluded):\n");
    for (profile, seconds) in &ledger.profiles {
        let offsets: Vec<String> = profile
            .iter()
            .map(|(core, offset)| format!("{core:02}:{offset}"))
            .collect();
        let text = format!(
            "{}: {}.{:03} h",
            offsets.join(" "),
            seconds / 3600,
            (seconds % 3600) * 1000 / 3600
        );
        wrap(out, "  ", "    ", &text);
    }
    render_totals(out, &totals);
    if let Some((boot, reason)) = ledger.last_unclean {
        let reason = reason.kind.map_or("unknown", |kind| kind.as_str());
        let text = format!(
            "last unclean end: {} (reset reason: {reason})",
            esc(boot.as_str())
        );
        wrap(out, "", "  ", &text);
    } else {
        out.push_str("last unclean end: none\n");
    }
}

/// The observed totals beside the CO 0 totals. They count what togi saw and what was imported, never a rate.
fn render_totals(out: &mut String, totals: &togi_watch::day_totals::DayTotals) {
    let unknown = togi_watch::day_totals::unknown_intervals(totals.unknown_intervals());
    let all = format!("all profiles: {} ({unknown})", totals.all().describe());
    wrap(out, "", "  ", &all);
    let stock = format!("stock (CO 0): {}", totals.stock().describe());
    wrap(out, "", "  ", &stock);
    out.push_str("totals are observations, not a bound\n");
}

struct Status<'a> {
    out: &'a mut String,
    journal: &'a JournalState,
    policy: &'a Projection,
    stopped: Option<&'a togi_watch::model::StopView>,
    scan: &'a Scan<'a>,
    at: policy::Where<'a>,
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

/// A dead end's condition in the words the status shows; the journal records its wire name.
const fn dead_end_label(condition: DeadEndCondition) -> &'static str {
    match condition {
        DeadEndCondition::FailureAtZero => "failure at offset 0",
        DeadEndCondition::Smu => "SMU error",
        DeadEndCondition::NoEvidence => "no conclusive evidence",
        DeadEndCondition::BootLoop => "boot loop",
        DeadEndCondition::Containment => "workload containment failed",
        DeadEndCondition::Preflight => "preflight failed",
        DeadEndCondition::Defect => "recorded decisions affected by a defect",
        DeadEndCondition::ThermalTrip => "thermal trip",
    }
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
                dead_end_label(dead_end.condition),
                dead_end.seq
            );
            self.line("", "  ", &text);
        }
        self.out.push('\n');
        policy::sections(self.out, self.policy, self.at);
        for (seq, msg) in &self.scan.between {
            let text = format!("between-trial evidence [#{seq}]: {}", esc(msg));
            self.line("\n", "  ", &text);
        }
        self.defects();
    }

    /// The header's activity: a dead end and a stop come before the night or the day.
    fn header(&self) -> String {
        match self.journal.dead_end {
            Some(_) => "dead end".to_owned(),
            None => policy::activity(self.policy, self.stopped, self.at),
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
            let (direction, advice) = match defect.direction {
                DefectDirection::TooCautious => ("too cautious", "  the session continues\n"),
                DefectDirection::TooAggressive => ("too aggressive", "  togi reset --all\n"),
            };
            let text = format!(
                "defect {}: {} ({direction}; fixed by pull request #{}); decisions {} affected cores {}",
                defect.id,
                esc(defect.title),
                defect.pr,
                decisions.join(" "),
                cores_of(defect.cores)
            );
            wrap(self.out, "\n", "  ", &text);
            self.out.push_str(advice);
        }
    }
}

#[cfg(test)]
mod tests;
