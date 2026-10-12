//! The outcome lines: what each way the trial in flight can end leads to, from the tuner's forecast.
//!
//! The forecast is conditional and these lines say so: each row is a premise ("if it passes", "if another core is
//! named") and what the tuner would decide and run next if that happened. Branches that say the same merge.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::time::Duration;

use ratatui::text::{Line, Span};
use togi_machine::{Condition, CoreId, CoreSet, WorkloadId};
use togi_tuner::{
    Branch, DecisionPayload, Load, Premise, TrialContext, TrialPlan, Uncertainty, WorkloadChoice,
};

use super::super::colours::Paint;
use super::super::fit::{console_text, cut_styled, cut_words, display_width, line_width};
use super::super::scene::{Scene, short_duration};
use super::super::terms::part_name;
use super::shape_words;
use crate::model::{HuntShape, TrialView};
use crate::words::{
    carried_groups, core_ids, decision_brief, decision_detail, decision_phrase, hunt_end_phrase,
    kind_words, solo_limit, workload_display,
};

/// How many outcome lines the trial band holds.
pub(super) const ROW_LIMIT: usize = 3;

/// One step of an outcome line; a minor one is left out first when the line does not fit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Phrase {
    pub(super) text: String,
    pub(super) minor: bool,
}

impl Phrase {
    fn new(text: impl Into<String>, minor: bool) -> Self {
        Self {
            text: text.into(),
            minor,
        }
    }
}

/// One outcome line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OutcomeRow {
    pub(super) label: String,
    pub(super) short: String,
    pub(super) compact: String,
    /// `compact` with the folded cores at 0 counted instead of spelled out, for a band too narrow for both.
    pub(super) brief: String,
    pub(super) phrases: Vec<Phrase>,
    pub(super) paint: Paint,
    /// Where backoffs take top requesters from, when new telemetry could change them.
    pub(super) basis: Option<&'static str>,
}

/// The outcome lines of the trial in flight in `width` cells: one per branch of the forecast, or the one line of
/// a compact screen. Empty when no trial is in flight.
pub(crate) fn outcome_lines(scene: &Scene<'_>, width: usize, compact: bool) -> Vec<Line<'static>> {
    let s = &scene.s;
    if s.trial.is_none()
        || s.stopped.is_some()
        || s.dead_end.is_some()
        || s.recover.is_some()
        || s.crashed_trial()
    {
        return Vec::new();
    }
    let rows = outcome_rows(scene);
    if compact {
        return compact_line(scene, &rows, width)
            .map(|line| cut_styled(&line, width))
            .into_iter()
            .collect();
    }
    rows.iter()
        .map(|row| {
            let label = scene.paint(row.paint, format!("{:<21}", row.label));
            let room = width.saturating_sub(21);
            let text = cut_words(&fit_phrases(&row.phrases, room), room);
            Line::from(vec![label, scene.paint(Paint::Text, text)])
        })
        .collect()
}

fn compact_line(scene: &Scene<'_>, rows: &[OutcomeRow], width: usize) -> Option<Line<'static>> {
    if rows.is_empty() {
        return None;
    }
    let basis = rows.iter().find_map(|row| row.basis);
    let parts = |level: u8, mark: bool| -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("   "));
            }
            let text = match level {
                0 => row.compact.clone(),
                1 => row.brief.clone(),
                _ => tight_next(&row.brief).into_owned(),
            };
            spans.push(scene.paint(row.paint, row.short.clone()));
            spans.push(Span::raw(" "));
            spans.push(scene.paint(Paint::Text, text));
            if mark && row.basis.is_some() {
                spans.push(scene.paint(Paint::Text, "?"));
            }
        }
        spans
    };
    let mut level = 0;
    let mut line = Line::from(parts(0, false));
    while level < 2 && line_width(&line) > width {
        level += 1;
        line = Line::from(parts(level, false));
    }
    if let Some(basis) = basis {
        let room = width.cast_signed() - line_width(&line).cast_signed() - 3;
        let say = |text: String, line: &mut Line<'static>| {
            line.spans.push(Span::raw("   "));
            line.spans.push(scene.paint(Paint::Grey, text));
        };
        if room >= ("backoffs ".len() + basis.len()).cast_signed() {
            say(format!("backoffs {basis}"), &mut line);
        } else if room >= basis.len().cast_signed() {
            say(basis.to_owned(), &mut line);
        } else {
            line = Line::from(parts(level, true));
        }
    }
    Some(line)
}

