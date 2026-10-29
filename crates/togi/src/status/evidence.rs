//! Recorded observations, not a stability verdict. Eligibility and cap calculations stay in the tuner.

use std::collections::{BTreeMap, BTreeSet};

use togi_journal::{ApplyCause, Event, ExposureObserved, Payload, RateBound, Seq, Slope};
use togi_machine::{CcdProfile, ObservedOffset, PerCore};
use togi_tuner::policy::{CapReason, Projection, QualificationStage};

use super::policy::{class_word, hours};
use crate::table::wrap;

fn line(out: &mut String, text: &str) {
    wrap(out, "", "  ", text);
}

fn profile_word(profile: &CcdProfile) -> String {
    profile
        .iter()
        .map(|(ccd, offset)| format!("CCD {ccd} {}", offset.get()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn offsets(profile: &CcdProfile, policy: &Projection) -> PerCore<ObservedOffset> {
    let mut offsets = PerCore::default();
    for ccd in &policy.ccds {
        if let Some(offset) = profile.get(ccd.ccd) {
            for core in ccd.cores {
                offsets.set(core, offset.observed());
            }
        }
    }
    offsets
}

fn bound_word(bound: RateBound) -> String {
    match bound {
        RateBound::Finite { per_hour } => format!("{:.8}/h", per_hour.get()),
        RateBound::Unbounded {} => "unbounded".to_owned(),
    }
}

fn slope_word(slope: Slope) -> String {
    match slope {
        Slope::Fallback { beta } => format!("fallback beta={:.6}", beta.get()),
        Slope::Measured {
            lower,
            historical_min,
        } => {
            let lower = match lower {
                togi_journal::LowerEndpoint::Bounded { beta } => format!("{:.6}", beta.get()),
                togi_journal::LowerEndpoint::Unbounded {} => "unbounded".to_owned(),
            };
            let history =
                historical_min.map_or("none".to_owned(), |beta| format!("{:.6}", beta.get()));
            format!("measured beta lower endpoint={lower}; earlier minimum={history}")
        }
    }
}

pub(super) fn write(out: &mut String, events: &[Event], policy: &Projection) {
    out.push_str("\nEvidence\n");
    line(
        out,
        "Ruleset 13 (compatibility RULESET 3). S uses isolated mapping classes only. Qualification, reproduction and controls can inform slope but cannot supply mapping rate coverage. The cap is coverage-incomplete.",
    );
    line(
        out,
        "The machine incremental hazard goal is 1 crash per 3,000 applied hours, divided among CCDs. Poisson bounds and log-linear projection are modeling assumptions, not demonstrated rare-event guarantees. CO 0 is a fallback, not proof that stock meets the hazard goal.",
    );
    let selected = policy
        .applied
        .as_ref()
        .map(|profile| (profile, "applied day profile"))
        .or_else(|| {
            policy
                .qualifications
                .iter()
                .rev()
                .find(|record| record.current && record.state == QualificationStage::Complete)
                .map(|record| {
                    (
                        &record.profile,
                        "night-qualified profile (not applied day evidence)",
                    )
                })
        });
    let totals = togi_watch::day_totals::DayTotals::of(events);
    match selected {
        Some((profile, source)) => {
            line(out, &format!("Exact {source}: {}", profile_word(profile)));
            line(
                out,
                &format!(
                    "exact profile: {}",
                    totals.profile(&offsets(profile, policy)).describe()
                ),
            );
        }
        None => line(
            out,
            "Exact day/qualified profile: none recorded; no nonzero day application is established.",
        ),
    }
    line(out, &format!("stock (CO 0): {}", totals.stock().describe()));
    line(
        out,
        &format!(
            "{} across recorded day intervals; downtime and uncheckpointed crash tails are not hours. Totals retain live/imported provenance, not a bound.",
            togi_watch::day_totals::unknown_intervals(totals.unknown_intervals())
        ),
    );

    out.push_str("\nCap calculation witnesses\n");
    for ccd in &policy.ccds {
        let Some(evidence) = &ccd.evidence else {
            line(
                out,
                &format!(
                    "CCD {}: current evidence context unavailable; no inferred rate or eligible hours",
                    ccd.ccd
                ),
            );
            continue;
        };
        line(
            out,
            &format!(
                "CCD {}: current read-only calculation S {}; {}; {}",
                ccd.ccd,
                evidence.offset.get(),
                evidence.reason.label(),
                slope_word(evidence.slope)
            ),
        );
        line(
            out,
            &format!(
                "CCD share of incremental hazard goal: {:.8}/h",
                evidence.budget.get()
            ),
        );
        match &evidence.reason {
            CapReason::Edge { rate_limited } => line(
                out,
                &format!(
                    "rate-limited offset {}; edge margin is five counts",
                    rate_limited.get()
                ),
            ),
            CapReason::Retained { recomputed } => line(
                out,
                &format!(
                    "non-mapping slope-only update recomputed {}; the shallower committed cap is retained",
                    recomputed.get()
                ),
            ),
            CapReason::Rate { class } => {
                line(out, &format!("binding class: {}", class_word(class)));
            }
            CapReason::NoAdequateClass | CapReason::UnknownEdge | CapReason::NoJustifiedDecay => {}
        }
        if let Some(cap) = &ccd.cap {
            line(
                out,
                &format!(
                    "committed S {} [#{}]; {}",
                    cap.offset.get(),
                    cap.witness,
                    slope_word(cap.slope)
                ),
            );
        }
        for (at, cell) in evidence.cells.iter().enumerate() {
            let projected = evidence
                .projected
                .get(at)
                .copied()
                .map_or("no justified projection".to_owned(), bound_word);
            line(
                out,
                &format!(
                    "{} at CO {}: {} eligible s ({} h), {} failures; 95% upper {}; at calculated S: {projected}",
                    class_word(&cell.class),
                    cell.offset.get(),
                    cell.eligible_s,
                    hours(cell.eligible_s),
                    cell.failures,
                    bound_word(cell.bound)
                ),
            );
        }
        for input in &evidence.slope_inputs {
            line(
                out,
                &format!(
                    "slope input: {} at CO {}, {:?}, {:.6} eligible h, {} failures",
                    class_word(&input.class),
                    input.offset.get(),
                    input.purpose,
                    input.hours,
                    input.failures
                ),
            );
        }
        for gap in &evidence.uncovered {
            let reason = match gap.reason {
                togi_journal::UncoveredReason::NoObservations => "no observations",
                togi_journal::UncoveredReason::NonMappingOnly => "non-mapping exposure only",
                togi_journal::UncoveredReason::MappingInadequate => "mapping exposure inadequate",
            };
            line(
                out,
                &format!(
                    "uncovered for the cap ({reason}): {}",
                    class_word(&gap.class)
                ),
            );
        }
    }

    out.push_str("\nQualification exposure\n");
    if policy.qualifications.is_empty() {
        line(out, "none recorded");
    }
    for record in &policy.qualifications {
        line(
            out,
            &format!(
                "{}: {:?}, {} eligible s ({} h) in distinct recorded exposure records, including partials/failures and cited reuse; context {}",
                profile_word(&record.profile),
                record.state,
                record.eligible_s,
                hours(record.eligible_s),
                if record.current { "current" } else { "stale" }
            ),
        );
    }
    day_provenance(out, events);
    applications(out, events);
    observations(out, events, selected.map(|(profile, _)| profile));
    comparisons(out, events, selected.map(|(profile, _)| profile));
    line(
        out,
        "These are observations and coverage gaps. No stability verdict or completion time is inferred.",
    );
}

fn day_provenance(out: &mut String, events: &[Event]) {
    out.push_str("\nDay observation provenance and excluded intervals\n");
    line(
        out,
        "Exact-vector day totals include all recorded firmware contexts; historical hours are not current-context qualification or hold credit.",
    );
    for event in events {
        if let Payload::DayBootStart(_) = &event.payload {
            line(
                out,
                &format!(
                    "[#{}] live observer start; boot {}",
                    event.seq,
                    super::esc(event.boot.as_str())
                ),
            );
        } else if let Payload::DayCheckpoint(checkpoint) = &event.payload {
            line(
                out,
                &format!(
                    "[#{}] live checkpoint; boot {}; {} cumulative observed s",
                    event.seq,
                    super::esc(event.boot.as_str()),
                    checkpoint.applied_secs
                ),
            );
        } else if let Payload::DayInterval(interval) = &event.payload {
            line(
                out,
                &format!(
                    "[#{}] boot {}: {:?}, {} s; {}; application {:?}",
                    event.seq,
                    super::esc(interval.boot.as_str()),
                    interval.state,
                    interval.end_ns.saturating_sub(interval.start_ns) / 1_000_000_000,
                    super::esc(&interval.reason),
                    interval.application
                ),
            );
        } else if let Payload::DayImported(imported) = &event.payload {
            line(
                out,
                &format!(
                    "[#{}] imported {:?}; original session {} #{}; {:?}; {} missing facts",
                    event.seq,
                    imported.original_kind,
                    imported.original.session,
                    imported.original.seq,
                    imported.class,
                    imported.missing.len()
                ),
            );
        } else if let Payload::DayRecord(_) = &event.payload {
            line(out, &format!("[#{}] manually recorded crash", event.seq));
        } else if let Payload::DayUncleanEnd(_) = &event.payload {
            line(
                out,
                &format!(
                    "[#{}] observed unclean end; answers determine crash totals",
                    event.seq
                ),
            );
        }
    }
}

fn applications(out: &mut String, events: &[Event]) {
    out.push_str("\nApplied-step history\n");
    let mut found = false;
    for event in events {
        if let Payload::DayApplied(applied) = &event.payload {
            found = true;
            let cause = match applied.cause {
                ApplyCause::Step { decision } => format!("step {}", decision.0),
                ApplyCause::Reapplication {} => "unchanged reapplication".to_owned(),
                ApplyCause::Reset { cause } => format!("reset [#{cause}]"),
            };
            line(
                out,
                &format!(
                    "[#{}] {}: {}; verified application {} ({cause})",
                    event.seq,
                    event.time,
                    profile_word(applied.target.ccd()),
                    applied.application.0
                ),
            );
        }
    }
    if !found {
        line(out, "none recorded; a proposal is not an application");
    }
}

fn observations(out: &mut String, events: &[Event], profile: Option<&CcdProfile>) {
    out.push_str("\nNight exposure provenance\n");
    let mut seen = BTreeSet::new();
    for event in events {
        if let Payload::ExposureObserved(observed) = &event.payload {
            if !observed.profile.ccd().is_zero() && profile != Some(observed.profile.ccd()) {
                continue;
            }
            if !seen.insert((observed.source.session, observed.source.seq)) {
                continue;
            }
            line(
                out,
                &format!(
                    "[#{}] {}: {} eligible s, {:?}, {:?}; source session {} #{}; epoch {}; {}",
                    event.seq,
                    profile_word(observed.profile.ccd()),
                    observed.eligible_s,
                    observed.activity,
                    observed.origin,
                    observed.source.session,
                    observed.source.seq,
                    observed.epoch,
                    class_word(&observed.class)
                ),
            );
        }
    }
    if seen.is_empty() {
        line(out, "none recorded for the selected profile or stock");
    }
}

/// Compare complete class identities, firmware and epoch, never regime alone. Each pair retains both
/// source records; missing clocks/temperatures stay unknown rather than becoming zero or an uplift.
fn comparisons(out: &mut String, events: &[Event], profile: Option<&CcdProfile>) {
    out.push_str("\nComparable same-class night clocks and temperatures\n");
    let Some(profile) = profile.filter(|profile| !profile.is_zero()) else {
        line(
            out,
            "no distinct nonzero selected profile for a stock comparison",
        );
        return;
    };
    let ended: BTreeMap<_, _> = events
        .iter()
        .filter_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload {
                Some((end.trial.clone(), (event.seq, end)))
            } else {
                None
            }
        })
        .collect();
    let observed: Vec<(Seq, &ExposureObserved)> = events
        .iter()
        .filter_map(|event| {
            if let Payload::ExposureObserved(observed) = &event.payload {
                Some((event.seq, observed))
            } else {
                None
            }
        })
        .collect();
    let mut found = false;
    for (seq, selected) in &observed {
        if selected.profile.ccd() != profile {
            continue;
        }
        let Some((stock_seq, stock)) = observed.iter().rev().find(|(_, stock)| {
            stock.profile.ccd().is_zero()
                && stock.class == selected.class
                && stock.firmware == selected.firmware
                && stock.epoch == selected.epoch
        }) else {
            continue;
        };
        found = true;
        line(
            out,
            &format!(
                "{}: selected [#{seq}], stock [#{stock_seq}] (same class, content, firmware and epoch)",
                class_word(&selected.class)
            ),
        );
        for (label, observation) in [("selected", *selected), ("stock", *stock)] {
            let sample = match observation.origin {
                togi_journal::ExposureOrigin::Live {} => observation
                    .source
                    .trial
                    .as_ref()
                    .and_then(|trial| ended.get(trial)),
                togi_journal::ExposureOrigin::Imported {} => None,
            };
            match sample {
                Some((seq, end)) => {
                    let clocks = end
                        .ccd_mhz
                        .iter()
                        .map(|(ccd, mhz)| format!("CCD {ccd} {mhz} MHz"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let temp = end
                        .tctl_max_c
                        .map_or("unknown".to_owned(), |temp| format!("{temp} C"));
                    line(
                        out,
                        &format!(
                            "{label} [#{seq}]: clocks {}; maximum Tctl {temp}; outcome {:?}",
                            if clocks.is_empty() {
                                "unknown"
                            } else {
                                &clocks
                            },
                            end.evidence_outcome()
                        ),
                    );
                }
                None => line(
                    out,
                    &format!(
                        "{label}: clocks and temperature unknown (no local trial-end witness)"
                    ),
                ),
            }
        }
    }
    if !found {
        line(
            out,
            "no matching stock/selected class with the same content, firmware and epoch; clocks and temperatures unknown",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use togi_journal::{
        Activity, CcdVector, ExposureClass, ExposureOrigin, OriginalSource, Outcome, Placement,
        Stamp, Tested, TrialEnd, TrialId,
    };
    use togi_machine::{BiosContext, CcdId, CoreId, CoreInfo, CpuId, Regime, TargetOffset};

    fn event(seq: u64, payload: impl Into<Payload>) -> Event {
        Event::new(
            Seq::new(seq).unwrap(),
            Stamp {
                time: jiff::Timestamp::UNIX_EPOCH,
                mono: None,
            },
            "boot".to_owned().into(),
            Vec::new(),
            payload.into(),
        )
    }

    fn exposure(trial: &str, offset: i64) -> ExposureObserved {
        let ccd = CcdId::new(0).unwrap();
        let core = CoreId::new(0).unwrap();
        let profile =
            CcdProfile::try_from(BTreeMap::from([(ccd, TargetOffset::new(offset).unwrap())]))
                .unwrap();
        ExposureObserved {
            source: OriginalSource {
                session: "20231114T221320Z".parse().unwrap(),
                seq: Seq::new(if offset == 0 { 1 } else { 2 }).unwrap(),
                boot: "boot".to_owned().into(),
                trial: Some(TrialId::from(trial.to_owned())),
            },
            origin: ExposureOrigin::Live {},
            firmware: BiosContext::default(),
            epoch: 1,
            class: ExposureClass {
                regime: Regime::R1,
                workloads: Vec::new(),
                cores: [core].into_iter().collect(),
                position: None,
                duration_s: 90,
                placement: Placement::Exact,
            },
            profile: CcdVector::expand(
                profile,
                &[CoreInfo {
                    core,
                    ccd,
                    cpus: vec![CpuId::new(0).unwrap()],
                }],
            )
            .unwrap(),
            tested: Tested::Exact {},
            activity: Activity::Qualification,
            outcome: Outcome::Pass,
            failures: Vec::new(),
            eligible_s: 80,
            uninterrupted_s: 80,
            exclusions: Vec::new(),
        }
    }

    fn end(trial: &str, temperature: i32, mhz: u32) -> TrialEnd {
        let mut end: TrialEnd = serde_json::from_value(serde_json::json!({
            "trial": trial, "outcome": "pass", "duration_s": 90,
        }))
        .unwrap();
        end.tctl_max_c = Some(temperature);
        end.ccd_mhz.insert(CcdId::new(0).unwrap(), mhz);
        end
    }

    #[test]
    fn comparable_measurements_keep_both_raw_witnesses_without_an_uplift_verdict() {
        let stock = exposure("stock", 0);
        let selected = exposure("selected", -10);
        let events = vec![
            event(1, end("stock", 60, 5000)),
            event(2, end("selected", 58, 5100)),
            event(3, stock),
            event(4, selected.clone()),
        ];
        let mut out = String::new();
        comparisons(&mut out, &events, Some(selected.profile.ccd()));
        assert!(out.contains("selected [#4], stock [#3]"), "{out}");
        assert!(
            out.contains("selected [#2]: clocks CCD 0 5100 MHz; maximum Tctl 58 C"),
            "{out}"
        );
        assert!(
            out.contains("stock [#1]: clocks CCD 0 5000 MHz; maximum Tctl 60 C"),
            "{out}"
        );
        assert!(!out.contains("faster"));
    }

    fn different_class(observed: &mut ExposureObserved) {
        observed.class.duration_s = 91;
    }
    fn different_epoch(observed: &mut ExposureObserved) {
        observed.epoch = 2;
    }
    fn different_firmware(observed: &mut ExposureObserved) {
        observed.firmware.bios_version = "changed".to_owned();
    }

    #[test_case::test_case(different_class; "class differs")]
    #[test_case::test_case(different_epoch; "epoch differs")]
    #[test_case::test_case(different_firmware; "firmware differs")]
    fn incompatible_measurements_are_not_compared(change: fn(&mut ExposureObserved)) {
        let mut stock = exposure("stock", 0);
        change(&mut stock);
        let selected = exposure("selected", -10);
        let events = vec![event(1, stock), event(2, selected.clone())];
        let mut out = String::new();
        comparisons(&mut out, &events, Some(selected.profile.ccd()));
        assert!(out.contains("no matching stock/selected class"), "{out}");
    }

    #[test]
    fn imported_trial_names_do_not_borrow_local_measurements() {
        let stock = exposure("stock", 0);
        let mut selected = exposure("selected", -10);
        selected.origin = ExposureOrigin::Imported {};
        let events = vec![
            event(1, end("selected", 40, 5500)),
            event(2, stock),
            event(3, selected.clone()),
        ];
        let mut out = String::new();
        comparisons(&mut out, &events, Some(selected.profile.ccd()));
        assert!(
            out.contains("selected: clocks and temperature unknown"),
            "{out}"
        );
        assert!(!out.contains("5500"), "{out}");
    }

    #[test]
    fn absent_application_history_has_an_explicit_plain_snapshot() {
        let mut out = String::new();
        applications(&mut out, &[]);
        assert_eq!(
            out,
            "\nApplied-step history\nnone recorded; a proposal is not an application\n"
        );
    }
}
