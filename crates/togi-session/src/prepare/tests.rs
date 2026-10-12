use std::num::NonZeroU32;
use togi_journal::{Dimension, Refusal};

use togi_journal::{Build, Journal, Marker, Payload, SessionId, StateDir};
use togi_machine::BiosContext;
use togi_sim::MachineDefinition;
use togi_sim::definition::ModelDef;

use togi_carry::CarryError;
use togi_journal::MemFs;
use togi_journal::fs::{Fault, FileKind, Op};

use super::*;
use crate::testing::{Bench, build, simulated};
use crate::{Ended, Stop, run};

fn bench() -> Bench {
    Bench::defined(&MachineDefinition {
        cores: 2,
        model: ModelDef {
            past_limit_rate: 0.0,
            ..ModelDef::default()
        },
        ..MachineDefinition::default()
    })
}

fn ended(bench: &Bench, carry: Option<PendingCarry>) -> Ended {
    let mut input = bench.input();
    input.policy.cycles = Some(NonZeroU32::MIN);
    input.carry = carry;
    let ended = run(input);
    assert_eq!(ended.stop, Some(Stop::Cycles));
    ended
}

fn session_of(journal: &Journal) -> SessionId {
    match &journal.events()[0].payload {
        Payload::SessionStart(start) => start.session,
        other => panic!("{other:?}"),
    }
}

fn recorded_context(journal: &Journal) -> BiosContext {
    journal
        .events()
        .iter()
        .find_map(|event| match &event.payload {
            Payload::SessionContext(context) => Some(context.bios_context()),
            _ => None,
        })
        .unwrap()
}

fn prepared(bench: &Bench, build: &Build, context: Option<&BiosContext>) -> Prepared {
    let boot = bench.sim.seams().host.boot_id().unwrap();
    prepare(
        simulated(&bench.mem),
        build,
        boot,
        bench.sim.now(),
        context,
        bench.defects,
    )
    .unwrap()
}

fn marked(journal: &Journal) -> Vec<(Marker, SessionId)> {
    journal.dir().markers().unwrap()
}

fn archived(journal: &Journal) -> Vec<SessionId> {
    journal.dir().archives().unwrap()
}

fn finished(bench: &Bench) -> (SessionId, BiosContext) {
    let journal = ended(bench, None).journal;
    let facts = (session_of(&journal), recorded_context(&journal));
    drop(journal.close().unwrap());
    facts
}

#[test]
fn a_changed_bios_context_archives_the_live_journal_as_a_transition_and_carries_it() {
    let bench = bench();
    let (id, context) = finished(&bench);
    let changed = BiosContext {
        bios_version: "other".into(),
        ..context
    };
    let prepared = prepared(&bench, &build(), Some(&changed));
    assert!(prepared.journal.events().is_empty());
    assert_eq!(archived(&prepared.journal), [id]);
    assert_eq!(marked(&prepared.journal), [(Marker::CarryPending, id)]);
    let carry = prepared.carry.unwrap();
    assert_eq!(carry.sources()[0].session, id);
}

#[test]
fn an_unchanged_or_unknown_context_leaves_the_live_journal_and_carries_nothing() {
    let bench = bench();
    let (id, context) = finished(&bench);
    for current in [Some(context), None] {
        let prepared = prepared(&bench, &build(), current.as_ref());
        assert_eq!(session_of(&prepared.journal), id);
        assert!(archived(&prepared.journal).is_empty());
        assert!(marked(&prepared.journal).is_empty());
        assert!(prepared.carry.is_none());
        drop(prepared.journal.close().unwrap());
    }
}

#[test]
fn a_journal_without_a_recorded_context_is_kept_whatever_the_current_one() {
    let rig = crate::testing::Rig::new(2);
    let bench = Bench {
        mem: rig.mem.clone(),
        ..bench()
    };
    drop(rig.appender);
    let other = BiosContext {
        bios_version: "other".into(),
        ..BiosContext::default()
    };
    let prepared = prepared(&bench, &build(), Some(&other));
    assert_eq!(prepared.journal.events().len(), 1);
    assert!(archived(&prepared.journal).is_empty());
}