/// An outcome line's phrases joined with arrows, leaving out minor ones, then any but the first and the last,
/// until it fits.
pub(super) fn fit_phrases(phrases: &[Phrase], width: usize) -> String {
    let join = |phrases: &[Phrase]| {
        phrases
            .iter()
            .map(|phrase| console_text(&phrase.text))
            .collect::<Vec<_>>()
            .join(" → ")
    };
    let mut text = join(phrases);
    if display_width(&text) <= width {
        return text;
    }
    let mut phrases = phrases.to_vec();
    while display_width(&text) > width {
        let drop = phrases
            .iter()
            .position(|phrase| phrase.minor && phrase.text != "...")
            .or_else(|| (1..phrases.len().saturating_sub(1)).find(|&i| phrases[i].text != "..."));
        let Some(drop) = drop else {
            return text;
        };
        if drop > 0 && phrases[drop - 1].text == "..."
            || phrases.get(drop + 1).is_some_and(|next| next.text == "...")
        {
            phrases.remove(drop);
        } else {
            phrases[drop] = Phrase::new("...", true);
        }
        text = join(&phrases);
    }
    text
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    AllPass,
    Pass,
    Named,
    Unnamed,
    Inconclusive,
}

fn kind(premise: Premise) -> Kind {
    match premise {
        Premise::Pass => Kind::Pass,
        Premise::AllPass => Kind::AllPass,
        Premise::Named(_) => Kind::Named,
        Premise::Unnamed => Kind::Unnamed,
        Premise::Inconclusive => Kind::Inconclusive,
    }
}

struct Group {
    kinds: Vec<Kind>,
    phrases: Vec<Phrase>,
    text: String,
    compact: String,
    base: String,
    folded: usize,
    basis: Option<&'static str>,
    passes: u32,
    zero: Vec<String>,
    stand_in: bool,
}

impl Group {
    fn zero_only(&self) -> bool {
        !self.zero.is_empty()
            && !self.stand_in
            && !self.phrases.is_empty()
            && self.kinds.iter().all(|&kind| kind == Kind::Named)
    }
}

