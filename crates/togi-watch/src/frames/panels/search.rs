//! The search turns, the BIOS profile and the open phase 2 round.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::num::NonZeroU8;

use ratatui::text::{Line, Span};
use togi_journal::Seq;
use togi_machine::{CcdId, CoreSet, TargetOffset};
use togi_tuner::TurnKind;

use super::super::colours::Paint;
use super::super::dash::note_line;
use super::super::fit::{cut_styled, plain, rule, wrap_styled};
use super::super::layout::SizeClass;
use super::super::measure::Tables;
use super::super::scene::{Scene, regime_list};
use super::table::{cell, stage_marker, table_row, wall_date};
use crate::words::{candidate_word, core_ids, noun, plural};

type Shape = (TargetOffset, Option<TargetOffset>, u32, bool);

/// The search's turns, one core per turn, with the solo limits already found under them.
pub(super) fn turn_lines(
    scene: &Scene<'_>,
    t: &Tables,
    width: usize,
    class: SizeClass,
) -> Vec<Line<'static>> {
    let mut out = vec![
        rule(
            width,
            &cell(scene, Paint::Grey, "TURNS"),
            &cell(scene, Paint::Grey, "one core per turn, in this order"),
            scene.style(Paint::Track),
        ),
        Line::default(),
    ];
    if class != SizeClass::Compact {
        out.push(table_row(
            &t.turns,
            &[
                Line::default(),
                cell(scene, Paint::Grey, "core"),
                cell(scene, Paint::Grey, "this turn"),
                cell(scene, Paint::Grey, "at"),
                cell(scene, Paint::Grey, "workload"),
                cell(scene, Paint::Grey, "so far"),
            ],
        ));
    }
    for turn in scene.s.turns {
        let (mut marker, mut style, mut faint) = (Line::default(), Paint::Text, Paint::Grey);
        if turn.running && scene.trial_runs() {
            marker = Line::from(stage_marker(scene, "►"));
            (style, faint) = (Paint::White, Paint::Text);
        }
        let mut what = if turn.kind
            == (TurnKind::Search {
                step: NonZeroU8::MIN,
            }) {
            "search: 1 count deeper".to_owned()
        } else {
            format!("search: {}", regime_list(&turn.regimes))
        };
        let mut so_far = "none yet".to_owned();
        if let Some(core) = scene.core(turn.core) {
            if let Some(n) = core.confirm.filter(|_| turn.kind == TurnKind::Confirm) {
                what = format!(
                    "confirm: light trial {} of {}",
                    (n.light + 1).min(n.needed),
                    n.needed
                );
                if n.light >= n.needed && n.heavy >= n.needed {
                    "records the solo limit".clone_into(&mut what);
                } else if n.light >= n.needed {
                    what = format!("confirm: heavy trial {} of {}", n.heavy + 1, n.needed);
                }
                so_far = format!(
                    "light {}/{}, heavy {}/{}",
                    n.light, n.needed, n.heavy, n.needed
                );
            } else {
                let note = plain(&note_line(scene, core, t.turns[5].w));
                if !note.is_empty() {
                    so_far = note;
                }
            }
        }
        out.push(table_row(
            &t.turns,
            &[
                marker,
                cell(scene, style, format!("{:02}", turn.core.get())),
                cell(scene, style, what),
                cell(scene, style, turn.offset.get().to_string()),
                cell(scene, Paint::Grey, turn.workload.workload().label),
                cell(scene, faint, so_far),
            ],
        ));
    }
    let found: CoreSet = scene
        .s
        .cores
        .iter()
        .filter(|core| core.solo.is_some())
        .map(|core| core.id)
        .collect();
    if !found.is_empty() {
        out.push(Line::default());
        out.push(cut_styled(
            &Line::from(vec![
                Span::raw("  "),
                scene.paint(Paint::Grey, "waiting for the cycles  "),
                scene.paint(Paint::Text, core_ids(found)),
                scene.paint(Paint::Grey, "  · solo limits found"),
            ]),
            width,
        ));
    }
    #[cfg(test)]
    crate::corpus::lines("panel.turns", &format!("{class:?}"), &out);
    out
}

const NO_PROFILE: &str =
    "No BIOS profile yet: it appears once the tuner has a phase for every core.";
const NOTHING_CONFIRMED: &str =
    "none confirmed yet: phase 1 has not passed a full cycle, so nothing here is confirmed";
const BIOS_NOTE: &str = "unconfirmed: a passed full cycle confirms it, unless a failure is held";
const BIOS_NOTE_SHORT: &str = "a passed full cycle confirms, unless a failure is held";