#[test]
fn a_journal_of_another_ruleset_is_archived_unread_and_prepare_repeats_the_same_carry() {
    let bench = bench();
    let (id, context) = finished(&bench);
    let next = Build {
        ruleset: 2,
        ..build()
    };
    let first = prepared(&bench, &next, Some(&context));
    assert!(first.journal.events().is_empty());
    assert_eq!(archived(&first.journal), [id]);
    let carry = first.carry.clone().unwrap();
    assert_eq!(carry.sources()[0].session, id);
    drop(first.journal.close().unwrap());

    let again = prepared(&bench, &next, Some(&context));
    assert_eq!(archived(&again.journal), [id]);
    assert_eq!(again.carry, Some(carry));
}

#[test]
fn an_interrupted_bios_archive_is_finished_and_prepare_repeats_the_same_carry() {
    let bench = bench();
    let (id, context) = finished(&bench);
    let changed = BiosContext {
        bios_version: "other".into(),
        ..context
    };
    let clean = self::bench();
    finished(&clean);
    let expected = prepared(&clean, &build(), Some(&changed)).carry.unwrap();

    let boot = bench.sim.seams().host.boot_id().unwrap();
    let live = simulated(&bench.mem)
        .open_at(boot, bench.sim.now())
        .unwrap();
    let before = bench.mem.contents("/state/events.jsonl").unwrap();
    bench
        .mem
        .inject(Fault::before(Op::Rename, 1).named("events.jsonl"));
    let stamp = Stamp {
        time: bench.sim.now(),
        mono: None,
    };
    live.archive(stamp, ArchiveKind::Transition, &[])
        .unwrap_err();
    let interrupted = bench.mem.contents("/state/events.jsonl").unwrap();
    assert!(interrupted.starts_with(&before));
    assert!(interrupted.len() > before.len());
    let dir = simulated(&bench.mem);
    assert_eq!(dir.live_session().unwrap(), Some(id));
    assert_eq!(dir.markers().unwrap(), [(Marker::CarryPending, id)]);
    assert!(dir.archives().unwrap().is_empty());
    drop(dir);

    let first = prepared(&bench, &build(), Some(&changed));
    assert!(first.journal.events().is_empty());
    assert_eq!(archived(&first.journal), [id]);
    assert_eq!(marked(&first.journal), [(Marker::CarryPending, id)]);
    assert_eq!(
        bench.mem.contents(format!("/state/archive/{id}.jsonl")),
        Some(interrupted)
    );
    let carry = first.carry.clone().unwrap();
    assert_eq!(carry.sources()[0].session, id);
    assert_eq!(carry, expected);
    drop(first.journal.close().unwrap());

    let again = prepared(&bench, &build(), Some(&changed));
    assert_eq!(archived(&again.journal), [id]);
    assert_eq!(again.carry, Some(carry));
}

#[test]
fn a_journal_this_build_refuses_is_refused_before_it_is_opened_and_left_unchanged() {
    let older = Build {
        ruleset: 0,
        ..build()
    };
    let tails: [&[u8]; 3] = [
        b"",
        b"{\"seq\":99,\"kind\":\"journal.torn",
        b"{\"seq\":99,\"kind\":\"from.the.future\",\"data\":{}}\n",
    ];
    for tail in tails {
        let bench = bench();
        let (_, context) = finished(&bench);
        let mut live = bench.mem.contents("/state/events.jsonl").unwrap();
        live.extend_from_slice(tail);
        bench.mem.put("/state/events.jsonl", live.clone());
        let boot = bench.sim.seams().host.boot_id().unwrap();
        let error = prepare(
            simulated(&bench.mem),
            &older,
            boot,
            bench.sim.now(),
            Some(&context),
            bench.defects,
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                PrepareError::Refused(Refusal {
                    dimension: Dimension::Ruleset,
                    current: 0,
                    ..
                })
            ),
            "{error:?}"
        );
        assert_eq!(bench.mem.contents("/state/events.jsonl"), Some(live));
        let dir = simulated(&bench.mem);
        assert!(dir.archives().unwrap().is_empty());
        assert!(dir.markers().unwrap().is_empty());
    }
}