/// The outcome lines: each branch's decisions in words, branches that say the same merged into one.
pub(super) fn outcome_rows(scene: &Scene<'_>) -> Vec<OutcomeRow> {
    let trial = scene.s.trial;
    let mut groups: Vec<Group> = Vec::new();
    for branch in &scene.s.forecast.branches {
        let (phrases, compact, basis) = outcome_words(scene, branch);
        let text = fit_phrases(&phrases, usize::MAX >> 1);
        let at = groups
            .iter()
            .position(|group| group.text == text)
            .unwrap_or_else(|| {
                groups.push(Group {
                    kinds: Vec::new(),
                    phrases,
                    text,
                    base: compact.clone(),
                    compact,
                    folded: 0,
                    basis,
                    passes: 0,
                    zero: Vec::new(),
                    stand_in: false,
                });
                groups.len() - 1
            });
        let kind = kind(branch.premise);
        groups[at].kinds.push(kind);
        groups[at].passes = groups[at].passes.max(branch.passes);
        if kind == Kind::Named {
            if !branch.at_zero {
                groups[at].stand_in = true;
            } else {
                let who = zero_words(scene, branch);
                if !groups[at].zero.contains(&who) {
                    groups[at].zero.push(who);
                }
            }
        }
    }
    if groups
        .iter()
        .any(|group| group.kinds.contains(&Kind::AllPass))
    {
        groups.retain(|group| group.kinds != [Kind::Pass]);
    }
    let mut visible = groups.len();
    if groups
        .iter()
        .any(|group| group.kinds == [Kind::Inconclusive])
    {
        visible -= 1;
    }
    let mut stand_in = groups.iter().position(|group| group.stand_in);
    while let Some(at) =
        stand_in.filter(|&at| !groups[at].phrases.is_empty() && visible > ROW_LIMIT)
    {
        let Some(zero) = groups.iter().position(Group::zero_only) else {
            break;
        };
        let z = groups.remove(zero);
        let at = if zero < at { at - 1 } else { at };
        let exception = format!(
            "{}:{}",
            z.zero.join(" or "),
            z.compact.strip_prefix('→').unwrap_or(&z.compact)
        );
        let group = &mut groups[at];
        if let Some(last) = group.phrases.last_mut() {
            let _ = write!(last.text, " · {exception}");
        }
        let _ = write!(group.compact, " · {exception}");
        group.folded += 1;
        visible -= 1;
        stand_in = Some(at);
    }
    let named_groups = groups
        .iter()
        .filter(|group| group.kinds.contains(&Kind::Named))
        .count();
    let hunting = scene.s.hunt.is_some();
    let order = if hunting {
        [
            Kind::AllPass,
            Kind::Pass,
            Kind::Unnamed,
            Kind::Named,
            Kind::Inconclusive,
        ]
    } else {
        [
            Kind::AllPass,
            Kind::Pass,
            Kind::Named,
            Kind::Unnamed,
            Kind::Inconclusive,
        ]
    };
    let rank = |group: &Group| {
        group
            .kinds
            .iter()
            .filter_map(|kind| order.iter().position(|ranked| ranked == kind))
            .min()
            .unwrap_or(order.len())
    };
    groups.sort_by_key(rank);
    let mut rows = Vec::new();
    for group in groups {
        let mut who = if named_groups > 1 {
            "another core"
        } else {
            "a core"
        }
        .to_owned();
        let zero = !group.zero.is_empty() && !group.stand_in;
        if zero && named_groups > 1 {
            who = group.zero.join(" or ");
        }
        let (mut label, mut short, paint) = premise_words(&group.kinds, group.passes, trial, &who);
        if zero
            && group.kinds.contains(&Kind::Unnamed)
            && !group.kinds.contains(&Kind::Pass)
            && !group.kinds.contains(&Kind::AllPass)
        {
            who = group.zero.join(" or ");
            label = format!("if none or {who} is named");
            short = format!("none or {} named", who.strip_prefix("a ").unwrap_or(&who));
        }
        let brief = if group.folded > 0 {
            format!("{} · +{} at 0", group.base, group.folded)
        } else {
            group.compact.clone()
        };
        rows.push(OutcomeRow {
            label,
            short,
            compact: group.compact,
            brief,
            phrases: group.phrases,
            paint,
            basis: group.basis,
        });
        if rows.len() == ROW_LIMIT {
            break;
        }
    }
    rows
}

/// What a named branch at 0 assumes is named: a top requester at 0, which ends tuning, or a core at 0 whose
/// failure moves its CCD's top group, naming the CCD when cores at 0 on other CCDs move theirs.
pub(super) fn zero_words(scene: &Scene<'_>, branch: &Branch) -> String {
    if branch.top_requester {
        return "a top requester at 0".to_owned();
    }
    let ccd = |branch: &Branch| match branch.premise {
        Premise::Named(core) => scene.core(core).map(|core| i64::from(core.ccd.0)),
        _ => None,
    };
    let mine = ccd(branch);
    let apart = scene.s.forecast.branches.iter().any(|other| {
        matches!(other.premise, Premise::Named(_))
            && other.at_zero
            && !other.top_requester
            && ccd(other) != mine
    });
    match mine.filter(|_| apart) {
        Some(ccd) => format!("a CCD {ccd} core at 0"),
        None => "a core at 0".to_owned(),
    }
}

/// Labels an outcome line; `who` names the core a named failure is about.
fn premise_words(
    kinds: &[Kind],
    passes: u32,
    trial: Option<&TrialView>,
    who: &str,
) -> (String, String, Paint) {
    let has = |kind| kinds.contains(&kind);
    let fails = has(Kind::Named) || has(Kind::Unnamed);
    let pass = has(Kind::Pass) || has(Kind::AllPass);
    if pass && fails {
        return (
            "pass or fail".to_owned(),
            "pass or fail".to_owned(),
            Paint::Text,
        );
    }
    if has(Kind::AllPass) {
        let (passed, of) = trial
            .filter(|trial| trial.count.of > 0)
            .map_or((passes, passes), |trial| {
                (trial.count.passed + passes, trial.count.of)
            });
        return (
            format!("if {passed} of {of} pass"),
            format!("{passed}/{of} pass"),
            Paint::Good,
        );
    }
    if has(Kind::Pass) {
        return ("if it passes".to_owned(), "pass".to_owned(), Paint::Good);
    }
    if has(Kind::Named) && has(Kind::Unnamed) {
        return ("if it fails".to_owned(), "fail".to_owned(), Paint::Bad);
    }
    if has(Kind::Named) {
        let name = who
            .strip_prefix("a ")
            .unwrap_or(who)
            .replacen("another", "other", 1);
        return (
            format!("if {who} is named"),
            format!("{name} named"),
            Paint::Bad,
        );
    }
    if has(Kind::Unnamed) {
        return (
            "if none is named".to_owned(),
            "none named".to_owned(),
            Paint::Bad,
        );
    }
    (
        "if inconclusive".to_owned(),
        "inconclusive".to_owned(),
        Paint::Grey,
    )
}