/// The BIOS profile page: the profile to enter, one row per CCD in slot order, with a star on every
/// unconfirmed offset, and which cycle confirmed the rest and when, with the journal event as evidence.
pub(super) fn bios_page(scene: &Scene<'_>, width: usize) -> Vec<Line<'static>> {
    let track = scene.style(Paint::Track);
    if scene.s.phases.is_none() {
        return wrap_styled(NO_PROFILE, width, scene.style(Paint::Grey));
    }
    let Some(bios) = scene.s.bios else {
        return vec![
            rule(
                width,
                &cell(scene, Paint::Grey, "BIOS PROFILE"),
                &Line::default(),
                track,
            ),
            Line::default(),
        ]
        .into_iter()
        .chain(wrap_styled(
            NOTHING_CONFIRMED,
            width,
            scene.style(Paint::Grey),
        ))
        .collect();
    };
    let mut right = Line::default();
    if !bios.unconfirmed.is_empty() {
        right = cell(
            scene,
            Paint::Hunt,
            format!(
                "UNCONFIRMED · cores {}",
                core_ids(bios.unconfirmed.iter().map(|mark| mark.core))
            ),
        );
    }
    let mut out = vec![
        rule(
            width,
            &cell(scene, Paint::Grey, "BIOS PROFILE"),
            &right,
            track,
        ),
        Line::default(),
    ];
    let grey = scene.style(Paint::Grey);
    if let Some(confirmed) = bios.confirmed {
        let text = match scene.s.bios_confirmation {
            Some(done) => format!(
                "confirmed by the passed full cycle {} that ended {} {}",
                done.cycle,
                wall_date(scene.zone, done.at),
                scene.wall_minute(done.at)
            ),
            None => "confirmed by a passed full cycle".to_owned(),
        };
        out.extend(wrap_styled(&text, width, grey));
        out.extend(wrap_styled(
            &format!("evidence: journal event #{}", confirmed.seq.get()),
            width,
            grey,
        ));
        out.push(Line::default());
    }
    let mut cells: BTreeMap<CcdId, Vec<Span<'static>>> = BTreeMap::new();
    let mut slots: BTreeMap<CcdId, Vec<u8>> = BTreeMap::new();
    for core in scene.s.cores {
        let Some(offset) = bios.shown.get(core.id) else {
            continue;
        };
        let held = bios.unconfirmed.iter().any(|mark| mark.core == core.id);
        let span = if held {
            scene.paint(Paint::Hunt, format!("{:4}*", offset.get()))
        } else {
            scene.paint(Paint::Text, format!("{:4} ", offset.get()))
        };
        cells.entry(core.ccd).or_default().push(span);
        slots.entry(core.ccd).or_default().push(core.id.get() % 8);
    }
    if let Some(first) = slots.values().next() {
        let header = first.iter().fold(String::new(), |mut header, slot| {
            let _ = write!(header, "{slot:4} ");
            header
        });
        out.push(Line::from(vec![
            scene.paint(Paint::Grey, "slot   "),
            scene.paint(Paint::Grey, header),
        ]));
    }
    for (ccd, cells) in cells {
        let mut spans = vec![scene.paint(Paint::Grey, format!("CCD {}  ", ccd.0))];
        spans.extend(cells);
        out.push(Line::from(spans));
    }
    if !bios.unconfirmed.is_empty() {
        out.push(Line::default());
        // The star and the heading already say unconfirmed, so a page too narrow for the whole note drops that word.
        let note = if width >= 2 + BIOS_NOTE.len() {
            BIOS_NOTE
        } else {
            BIOS_NOTE_SHORT
        };
        out.push(cut_styled(
            &Line::from(vec![
                scene.paint(Paint::Hunt, "* "),
                scene.paint(Paint::Grey, note),
            ]),
            width,
        ));
        let since = bios.since().map_or(0, Seq::get);
        out.extend(wrap_styled(
            &format!("unconfirmed since journal event #{since}"),
            width,
            grey,
        ));
    }
    #[cfg(test)]
    crate::corpus::lines("page.bios", "", &out);
    out
}

/// The open round: its candidates with their gaps, the bound on what is left, and its checks.
pub(super) fn phase2_lines(scene: &Scene<'_>, width: usize) -> Vec<Line<'static>> {
    let Some(phases) = scene.s.phases else {
        return Vec::new();
    };
    let round = phases.round.unwrap_or(0);
    let mut out = vec![
        rule(
            width,
            &cell(scene, Paint::Grey, format!("PHASE 2 · ROUND {round}")),
            &Line::default(),
            scene.style(Paint::Track),
        ),
        Line::default(),
        cut_styled(
            &cell(
                scene,
                Paint::Grey,
                format!(
                    "{} left if none fails, then one full cycle",
                    plural(phases.rounds_left as usize, "round")
                ),
            ),
            width,
        ),
    ];
    let mut shapes: Vec<(Shape, CoreSet)> = Vec::new();
    for candidate in &phases.candidates {
        let shape = (
            candidate.offset,
            candidate.solo_limit,
            candidate.gap,
            candidate.moving,
        );
        match shapes.iter_mut().find(|(known, _)| *known == shape) {
            Some((_, cores)) => {
                cores.insert(candidate.core);
            }
            None => shapes.push((shape, std::iter::once(candidate.core).collect())),
        }
    }
    for ((offset, solo, gap, moving), cores) in shapes {
        let note = format!("  {}", candidate_word(moving));
        let solo = solo.map_or_else(|| "-".to_owned(), |solo| solo.get().to_string());
        out.push(cut_styled(
            &Line::from(vec![
                scene.paint(
                    Paint::Text,
                    format!(
                        "{} {}  {} → solo {solo}  gap {gap}",
                        noun(cores.len(), "core"),
                        core_ids(cores),
                        offset.get()
                    ),
                ),
                scene.paint(Paint::Grey, note),
            ]),
            width,
        ));
    }
    out.push(Line::default());
    out.push(cell(scene, Paint::Grey, "CHECKS"));
    for check in scene.s.deepening.iter().flat_map(|round| &round.checks) {
        out.push(cut_styled(
            &Line::from(vec![
                scene.paint(
                    Paint::Text,
                    format!(
                        "{} on {}  {}/{} passed",
                        check.regime,
                        core_ids(check.cores.iter()),
                        check.passed,
                        check.needed
                    ),
                ),
                Span::raw("  "),
                scene.paint(Paint::Grey, check.workload.workload().label),
            ]),
            width,
        ));
    }
    #[cfg(test)]
    crate::corpus::lines("panel.phase2", "", &out);
    out
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU8;

    use jiff::Timestamp;
    use jiff::tz::TimeZone;
    use togi_machine::Regime;
    use togi_tuner::{
        BiosProfile, Candidate, DeepeningCheck, DeepeningView, PhasePlan, Profile, SearchTurn,
        TurnKind,
    };

    use super::super::super::layout::SizeClass;
    use super::super::super::measure::Tables;
    use super::super::fixtures::{bios, core_view, phases, scene, texts, topology};
    use super::{bios_page, phase2_lines, turn_lines};
    use crate::model::{BiosConfirmation, Confirm, CoreState, Model};
    use crate::tests::kit::{core, nz, off, seq, set};

    fn confirm_turn(light: u32, heavy: u32) -> Vec<String> {
        let mut view = core_view(11, 1);
        view.state = CoreState::Confirm;
        view.confirm = Some(Confirm {
            offset: off(-7),
            light,
            heavy,
            needed: 5,
        });
        let cores = [view];
        let turns = [SearchTurn {
            core: core(11),
            kind: TurnKind::Confirm,
            regimes: vec![Regime::R1],
            offset: off(-7),
            workload: togi_machine::workloads(Regime::R1)[0].id(),
            light,
            heavy,
            needed: 5,
            running: false,
        }];
        let model = Model::new();
        let mut s = model.snapshot();
        s.cores = &cores;
        s.turns = &turns;
        let zone = TimeZone::UTC;
        let scene = scene(&s, &zone);
        let mut all = Vec::new();
        for class in [SizeClass::Wide, SizeClass::Medium] {
            all.extend(texts(&turn_lines(
                &scene,
                &Tables::new(class, 80),
                80,
                class,
            )));
        }
        all
    }

    #[test]
    fn a_complete_confirmation_turn_records_the_solo_limit() {
        for (name, light, heavy, want) in [
            ("first light", 0, 0, "confirm: light trial 1 of 5"),
            ("last light", 4, 0, "confirm: light trial 5 of 5"),
            ("first heavy", 5, 0, "confirm: heavy trial 1 of 5"),
            ("last heavy", 5, 4, "confirm: heavy trial 5 of 5"),
            ("complete", 5, 5, "records the solo limit"),
        ] {
            let text = confirm_turn(light, heavy);
            assert_eq!(
                text.iter().filter(|row| row.contains(want)).count(),
                2,
                "{name}: both classes say {want:?}:\n{}",
                text.join("\n")
            );
        }
        let text = confirm_turn(2, 0).join("\n");
        assert!(text.contains("light 2/5, heavy 0/5"), "{text}");
    }

    /// The 160x45 turns panel is 75 cells wide; its `this turn` column is the one the descriptions must fit.
    #[test]
    fn the_medium_turn_descriptions_fit_their_column_whole() {
        let t = Tables::new(SizeClass::Medium, 75);
        let column = t.turns[2].w;
        let turn = |step: u8, regimes: Vec<Regime>| SearchTurn {
            core: core(2),
            kind: TurnKind::Search {
                step: NonZeroU8::new(step).expect("a search step"),
            },
            regimes,
            offset: off(-35),
            workload: togi_machine::workloads(Regime::R1)[0].id(),
            light: 0,
            heavy: 0,
            needed: 5,
            running: false,
        };
        let turns = [
            turn(2, vec![Regime::R1, Regime::R2]),
            turn(1, vec![Regime::R1]),
        ];
        let model = Model::new();
        let mut s = model.snapshot();
        s.turns = &turns;
        let zone = TimeZone::UTC;
        let scene = scene(&s, &zone);
        let rows = texts(&turn_lines(&scene, &t, 75, SizeClass::Medium));
        let pair = rows
            .iter()
            .find(|row| row.contains("search: light, then heavy"))
            .unwrap_or_else(|| panic!("{rows:?}"));
        let deeper = rows
            .iter()
            .find(|row| row.contains("search: 1 count deeper"))
            .unwrap_or_else(|| panic!("{rows:?}"));
        assert!(!pair.contains("...") && !deeper.contains("..."), "{rows:?}");
        for words in [
            "search: light, then heavy",
            "search: 1 count deeper",
            "confirm: light trial 5 of 5",
            "confirm: heavy trial 5 of 5",
            "records the solo limit",
        ] {
            assert!(
                super::super::super::fit::display_width(words) <= column,
                "{words} in {column}"
            );
        }
        for (light, heavy, want) in [
            (0, 0, "confirm: light trial 1 of 5"),
            (5, 4, "confirm: heavy trial 5 of 5"),
        ] {
            let rows = confirm_turn(light, heavy);
            assert!(
                rows.iter()
                    .any(|row| row.contains(want) && !row.contains("...")),
                "{rows:?}"
            );
        }
    }

    #[test]
    fn the_turns_say_which_cores_already_have_their_solo_limit() {
        let mut running = core_view(11, 1);
        running.state = CoreState::Confirm;
        let mut found = core_view(3, 0);
        found.solo = Some(off(-20));
        let mut other = core_view(5, 0);
        other.solo = Some(off(-18));
        let turn = |core_id: u8| SearchTurn {
            core: core(core_id),
            kind: TurnKind::Search {
                step: NonZeroU8::MIN,
            },
            regimes: vec![Regime::R1],
            offset: off(-7),
            workload: togi_machine::workloads(Regime::R1)[0].id(),
            light: 0,
            heavy: 0,
            needed: 5,
            running: false,
        };
        let turns = [turn(11)];
        let model = Model::new();
        let zone = TimeZone::UTC;
        let rows = |cores: &[crate::model::CoreView]| {
            let mut s = model.snapshot();
            s.cores = cores;
            s.turns = &turns;
            let scene = scene(&s, &zone);
            texts(&turn_lines(
                &scene,
                &Tables::new(SizeClass::Wide, 80),
                80,
                SizeClass::Wide,
            ))
        };
        let before = rows(std::slice::from_ref(&running));
        assert!(
            before
                .iter()
                .all(|row| !row.contains("waiting for the cycles")),
            "{before:?}"
        );
        let after = rows(&[running, found, other]);
        let n = after.len();
        assert_eq!(after[n - 2], "", "{after:?}");
        assert_eq!(
            after[n - 1],
            "  waiting for the cycles  03 05  · solo limits found"
        );
    }

    fn bios_rows(
        bios: Option<&BiosProfile>,
        phases: Option<&PhasePlan>,
        confirmation: Option<BiosConfirmation>,
        width: usize,
    ) -> Vec<String> {
        let cores = [core_view(0, 0), core_view(1, 0), core_view(8, 1)];
        let model = Model::new();
        let mut s = model.snapshot();
        s.cores = &cores;
        s.bios = bios;
        s.phases = phases;
        s.bios_confirmation = confirmation;
        let zone = TimeZone::fixed(jiff::tz::offset(2));
        let scene = scene(&s, &zone);
        texts(&bios_page(&scene, width))
    }

    fn confirmation() -> BiosConfirmation {
        BiosConfirmation {
            cycle: 3,
            at: Timestamp::from_second(1_767_311_998).expect("a timestamp"),
            seq: seq(12),
        }
    }

    #[test]
    fn the_bios_profile_stars_what_a_cycle_has_not_confirmed() {
        let plan = phases(None);
        let held = bios(true);
        let rows = bios_rows(Some(&held), Some(&plan), Some(confirmation()), 80);
        assert!(rows[0].starts_with("BIOS PROFILE"));
        assert!(rows[0].ends_with("UNCONFIRMED · cores 01"), "{}", rows[0]);
        let slot = rows
            .iter()
            .position(|row| row.starts_with("slot"))
            .expect("slots");
        assert_eq!(rows[slot].trim_end(), "slot      0    1");
        assert_eq!(rows[slot + 1].trim_end(), "CCD 0   -20  -25*");
        assert_eq!(rows[slot + 2].trim_end(), "CCD 1   -10");
        assert!(
            rows.contains(
                &"* unconfirmed: a passed full cycle confirms it, unless a failure is held"
                    .to_owned()
            ),
            "{rows:?}"
        );
        assert!(
            rows.contains(&"unconfirmed since journal event #15".to_owned()),
            "{rows:?}"
        );
    }

    #[test]
    fn a_confirmation_is_described_by_its_cycle_and_local_date_with_the_event_as_evidence() {
        let plan = phases(None);
        let confirmed = bios(false);
        let rows = bios_rows(Some(&confirmed), Some(&plan), Some(confirmation()), 100);
        assert_eq!(
            rows[2],
            "confirmed by the passed full cycle 3 that ended 2026-01-02 01:59"
        );
        assert_eq!(rows[3], "evidence: journal event #12");
        assert!(rows.iter().all(|row| !row.contains("ending #")), "{rows:?}");
        let unknown = bios_rows(Some(&confirmed), Some(&plan), None, 100);
        assert_eq!(unknown[2], "confirmed by a passed full cycle");
        assert_eq!(unknown[3], "evidence: journal event #12");
    }

    #[test]
    fn before_a_confirmed_profile_the_bios_page_says_none_is_confirmed() {
        let plan = phases(None);
        let rows = bios_rows(None, Some(&plan), None, 100);
        assert!(rows[0].starts_with("BIOS PROFILE"));
        assert_eq!(
            rows[2],
            "none confirmed yet: phase 1 has not passed a full cycle, so nothing here is confirmed"
        );
        let held = bios(false);
        let rows = bios_rows(Some(&held), None, None, 100);
        assert_eq!(
            rows,
            ["No BIOS profile yet: it appears once the tuner has a phase for every core."]
        );
    }

    #[test]
    fn the_round_lists_its_candidates_by_shape_and_its_checks() {
        let topology = topology();
        let profile = Profile::new(
            &topology,
            [0u8, 1, 8].iter().map(|&id| (core(id), off(-30))).collect(),
        )
        .expect("a profile");
        let candidate = |id: u8, moving: bool| Candidate {
            core: core(id),
            offset: off(-30),
            solo_limit: Some(off(-25)),
            gap: 5,
            carried: false,
            moving,
        };
        let mut plan = phases(Some(3));
        plan.candidates = vec![candidate(0, false), candidate(1, false), candidate(8, true)];
        let round = DeepeningView {
            round: nz(3),
            seq: seq(20),
            target: profile,
            profile,
            cores: set(&[0, 1, 8]),
            checks: vec![DeepeningCheck {
                regime: Regime::R1,
                workload: togi_machine::workloads(Regime::R1)[0].id(),
                cores: set(&[0, 1]),
                condition: togi_machine::Condition::Alone,
                profile,
                needed: 5,
                passed: 2,
                failed: 0,
                running: true,
            }],
        };
        let model = Model::new();
        let mut s = model.snapshot();
        s.phases = Some(&plan);
        s.deepening = Some(&round);
        let zone = TimeZone::UTC;
        let scene = scene(&s, &zone);
        let rows = texts(&phase2_lines(&scene, 100));
        assert!(rows[0].starts_with("PHASE 2 · ROUND 3"));
        assert_eq!(rows[2], "2 rounds left if none fails, then one full cycle");
        assert_eq!(
            rows[3],
            "cores 00 01  -30 → solo -25  gap 5  checked again in place"
        );
        assert_eq!(rows[4], "core 08  -30 → solo -25  gap 5  moving");
        assert_eq!(rows[5], "");
        assert_eq!(rows[6], "CHECKS");
        assert!(
            rows[7].starts_with("R1 on 00 01  2/5 passed  "),
            "{}",
            rows[7]
        );
    }
}