#[test]
fn a_carry_the_live_journal_committed_clears_its_marker_and_is_not_carried_again() {
    let bench = bench();
    let (id, context) = finished(&bench);
    let next = Build {
        ruleset: 2,
        ..build()
    };
    let first = prepared(&bench, &next, Some(&context));
    let carry = first.carry.clone().unwrap();
    drop(first.journal.close().unwrap());
    let mut input = bench.input();
    input.build = next.clone();
    input.policy.cycles = Some(NonZeroU32::MIN);
    input.carry = Some(carry);
    let committed = run(input).journal;
    assert!(
        committed
            .events()
            .iter()
            .any(|event| matches!(&event.payload, Payload::SessionCarried(_)))
    );
    assert_eq!(marked(&committed), [(Marker::CarryPending, id)]);
    drop(committed.close().unwrap());

    let after = prepared(&bench, &next, Some(&context));
    assert!(after.carry.is_none());
    assert!(marked(&after.journal).is_empty());
    assert_ne!(session_of(&after.journal), id);
}

#[test]
fn a_live_journal_at_the_reset_boundary_is_archived_unchanged_and_the_carry_marker_dropped() {
    let bench = bench();
    let (older, context) = finished(&bench);
    let next = Build {
        ruleset: 2,
        ..build()
    };
    let first = prepared(&bench, &next, Some(&context));
    drop(first.journal.close().unwrap());
    let mut input = bench.input();
    input.build = next.clone();
    input.policy.cycles = Some(NonZeroU32::MIN);
    let live = run(input).journal;
    let id = session_of(&live);
    let mut dir = live.close().unwrap();
    assert_eq!(dir.mark_reset_all().unwrap(), Some(id));
    drop(dir);
    let before = bench.mem.contents("/state/events.jsonl").unwrap();

    let prepared = prepared(&bench, &next, Some(&context));
    assert!(prepared.journal.events().is_empty());
    assert!(prepared.carry.is_none());
    assert_eq!(marked(&prepared.journal), [(Marker::ResetAll, id)]);
    assert_eq!(archived(&prepared.journal), [older, id]);
    assert_eq!(
        bench.mem.contents(format!("/state/archive/{id}.jsonl")),
        Some(before)
    );
}

#[test]
fn a_pending_carry_the_boundary_passed_is_dropped_without_a_carry() {
    let bench = bench();
    let (id, context) = finished(&bench);
    let next = Build {
        ruleset: 2,
        ..build()
    };
    let first = prepared(&bench, &next, Some(&context));
    let mut dir = first.journal.close().unwrap();
    dir.mark_reset_all().unwrap();
    drop::<StateDir>(dir);
    let after = prepared(&bench, &next, Some(&context));
    assert!(after.carry.is_none());
    assert_eq!(marked(&after.journal), [(Marker::ResetAll, id)]);
}

#[test]
fn an_older_journal_with_events_this_build_cannot_decode_is_archived_unread_and_supplies_no_carry()
{
    let bench = bench();
    let (id, context) = finished(&bench);
    let mut garbled = bench.mem.contents("/state/events.jsonl").unwrap();
    garbled.extend_from_slice(b"{\"seq\":99,\"kind\":\"from.the.future\",\"data\":{}}\n");
    bench.mem.put("/state/events.jsonl", garbled.clone());
    let next = Build {
        ruleset: 2,
        ..build()
    };
    let prepared = prepared(&bench, &next, Some(&context));
    assert!(prepared.carry.is_none());
    assert!(prepared.journal.events().is_empty());
    assert_eq!(archived(&prepared.journal), [id]);
    assert_eq!(marked(&prepared.journal), [(Marker::CarryPending, id)]);
    assert_eq!(
        bench.mem.contents(format!("/state/archive/{id}.jsonl")),
        Some(garbled)
    );
}