/// Where a forecast's decisions take top requesters from when its trial ends without telemetry: earlier
/// requests, or offset order where none were measured.
pub(super) fn request_basis(branch: &Branch) -> &'static str {
    if branch.offset_order {
        "per offset order"
    } else {
        "per earlier requests"
    }
}

/// Puts a branch's decisions in words, in the order the tuner records them, then the trial it runs next.
fn outcome_words(
    scene: &Scene<'_>,
    branch: &Branch,
) -> (Vec<Phrase>, String, Option<&'static str>) {
    let trial = scene.s.trial;
    let named = matches!(branch.premise, Premise::Named(_));
    let mut phrases = decision_phrases(scene, &branch.decisions, named);
    let mut basis = None;
    if branch.without_telemetry {
        if let Some(last) = phrases.last_mut() {
            let text = request_basis(branch);
            last.text.push(' ');
            last.text.push_str(text);
            basis = Some(text);
        }
    }
    let mut shape = None;
    let mut step_done = false;
    for decision in &branch.decisions {
        match decision {
            DecisionPayload::HuntStart(start) => {
                shape = Some(HuntShape {
                    failing: start.failing,
                    parked: start.parked,
                });
            }
            DecisionPayload::CheckingStep(_) => step_done = true,
            _ => {}
        }
    }
    let passing = matches!(branch.premise, Premise::Pass | Premise::AllPass);
    if let Some(trial) = trial.filter(|_| passing && !step_done) {
        let place = trial.cycle;
        let next_step = branch.next.as_ref().and_then(cycle_of);
        let moves_on = match (place.and_then(|place| place.step), next_step) {
            (Some(step), Some((cycle, next))) => {
                (Some(cycle) == place.map(|place| place.cycle) && next > step).then_some(step)
            }
            _ => None,
        };
        if let Some(step) = moves_on {
            phrases.push(Phrase::new(format!("step {step} is done"), false));
        } else if let Some(place) = place.filter(|place| place.parts > 1) {
            if trial.count.of > 0 && trial.count.passed + branch.passes >= trial.count.of {
                phrases.push(Phrase::new(
                    format!("part {} is done", place.part.unwrap_or_default()),
                    false,
                ));
            }
        }
    }
    let mut compact = String::new();
    for decision in &branch.decisions {
        if matches!(decision, DecisionPayload::DeadEnd(_)) || compact.is_empty() {
            if let Some(words) = decision_brief(decision) {
                compact = words;
            }
        }
    }
    let next = next_words(scene, branch, trial, shape.as_ref(), &phrases, &mut compact);
    if compact.is_empty() {
        compact = compact_next(&next);
    }
    if !next.is_empty() {
        phrases.push(Phrase::new(next, false));
    }
    (phrases, format!("→ {compact}"), basis)
}

fn next_words(
    scene: &Scene<'_>,
    branch: &Branch,
    trial: Option<&TrialView>,
    shape: Option<&HuntShape>,
    phrases: &[Phrase],
    compact: &mut String,
) -> String {
    match branch.uncertainty {
        Some(Uncertainty::NeedsRanking) => {
            return "read the core ranking before deciding".to_owned();
        }
        Some(Uncertainty::NeedsHistory) => {
            return "complete hunt history needed before deciding".to_owned();
        }
        Some(Uncertainty::RecurringPlan) => {
            return "the hunt would repeat this plan; nothing further is projected".to_owned();
        }
        Some(Uncertainty::Stopped { .. }) | None => {}
    }
    let Some(next) = &branch.next else {
        return if phrases.is_empty() {
            "no next trial projected".to_owned()
        } else {
            String::new()
        };
    };
    let again = trial.filter(|trial| same_trial(next, trial));
    if let Some(trial) = again {
        if next.retry {
            "same trial again".clone_into(compact);
            return "the same trial runs again".to_owned();
        }
        if branch.premise == Premise::Pass && trial.count.index < trial.count.of {
            return format!(
                "next: trial {} of {}",
                trial.count.index + 1,
                trial.count.of
            );
        }
        return "next: this load again".to_owned();
    }
    if let Some(trial) = trial.filter(|trial| same_part(next, trial)) {
        let passing = matches!(branch.premise, Premise::Pass | Premise::AllPass);
        let index = trial.count.index + branch.passes.max(1);
        if passing && index <= trial.count.of {
            return format!(
                "next: trial {index} of {} · {}",
                trial.count.of,
                short_duration(duration_of(next))
            );
        }
    }
    format!("next: {}", next_trial_words(scene, next, trial, shape))
}

fn duration_of(plan: &TrialPlan) -> Duration {
    Duration::from_secs(u64::from(plan.duration.get()))
}

fn cycle_of(plan: &TrialPlan) -> Option<(u32, u32)> {
    match plan.context {
        TrialContext::Cycle { cycle, step, .. } => Some((cycle.get(), step.get().get())),
        _ => None,
    }
}

fn hunt_of(plan: &TrialPlan) -> Option<(u32, u32)> {
    match plan.context {
        TrialContext::Hunt { hunt, group } => Some((hunt.get(), group.get())),
        _ => None,
    }
}

fn round_of(plan: &TrialPlan) -> Option<u32> {
    match plan.context {
        TrialContext::Round { round } => Some(round.get()),
        _ => None,
    }
}

fn is_rerun(plan: &TrialPlan) -> bool {
    matches!(
        plan.context,
        TrialContext::Rerun { .. } | TrialContext::ZeroRerun { .. }
    )
}

fn single_of(plan: &TrialPlan) -> Option<CoreId> {
    match plan.load {
        Load::Single(core) => Some(core),
        Load::Many(_) => None,
    }
}

fn workload_of(plan: &TrialPlan) -> Option<WorkloadId> {
    match plan.workload {
        WorkloadChoice::Frozen(id) => Some(id),
        WorkloadChoice::Rotate => None,
    }
}

/// Whether `next` is another trial of the same requirement as `trial`.
fn same_trial(next: &TrialPlan, trial: &TrialView) -> bool {
    let place = trial.cycle;
    let same = next.regime == trial.regime
        && workload_of(next).is_none_or(|id| id == trial.workload)
        && duration_of(next) == trial.duration
        && hunt_of(next) == trial.hunt.map(|hunt| (hunt.hunt, hunt.group))
        && cycle_of(next) == place.map(|place| (place.cycle, place.step.unwrap_or_default()))
        && round_of(next) == trial.round
        && is_rerun(next) == trial.rerun
        && next.condition == trial.condition;
    match single_of(next) {
        Some(core) => {
            same && trial.condition == Condition::Alone
                && trial.core == Some(core)
                && trial.offset == next.profile.get(core)
        }
        None => same && next.load.cores() == trial.cores,
    }
}

/// Whether `next` is a later trial of the checking part `trial` belongs to, perhaps of another length.
fn same_part(next: &TrialPlan, trial: &TrialView) -> bool {
    let place = trial.cycle;
    match (cycle_of(next), place) {
        (Some((cycle, step)), Some(place)) => {
            hunt_of(next).is_none()
                && !is_rerun(next)
                && cycle == place.cycle
                && Some(step) == place.step
                && next.load.cores() == trial.cores
        }
        _ => false,
    }
}