#[test]
fn an_older_archive_this_build_cannot_decode_ends_the_walk_without_failing_the_carry() {
    let next = Build {
        ruleset: 2,
        ..build()
    };
    let clean = bench();
    let (_, context) = finished(&clean);
    let expected = prepared(&clean, &next, Some(&context)).carry.unwrap();

    let behind = bench();
    finished(&behind);
    behind
        .mem
        .put("/state/archive/20200101T000000Z.jsonl", "not json\n");
    let got = prepared(&behind, &next, Some(&context));
    assert_eq!(got.carry, Some(expected));
}

#[test]
fn an_archive_that_cannot_be_read_still_fails_the_carry() {
    let bench = bench();
    let (id, context) = finished(&bench);
    let next = Build {
        ruleset: 2,
        ..build()
    };
    drop(
        prepared(&bench, &next, Some(&context))
            .journal
            .close()
            .unwrap(),
    );
    bench
        .mem
        .inject(Fault::before(Op::Read, 1).named(format!("{id}.jsonl")));
    let boot = bench.sim.seams().host.boot_id().unwrap();
    let error = prepare(
        simulated(&bench.mem),
        &next,
        boot,
        bench.sim.now(),
        Some(&context),
        bench.defects,
    )
    .unwrap_err();
    assert!(
        matches!(&error, PrepareError::Carry(CarryError::Open { session, .. }) if *session == id),
        "{error:?}"
    );
}

#[test]
fn a_broken_archive_layout_or_live_header_refuses_the_preparation_and_leaves_the_live_journal_unchanged()
 {
    let next = Build {
        ruleset: 2,
        ..build()
    };
    for layout in [
        "archive is a file",
        "archive target is a directory",
        "invalid session header",
    ] {
        let bench = bench();
        let (id, context) = finished(&bench);
        match layout {
            "archive is a file" => bench.mem.put("/state/archive", ""),
            "archive target is a directory" => {
                bench.mem.put(format!("/state/archive/{id}.jsonl/keep"), "");
            }
            _ => bench.mem.put("/state/events.jsonl", "not JSON\n"),
        }
        let before = bench.mem.contents("/state/events.jsonl").unwrap();
        let boot = bench.sim.seams().host.boot_id().unwrap();
        let error = prepare(
            simulated(&bench.mem),
            &next,
            boot,
            bench.sim.now(),
            Some(&context),
            bench.defects,
        )
        .unwrap_err();
        let expected = match layout {
            "archive target is a directory" => {
                matches!(&error, PrepareError::Dir(DirError::ArchiveExists { session }) if *session == id)
            }
            "invalid session header" => {
                matches!(&error, PrepareError::Dir(DirError::NotAJournal { .. }))
            }
            _ => matches!(&error, PrepareError::Dir(_)),
        };
        assert!(expected, "{layout}: {error:?}");
        assert_eq!(
            bench.mem.contents("/state/events.jsonl"),
            Some(before),
            "{layout}"
        );
        assert!(
            bench
                .mem
                .contents(format!("/state/archive/{id}.jsonl"))
                .is_none(),
            "{layout}"
        );
    }
}

type Scenario = (&'static str, Bench, Build, Option<BiosContext>);

fn next_build() -> Build {
    Build {
        ruleset: 2,
        ..build()
    }
}

/// Directories in every state `prepare` decides on, each with the build and BIOS context the next boot has.
fn scenarios() -> Vec<Scenario> {
    let mut all: Vec<Scenario> = vec![("empty", bench(), build(), None)];

    let plain = bench();
    let (_, context) = finished(&plain);
    all.push(("plain resume", plain, build(), Some(context.clone())));

    let transition = bench();
    let (_, context) = finished(&transition);
    let changed = BiosContext {
        bios_version: "other".into(),
        ..context
    };
    all.push(("context change", transition, build(), Some(changed)));

    let archive = bench();
    let (_, context) = finished(&archive);
    all.push(("build archive", archive, next_build(), Some(context)));

    let unsettled = bench();
    let (_, context) = finished(&unsettled);
    drop(
        prepared(&unsettled, &next_build(), Some(&context))
            .journal
            .close()
            .unwrap(),
    );
    all.push(("carry unsettled", unsettled, next_build(), Some(context)));

    let settled = bench();
    let (_, context) = finished(&settled);
    let first = prepared(&settled, &next_build(), Some(&context));
    drop(first.journal.close().unwrap());
    let mut input = settled.input();
    input.build = next_build();
    input.policy.cycles = Some(NonZeroU32::MIN);
    input.carry = first.carry;
    drop(run(input).journal.close().unwrap());
    all.push(("carry settled", settled, next_build(), Some(context)));

    let reset = bench();
    let (_, context) = finished(&reset);
    drop(
        prepared(&reset, &next_build(), Some(&context))
            .journal
            .close()
            .unwrap(),
    );
    let mut input = reset.input();
    input.build = next_build();
    input.policy.cycles = Some(NonZeroU32::MIN);
    let mut dir = run(input).journal.close().unwrap();
    dir.mark_reset_all().unwrap();
    drop(dir);
    all.push(("reset boundary", reset, next_build(), Some(context)));
    all
}

fn boot(name: &str) -> BootId {
    BootId::from(name.to_owned())
}

fn copy_of(mem: &MemFs) -> MemFs {
    let copy = MemFs::new();
    for (path, kind) in mem.entries() {
        if kind == FileKind::File {
            copy.put(&path, mem.contents(&path).unwrap());
        }
    }
    copy
}

/// Everything a preparation leaves: the journal it hands over, its carry and the directory's files.
fn left(prepared: &Prepared, mem: &MemFs) -> impl PartialEq + std::fmt::Debug {
    let events: Vec<String> = prepared
        .journal
        .events()
        .iter()
        .map(|event| format!("{event:?}"))
        .collect();
    let files: Vec<_> = mem
        .entries()
        .into_iter()
        .map(|(path, kind)| {
            let bytes = mem.contents(&path);
            (path, kind, bytes)
        })
        .collect();
    (
        events,
        prepared.journal.boot().clone(),
        prepared.journal.session(),
        prepared.carry.clone(),
        files,
    )
}

#[test]
fn a_kept_journal_is_prepared_as_the_reopened_directory_is() {
    for (name, bench, build, context) in scenarios() {
        let rebooted = boot("rebooted");
        let now = bench.sim.now();
        let (closed, kept) = (copy_of(&bench.mem), copy_of(&bench.mem));
        let expected = prepare(
            simulated(&closed),
            &build,
            rebooted.clone(),
            now,
            context.as_ref(),
            bench.defects,
        )
        .unwrap();

        let mut journal = simulated(&kept).open_at(boot("crashed"), now).unwrap();
        journal.reboot(rebooted.clone()).unwrap();
        let reads = kept.reads().len();
        let actual = prepare_kept(
            journal,
            &build,
            rebooted,
            now,
            context.as_ref(),
            bench.defects,
        )
        .unwrap();
        assert_eq!(
            left(&actual, &kept),
            left(&expected, &closed),
            "{name}: the files, events and carry differ"
        );
        if matches!(name, "empty" | "plain resume" | "carry unsettled") {
            assert!(
                kept.reads()[reads..]
                    .iter()
                    .all(|path| !path.ends_with("events.jsonl")),
                "{name}: the kept journal was read from its file"
            );
        }
    }
}

#[test]
fn a_kept_journal_this_build_refuses_is_refused_and_left_unchanged() {
    let bench = bench();
    let (_, context) = finished(&bench);
    let older = Build {
        ruleset: 0,
        ..build()
    };
    let before = bench.mem.contents("/state/events.jsonl").unwrap();
    let now = bench.sim.now();
    let mut journal = simulated(&bench.mem).open_at(boot("crashed"), now).unwrap();
    journal.reboot(boot("rebooted")).unwrap();
    let error = prepare_kept(
        journal,
        &older,
        boot("rebooted"),
        now,
        Some(&context),
        bench.defects,
    )
    .unwrap_err();
    assert!(matches!(error, PrepareError::Refused(_)), "{error:?}");
    assert_eq!(bench.mem.contents("/state/events.jsonl"), Some(before));
    assert!(simulated(&bench.mem).archives().unwrap().is_empty());
}