/// Describes a trial the tuner runs next the way the trial band describes the trial in flight. A branch that
/// starts a hunt passes that hunt's shape.
pub(super) fn next_trial_words(
    scene: &Scene<'_>,
    next: &TrialPlan,
    after: Option<&TrialView>,
    shape: Option<&HuntShape>,
) -> String {
    let kind = format!("{} {}", next.regime, kind_words(next.regime));
    let what = match single_of(next) {
        Some(core) => {
            let offset = next
                .profile
                .get(core)
                .map_or(0, togi_machine::TargetOffset::get);
            let alone = if next.condition == Condition::Alone {
                " alone"
            } else {
                ""
            };
            format!("{kind} on core {:02}{alone} at {offset}", core.get())
        }
        None => format!("{kind} on {}", core_ids(next.load.cores())),
    };
    let duration = short_duration(duration_of(next));
    if let Some((hunt, group)) = hunt_of(next) {
        let shape = shape.or_else(|| scene.s.shapes.get(&hunt));
        let layout = shape.map_or_else(String::new, |shape| {
            shape_words(scene, next.load.cores(), &next.profile.offsets(), shape)
        });
        if scene
            .s
            .hunt
            .is_some_and(|active| active.plan.number == hunt)
        {
            if let Some(part) = shape.and_then(|shape| plan_part(scene, next, shape)) {
                return format!("part {part}: {layout}");
            }
        }
        let mut text = format!("hunt {hunt} group {group}: {what}");
        if !layout.is_empty() {
            text.push_str(" · ");
            text.push_str(&layout);
        }
        return text;
    }
    if is_rerun(next) {
        return if next.condition == Condition::Parked {
            format!("rerun {what} with every core at 0 · {duration}")
        } else {
            format!("rerun {what} · {duration}")
        };
    }
    if let Some((cycle, step)) = cycle_of(next) {
        let same_place = after.is_some_and(|trial| {
            trial
                .cycle
                .is_some_and(|place| place.cycle == cycle && place.step == Some(step))
        });
        if same_place {
            if let Some(name) = part_words(scene, next) {
                return name;
            }
        }
        let mut text = format!("step {step}: {kind}");
        if same_place && single_of(next).is_none() && !next.load.cores().is_empty() {
            let _ = write!(text, " on {}", core_ids(next.load.cores()));
        }
        if let Some(workload) = workload_of(next) {
            let _ = write!(text, " with {}", workload_display(workload));
        }
        return text;
    }
    if let Some(round) = round_of(next) {
        return format!("deepening round {round}: {what}");
    }
    what
}

/// Which part of the running hunt's split a trial loads, counting from 1.
fn plan_part(scene: &Scene<'_>, next: &TrialPlan, shape: &HuntShape) -> Option<usize> {
    let hunt = scene.s.hunt?;
    hunt.plan
        .parts
        .iter()
        .position(|part| {
            scene.s.order.iter().all(|&id| {
                let (Some(at), Some(fails), Some(parks)) = (
                    next.profile.get(id),
                    shape.failing.get(id),
                    shape.parked.get(id),
                ) else {
                    return true;
                };
                fails == parks || (at == fails) == part.failing.contains(id)
            })
        })
        .map(|index| index + 1)
}

/// The part of the current checking step a trial loads: `part 2: full CCD 0 on 00-07, 5m`, or
/// `part 4: core 09 at -26, 2m` for a step that loads one core at a time.
fn part_words(scene: &Scene<'_>, next: &TrialPlan) -> Option<String> {
    let (index, name) = matching_part(scene, next)?;
    let duration = short_duration(duration_of(next));
    Some(match single_of(next) {
        Some(core) => {
            let offset = next
                .profile
                .get(core)
                .map_or(0, togi_machine::TargetOffset::get);
            format!(
                "part {index}: core {:02} at {offset}, {duration}",
                core.get()
            )
        }
        None => format!(
            "part {index}: {name} on {}, {duration}",
            core_ids(next.load.cores())
        ),
    })
}

/// The 1-based index and name of the part of `next`'s step that loads exactly its cores.
fn matching_part(scene: &Scene<'_>, next: &TrialPlan) -> Option<(usize, String)> {
    let (_, step) = cycle_of(next)?;
    let cycle = scene.s.cycle?;
    let step = cycle
        .plan
        .steps
        .get(usize::try_from(step).ok()?.checked_sub(1)?)?;
    let want = next.load.cores();
    let index = step.parts.iter().position(|part| part.cores == want)?;
    Some((index + 1, part_name(step.regime, &step.parts[index])))
}

/// Shortens what comes next to its position: `part 2: 08-15`, `part 4: core 09`, `group 8`, `step 3`.
pub(super) fn compact_next(next: &str) -> String {
    let text = next.strip_prefix("next: ").unwrap_or(next);
    if let Some((name, rest)) = text.split_once(": ") {
        if name.starts_with("hunt ") {
            let group = name.split_once(" group ").map_or("", |(_, group)| group);
            return format!("group {group}");
        }
        if name.starts_with("part ") {
            if let Some(core) = rest.strip_prefix("core ") {
                let id = core.split(' ').next().unwrap_or_default();
                return format!("{name}: core {id}");
            }
            if rest.contains(" on ") {
                return name.to_owned();
            }
            let cores = rest.split(' ').next().unwrap_or_default();
            return format!("{name}: {cores}");
        }
        return name.to_owned();
    }
    if text.starts_with("rerun ") {
        if text.contains(" with every core at 0") {
            return "rerun at CO 0".to_owned();
        }
        return "rerun".to_owned();
    }
    text.to_owned()
}

/// Shortens a compact outcome that names the next trial to its regime and the cores it loads, leaving what
/// carries no core as it is.
pub(super) fn tight_next(text: &str) -> Cow<'_, str> {
    let Some(rest) = text.strip_prefix("→ R") else {
        return Cow::Borrowed(text);
    };
    let Some(digit) = rest.chars().next().filter(char::is_ascii_digit) else {
        return Cow::Borrowed(text);
    };
    let Some(words) = rest[1..].strip_prefix(' ') else {
        return Cow::Borrowed(text);
    };
    let mut best = None;
    let mut at = 0;
    while let Some(found) = words[at..].find("on ") {
        let start = at + found;
        at = start + 1;
        let before = &words[..start];
        let word_boundary = start > 0
            && before.ends_with(' ')
            && !before.starts_with(' ')
            && !before.contains("  ");
        if !word_boundary {
            continue;
        }
        let target = &words[start + 3..];
        let end = if let Some(core) = target.strip_prefix("core ") {
            let digits = core.chars().take_while(char::is_ascii_digit).count();
            (digits > 0).then(|| "core ".len() + digits)
        } else if target.starts_with(|c: char| c.is_ascii_digit()) {
            Some(
                1 + target[1..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == ' ' || *c == '-')
                    .count(),
            )
        } else {
            None
        };
        if let Some(end) = end {
            best = Some(&target[..end]);
        }
    }
    match best {
        Some(target) => Cow::Owned(format!("→ R{digit} on {}", target.trim_end())),
        None => Cow::Borrowed(text),
    }
}

/// Puts each recorded decision in a short phrase. Consecutive hunt groups answered by carried trials become one
/// phrase. A branch that assumes a named core speaks of "it", since any loaded core could be the one.
pub(super) fn decision_phrases(
    scene: &Scene<'_>,
    decisions: &[DecisionPayload],
    named: bool,
) -> Vec<Phrase> {
    let mut out: Vec<Phrase> = Vec::new();
    let mut push = |text: String, minor: bool| {
        if !text.is_empty() && out.last().is_none_or(|last| last.text != text) {
            out.push(Phrase::new(text, minor));
        }
    };
    let mut named_core = None;
    let mut failed = None;
    let mut i = 0;
    while i < decisions.len() {
        let rest = &decisions[i + 1..];
        match &decisions[i] {
            DecisionPayload::Failure(failure) => {
                if let Some(seq) = failure.known_failure {
                    push(known_failure_words(scene, failure, seq, rest), false);
                } else if let (true, Some(core)) = (named, failure.core) {
                    named_core = Some(core);
                    if !moves_another_core(rest, core) {
                        push("its failure point is recorded".to_owned(), false);
                    }
                } else if let Some(core) = failure.core {
                    failed = Some(core);
                }
            }
            decision @ DecisionPayload::HuntStart(_) => {
                push(decision_detail(decision).unwrap_or_default(), true);
            }
            DecisionPayload::HuntGroup(group) => {
                if group.skipped {
                    push(format!("group {} skipped", group.group), false);
                } else if let Some(inferred) = group.inferred {
                    let mut last = i;
                    while let Some(DecisionPayload::HuntGroup(next)) = decisions.get(last + 1) {
                        if next.inferred != group.inferred
                            || next.skipped
                            || next.hunt != group.hunt
                        {
                            break;
                        }
                        last += 1;
                    }
                    let end = match &decisions[last] {
                        DecisionPayload::HuntGroup(end) => end.group.get(),
                        _ => group.group.get(),
                    };
                    let (text, minor) = carried_groups(group.group.get(), end, inferred);
                    push(text, minor);
                    i = last;
                }
            }
            DecisionPayload::HuntEnd(end) => {
                let (text, minor) = hunt_end_phrase(end, named);
                push(text, minor);
            }
            decision @ DecisionPayload::Combination(_) => {
                push(decision_detail(decision).unwrap_or_default(), false);
            }
            DecisionPayload::TunerDecision(decision) => {
                if named
                    && named_core == Some(decision.core)
                    && decision.decision == togi_journal::Decision::Backoff
                {
                    push("it backs off".to_owned(), false);
                } else {
                    push(decision_phrase(decision, failed), false);
                }
            }
            DecisionPayload::CorePhase(phase) => {
                use togi_journal::CorePhase;
                if phase.from == Some(CorePhase::Search) && phase.to != CorePhase::Search {
                    if let Some(limit) = solo_limit(phase) {
                        push(
                            format!("core {:02} solo limit {limit}", phase.core.get()),
                            false,
                        );
                    }
                } else if !named && phase.to == CorePhase::Search {
                    push(
                        format!("core {:02} starts its search over", phase.core.get()),
                        false,
                    );
                }
            }
            DecisionPayload::CheckingStep(step) => {
                if step.step.get() > 1 {
                    push(format!("step {} is done", step.step.get() - 1), false);
                }
            }
            decision @ (DecisionPayload::CheckingChain(_)
            | DecisionPayload::DeepeningRound(_)
            | DecisionPayload::DeadEnd(_)
            | DecisionPayload::HuntSkipped(_)) => {
                push(decision_detail(decision).unwrap_or_default(), false);
            }
            decision @ DecisionPayload::CheckingCycle(cycle) => {
                let minor = cycle.event == togi_journal::CycleEvent::Start;
                push(decision_detail(decision).unwrap_or_default(), minor);
            }
            DecisionPayload::TunerWarning(_) | DecisionPayload::ProfileChange(_) => {}
        }
        i += 1;
    }
    out
}

/// Whether the first move after a named failure records its failure point on another core.
fn moves_another_core(after: &[DecisionPayload], named: CoreId) -> bool {
    after.iter().find_map(|decision| match decision {
        DecisionPayload::TunerDecision(mv) => Some(mv.failure_point.is_some() && mv.core != named),
        _ => None,
    }) == Some(true)
}

/// Says which trial the tuner skips because it already failed at these offsets.
fn known_failure_words(
    scene: &Scene<'_>,
    failure: &togi_journal::Failure,
    seq: togi_journal::Seq,
    after: &[DecisionPayload],
) -> String {
    let mut what = match failure.regime {
        Some(regime) => format!("{regime} {}", kind_words(regime)),
        None => "idle".to_owned(),
    };
    if let Some(start) = after.iter().find_map(|decision| match decision {
        DecisionPayload::HuntStart(start) => Some(start),
        _ => None,
    }) {
        what = format!("{what} on {}", core_ids(start.cores));
        if let Some(trial) = scene.s.trial.filter(|trial| !trial.rerun) {
            if let Some(name) = checking_part_name(scene, trial, start.cores) {
                what = name;
            }
        }
    }
    let verb = if failure.signal == togi_machine::Signal::Crash {
        "crashed"
    } else {
        "failed"
    };
    let mut text = format!("{what} at these offsets already {verb}");
    if scene.s.carried.contains(&seq) {
        text.push_str(" in a carried trial");
    }
    text
}

/// The name of the part of the trial's step that loads exactly `cores`.
fn checking_part_name(scene: &Scene<'_>, trial: &TrialView, cores: CoreSet) -> Option<String> {
    let step = trial.cycle?.step?;
    let cycle = scene.s.cycle?;
    let step = cycle
        .plan
        .steps
        .get(usize::try_from(step).ok()?.checked_sub(1)?)?;
    let part = step.parts.iter().find(|part| part.cores == cores)?;
    Some(part_name(step.regime, part))
}
