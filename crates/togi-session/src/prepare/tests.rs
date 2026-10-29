use test_case::test_case;
use togi_journal::{Dimension, ImportClass, MissingFact, Refusal, Stamp};

use togi_journal::{Build, Journal, Marker, Payload, SessionId, StateDir};
use togi_machine::BiosContext;
use togi_sim::MachineDefinition;
use togi_sim::definition::ModelDef;

use togi_journal::MemFs;
use togi_journal::fs::{Fault, FileKind, Op};

use super::*;
use crate::testing::{Bench, build, finished_with_day_records, simulated};
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

fn ended(bench: &Bench) -> Ended {
    let ended = run(bench.input_ending_at(bench.short_night()));
    assert!(matches!(ended.stop, Some(Stop::Signal)), "{:?}", ended.stop);
    ended
}

fn session_of(journal: &Journal) -> SessionId {
    let payload = &journal.events()[0].payload;
    let Payload::SessionStart(start) = payload else {
        panic!("{payload:?}");
    };
    start.session
}

fn prepared(bench: &Bench, build: &Build) -> Prepared {
    let boot = bench.sim.seams().host.boot_id().unwrap();
    prepare(simulated(&bench.mem), build, boot, bench.sim.now()).unwrap()
}

fn marked(journal: &Journal) -> Vec<(Marker, SessionId)> {
    journal.dir().markers().unwrap()
}

fn archived(journal: &Journal) -> Vec<SessionId> {
    journal.dir().archives().unwrap()
}

fn finished(bench: &Bench) -> SessionId {
    finished_with_day_records(bench).0
}

fn next_build() -> Build {
    Build {
        ruleset: 2,
        ..build()
    }
}

#[test]
fn a_journal_of_another_ruleset_archives_and_selects_raw_observations_and_campaign_history() {
    let bench = bench();
    let id = finished(&bench);
    let prepared = prepared(&bench, &next_build());
    assert!(prepared.journal.events().is_empty());
    assert_eq!(archived(&prepared.journal), [id]);
    assert_eq!(marked(&prepared.journal), [(Marker::ImportPending, id)]);
    let import = prepared.import.unwrap();
    assert_eq!(import.source(), id);
    assert!(import.destination() > id);
    let pending = prepared.journal.dir().pending_import().unwrap().unwrap();
    assert_eq!(pending.source, id);
    assert_eq!(pending.destination, import.destination());

    let manifest = import.manifest();
    let kinds: Vec<_> = manifest
        .records()
        .iter()
        .map(|record| record.original_kind.as_str())
        .collect();
    assert_eq!(
        kinds,
        [
            "session.start",
            "campaign.start",
            "exposure.observed",
            "day.boot_start",
            "day.checkpoint",
            "day.clean_stop"
        ]
    );
    assert!(manifest.records().iter().all(|r| r.original.session == id));
    assert!(manifest.unknown().is_empty());
    let counts = manifest.commitment().counts;
    assert_eq!(
        (counts.selected, counts.imported, counts.duplicates),
        (6, 6, 0)
    );
    assert_eq!(counts.excluded, 0);
}

#[test]
fn an_imported_record_names_what_its_payload_lacked() {
    let bench = bench();
    finished(&bench);
    let import = prepared(&bench, &next_build()).import.unwrap();
    let records = import.manifest().records();
    let by_kind = |kind| {
        records
            .iter()
            .find(|record| record.original_kind == kind)
            .unwrap()
    };
    let (start, checkpoint, stop) = (
        by_kind(togi_journal::EventKind::DayBootStart),
        by_kind(togi_journal::EventKind::DayCheckpoint),
        by_kind(togi_journal::EventKind::DayCleanStop),
    );
    assert_eq!(start.class, ImportClass::Boot);
    assert!(start.missing.is_empty());
    assert!(start.build.is_some() && start.firmware.is_some() && start.profile.is_some());
    assert_eq!(checkpoint.class, ImportClass::Checkpoint);
    for fact in [
        MissingFact::Build,
        MissingFact::Firmware,
        MissingFact::Profile,
    ] {
        assert!(checkpoint.missing.contains(&fact), "{fact:?}");
    }
    assert_eq!(stop.class, ImportClass::End);
}

#[test]
fn a_changed_bios_context_is_no_transition_the_live_journal_stays_and_imports_nothing() {
    let bench = bench();
    let id = finished(&bench);
    let mut host = bench.sim.seams().host;
    let context = host.bios_context().unwrap();
    bench.sim.set_bios_context(BiosContext {
        bios_version: "other".into(),
        ..context
    });

    let prepared = prepared(&bench, &build());
    assert_eq!(session_of(&prepared.journal), id);
    assert!(archived(&prepared.journal).is_empty());
    assert!(marked(&prepared.journal).is_empty());
    assert!(prepared.import.is_none());
    drop(prepared.journal.close().unwrap());
}

#[test]
fn a_journal_of_another_ruleset_is_archived_unread_and_prepare_repeats_the_same_import() {
    let bench = bench();
    let id = finished(&bench);
    let first = prepared(&bench, &next_build());
    assert!(first.journal.events().is_empty());
    assert_eq!(archived(&first.journal), [id]);
    let import = first.import.clone().unwrap();
    assert_eq!(import.source(), id);
    assert_eq!(import.manifest().records().len(), 6);
    drop(first.journal.close().unwrap());

    let again = prepared(&bench, &next_build());
    assert_eq!(archived(&again.journal), [id]);
    assert_eq!(again.import, Some(import));
}

#[test]
fn an_interrupted_archive_is_finished_and_prepare_repeats_the_same_import() {
    let bench = bench();
    let id = finished(&bench);
    let clean = self::bench();
    finished(&clean);
    let expected = prepared(&clean, &next_build()).import.unwrap();

    let destination = simulated(&bench.mem)
        .import_destination(&id, bench.sim.now())
        .unwrap();
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
    live.archive(stamp, ArchiveKind::Transition { destination }, &[])
        .unwrap_err();
    let interrupted = bench.mem.contents("/state/events.jsonl").unwrap();
    assert!(interrupted.starts_with(&before));
    assert!(interrupted.len() > before.len());
    let dir = simulated(&bench.mem);
    assert_eq!(dir.live_session().unwrap(), Some(id));
    assert_eq!(dir.markers().unwrap(), [(Marker::ImportPending, id)]);
    assert!(dir.archives().unwrap().is_empty());
    drop(dir);

    let first = prepared(&bench, &next_build());
    assert!(first.journal.events().is_empty());
    assert_eq!(archived(&first.journal), [id]);
    assert_eq!(marked(&first.journal), [(Marker::ImportPending, id)]);
    assert_eq!(
        bench.mem.contents(format!("/state/archive/{id}.jsonl")),
        Some(interrupted)
    );
    let import = first.import.clone().unwrap();
    assert_eq!(import.source(), id);
    assert_eq!(import.destination(), destination);
    assert_eq!(import, expected);
    drop(first.journal.close().unwrap());

    let again = prepared(&bench, &next_build());
    assert_eq!(archived(&again.journal), [id]);
    assert_eq!(again.import, Some(import));
}

#[test_case(b""; "complete")]
#[test_case(b"{\"seq\":99,\"kind\":\"journal.torn"; "torn tail")]
#[test_case(b"{\"seq\":99,\"kind\":\"from.the.future\",\"data\":{}}\n"; "unknown event")]
fn a_journal_this_build_refuses_is_refused_before_it_is_opened_and_left_unchanged(tail: &[u8]) {
    let older = Build {
        ruleset: 0,
        ..build()
    };

    let bench = bench();
    let _ = finished(&bench);
    let mut live = bench.mem.contents("/state/events.jsonl").unwrap();
    live.extend_from_slice(tail);
    bench.mem.put("/state/events.jsonl", live.clone());
    let boot = bench.sim.seams().host.boot_id().unwrap();
    let error = prepare(simulated(&bench.mem), &older, boot, bench.sim.now()).unwrap_err();
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

#[test]
fn an_import_the_live_journal_committed_clears_its_marker_and_is_not_imported_again() {
    let bench = bench();
    let id = finished(&bench);
    let first = prepared(&bench, &next_build());
    let import = first.import.clone().unwrap();
    drop(first.journal.close().unwrap());
    let mut input = bench.input_ending_at(bench.short_night());
    input.build = next_build();
    input.import = Some(import.clone());
    let committed = run(input).journal;
    assert_eq!(session_of(&committed), import.destination());
    let imported = committed
        .events()
        .iter()
        .filter(|event| matches!(&event.payload, Payload::DayImported(_)))
        .count();
    assert_eq!(imported, 6);
    assert!(
        committed
            .events()
            .iter()
            .any(|event| matches!(&event.payload, Payload::SessionImported(_)))
    );
    assert_eq!(marked(&committed), [(Marker::ImportPending, id)]);
    drop(committed.close().unwrap());

    let after = prepared(&bench, &next_build());
    assert!(after.import.is_none());
    assert!(marked(&after.journal).is_empty());
    assert_eq!(session_of(&after.journal), import.destination());
}

#[test]
fn a_transition_out_of_an_uncommitted_import_keeps_the_original_source_and_counts_the_copies_once()
{
    let bench = bench();
    let id = finished(&bench);
    let first = prepared(&bench, &next_build());
    let import = first.import.clone().unwrap();
    drop(first.journal.close().unwrap());

    let boot = bench.sim.seams().host.boot_id().unwrap();
    let mut live = simulated(&bench.mem)
        .open_at(boot, bench.sim.now())
        .unwrap();
    let stamp = Stamp {
        time: bench.sim.now(),
        mono: None,
    };
    live.append(
        stamp,
        Payload::SessionStart(togi_journal::SessionStart {
            build: next_build(),
            session: import.destination(),
            cores: Vec::new(),
        }),
        &[],
    )
    .unwrap();
    live.append(
        stamp,
        Payload::DayImported(import.manifest().records()[0].clone()),
        &[],
    )
    .unwrap();
    drop(live.close().unwrap());

    let newer = Build {
        ruleset: 3,
        ..build()
    };
    let second = prepared(&bench, &newer);
    assert_eq!(archived(&second.journal), [id, import.destination()]);
    assert_eq!(marked(&second.journal), [(Marker::ImportPending, id)]);
    let again = second.import.unwrap();
    assert_eq!(again.source(), id);
    assert!(again.destination() > import.destination());
    let manifest = again.manifest();
    assert!(
        import
            .manifest()
            .records()
            .iter()
            .all(|record| manifest.records().contains(record))
    );
    assert_eq!(
        manifest.records().last().unwrap().original.session,
        import.destination()
    );
    let counts = manifest.commitment().counts;
    assert_eq!(
        (counts.selected, counts.imported, counts.duplicates),
        (7, 7, 1)
    );
}

#[test]
fn a_transition_reads_campaign_history_before_the_last_committed_import() {
    let bench = bench();
    let id = finished(&bench);
    let first = prepared(&bench, &next_build());
    let import = first.import.clone().unwrap();
    drop(first.journal.close().unwrap());
    let mut input = bench.input_ending_at(bench.short_night());
    input.build = next_build();
    input.import = Some(import.clone());
    drop(run(input).journal.close().unwrap());

    let newer = Build {
        ruleset: 3,
        ..build()
    };
    let second = prepared(&bench, &newer);
    let again = second.import.unwrap();
    assert_eq!(archived(&second.journal), [id, import.destination()]);
    let manifest = again.manifest();
    let counts = manifest.commitment().counts;
    assert!(counts.selected > 6);
    assert!(counts.duplicates > 0);
    assert!(
        manifest
            .records()
            .iter()
            .any(|record| record.original.session == id)
    );
    let selected: std::collections::HashSet<_> = manifest.selected().collect();
    assert_eq!(selected.len(), manifest.records().len());
    let sources = manifest.commitment().sources;
    assert_eq!(
        sources
            .iter()
            .map(|source| source.session)
            .collect::<Vec<_>>(),
        [id, import.destination()]
    );
}

#[test]
fn a_live_journal_at_the_reset_boundary_is_archived_unchanged_and_the_import_marker_dropped() {
    let bench = bench();
    let older = finished(&bench);
    let first = prepared(&bench, &next_build());
    drop(first.journal.close().unwrap());
    let mut input = bench.input_ending_at(bench.short_night());
    input.build = next_build();
    input.import = first.import;
    let live = run(input).journal;
    let id = session_of(&live);
    let mut dir = live.close().unwrap();
    assert_eq!(dir.mark_reset_all().unwrap(), Some(id));
    drop(dir);
    let before = bench.mem.contents("/state/events.jsonl").unwrap();

    let prepared = prepared(&bench, &next_build());
    assert!(prepared.journal.events().is_empty());
    assert!(
        prepared
            .import
            .as_ref()
            .unwrap()
            .manifest()
            .records()
            .iter()
            .all(|record| !matches!(
                record.original_kind,
                togi_journal::EventKind::ExposureObserved
                    | togi_journal::EventKind::DayBootStart
                    | togi_journal::EventKind::DayCheckpoint
                    | togi_journal::EventKind::DayCleanStop
            ))
    );
    assert_eq!(marked(&prepared.journal), [(Marker::ResetAll, id)]);
    assert_eq!(archived(&prepared.journal), [older, id]);
    assert_eq!(
        bench.mem.contents(format!("/state/archive/{id}.jsonl")),
        Some(before)
    );
}

#[test]
fn a_pending_import_the_boundary_passed_keeps_campaign_history_without_raw_tuning_data() {
    let bench = bench();
    let id = finished(&bench);
    let first = prepared(&bench, &next_build());
    let mut dir = first.journal.close().unwrap();
    dir.mark_reset_all().unwrap();
    drop::<StateDir>(dir);
    let after = prepared(&bench, &next_build());
    assert!(
        after
            .import
            .as_ref()
            .unwrap()
            .manifest()
            .records()
            .iter()
            .all(|record| matches!(
                record.original_kind,
                togi_journal::EventKind::SessionStart | togi_journal::EventKind::CampaignStart
            ))
    );
    assert_eq!(
        marked(&after.journal),
        [(Marker::ImportPending, id), (Marker::ResetAll, id)]
    );
}

#[test]
fn a_reset_boundary_stops_raw_tuning_carry_but_not_campaign_history() {
    let bench = bench();
    let old = finished(&bench);
    let mut dir = simulated(&bench.mem);
    dir.archive_unreadable(ArchiveKind::ResetAll)
        .unwrap()
        .unwrap();
    dir.mark_reset_all().unwrap();
    drop(dir);
    assert_eq!(simulated(&bench.mem).archives().unwrap(), [old]);

    bench.sim.reboot().unwrap();
    let mut journal = ended(&bench).journal;
    let id = session_of(&journal);
    assert_ne!(id, old);
    let stamp = Stamp {
        time: bench.sim.now(),
        mono: None,
    };
    journal
        .append(
            stamp,
            Payload::DayCleanStop(togi_journal::DayCleanStop {}),
            &[],
        )
        .unwrap();
    drop(journal.close().unwrap());
    let prepared = prepared(&bench, &next_build());
    let import = prepared.import.unwrap();
    assert_eq!(import.source(), id);
    let records = import.manifest().records();
    assert!(records.iter().any(|record| record.original.session == old
        && record.original_kind == togi_journal::EventKind::CampaignStart));
    assert!(
        records
            .iter()
            .filter(|record| record.original.session == old)
            .all(|record| matches!(
                record.original_kind,
                togi_journal::EventKind::SessionStart | togi_journal::EventKind::CampaignStart
            ))
    );
    assert!(records.iter().any(|record| record.original.session == id
        && record.original_kind == togi_journal::EventKind::DayCleanStop));
}

#[test]
fn unknown_kinds_and_unreadable_lines_are_listed_and_never_imported() {
    let bench = bench();
    let id = finished(&bench);
    let mut bytes = bench.mem.contents("/state/events.jsonl").unwrap();
    bytes.extend_from_slice(
        b"{\"seq\":99,\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"b\",\"kind\":\"from.the.future\",\"data\":{}}\nnot json\n",
    );
    bench.mem.put("/state/events.jsonl", bytes.clone());
    let import = prepared(&bench, &next_build()).import.unwrap();
    let manifest = import.manifest();
    assert_eq!(manifest.records().len(), 6);
    let kinds: Vec<_> = manifest
        .unknown()
        .iter()
        .map(|record| record.kind.as_str())
        .collect();
    assert_eq!(kinds, ["from.the.future", "unreadable"]);
    assert!(manifest.unknown().iter().all(|r| r.original.session == id));
    assert_eq!(manifest.commitment().counts.unknown, 2);
    assert_eq!(
        bench.mem.contents(format!("/state/archive/{id}.jsonl")),
        Some(bytes)
    );
}

#[test]
fn a_torn_tail_that_parses_is_never_an_imported_record() {
    let bench = bench();
    let id = finished(&bench);
    let mut bytes = bench.mem.contents("/state/events.jsonl").unwrap();
    let last = bytes[..bytes.len() - 1]
        .split(|byte| *byte == b'\n')
        .next_back()
        .unwrap();
    let mut tail: serde_json::Value = serde_json::from_slice(last).unwrap();
    tail["seq"] = 99.into();
    serde_json::to_writer(&mut bytes, &tail).unwrap();
    assert_eq!(bytes.last(), Some(&b'}'));
    bench.mem.put("/state/events.jsonl", bytes);
    let import = prepared(&bench, &next_build()).import.unwrap();
    let manifest = import.manifest();
    assert_eq!(manifest.records().len(), 6);
    assert!(manifest.unknown().is_empty());
    assert!(
        manifest
            .records()
            .iter()
            .all(|record| { record.original.session == id && record.original.seq.get() != 99 })
    );
}

#[test]
fn an_archive_that_cannot_be_read_fails_the_preparation() {
    let bench = bench();
    let id = finished(&bench);
    drop(prepared(&bench, &next_build()).journal.close().unwrap());
    bench
        .mem
        .inject(Fault::before(Op::Read, 1).named(format!("{id}.jsonl")));
    let boot = bench.sim.seams().host.boot_id().unwrap();
    let error = prepare(simulated(&bench.mem), &next_build(), boot, bench.sim.now()).unwrap_err();
    assert!(matches!(&error, PrepareError::Dir(_)), "{error:?}");
}

#[derive(Debug, Clone, Copy)]
enum BrokenLayout {
    ArchiveFile,
    ArchiveTargetDirectory,
    InvalidHeader,
}

#[test_case(BrokenLayout::ArchiveFile; "archive is a file")]
#[test_case(BrokenLayout::ArchiveTargetDirectory; "archive target is a directory")]
#[test_case(BrokenLayout::InvalidHeader; "invalid session header")]
fn a_broken_archive_layout_or_live_header_refuses_the_preparation_and_leaves_the_live_journal_unchanged(
    layout: BrokenLayout,
) {
    let bench = bench();
    let id = finished(&bench);
    let destination = simulated(&bench.mem)
        .import_destination(&id, bench.sim.now())
        .unwrap();
    match layout {
        BrokenLayout::ArchiveFile => bench.mem.put("/state/archive", ""),
        BrokenLayout::ArchiveTargetDirectory => {
            bench.mem.put(format!("/state/archive/{id}.jsonl/keep"), "");
        }
        BrokenLayout::InvalidHeader => bench.mem.put("/state/events.jsonl", "not JSON\n"),
    }
    let before = bench.mem.contents("/state/events.jsonl").unwrap();
    let boot = bench.sim.seams().host.boot_id().unwrap();
    let error = prepare(simulated(&bench.mem), &next_build(), boot, bench.sim.now()).unwrap_err();
    let expected = match layout {
        BrokenLayout::ArchiveTargetDirectory => {
            matches!(&error, PrepareError::Dir(DirError::ArchiveExists { session }) if *session == id)
        }
        BrokenLayout::InvalidHeader => {
            matches!(&error, PrepareError::Dir(DirError::ForeignJournal { .. }))
        }
        BrokenLayout::ArchiveFile => matches!(&error, PrepareError::Dir(_)),
    };
    assert!(expected, "{layout:?}: {error:?}");
    assert_eq!(
        bench.mem.contents("/state/events.jsonl"),
        Some(before),
        "{layout:?}"
    );
    assert!(
        bench
            .mem
            .contents(format!("/state/archive/{id}.jsonl"))
            .is_none(),
        "{layout:?} (destination {destination})"
    );
}

/// Directories in every state `prepare` decides on, each with the build the next boot has.
#[derive(Debug, Clone, Copy)]
enum ScenarioKind {
    Empty,
    PlainResume,
    ContextChange,
    BuildArchive,
    ImportUnsettled,
    ImportSettled,
    ResetBoundary,
}

fn scenario(kind: ScenarioKind) -> (Bench, Build) {
    match kind {
        ScenarioKind::Empty => (bench(), build()),
        ScenarioKind::PlainResume => {
            let plain = bench();
            let _ = finished(&plain);
            (plain, build())
        }
        ScenarioKind::ContextChange => {
            let changed = bench();
            let _ = finished(&changed);
            let mut host = changed.sim.seams().host;
            let context = host.bios_context().unwrap();
            changed.sim.set_bios_context(BiosContext {
                bios_version: "other".into(),
                ..context
            });
            (changed, build())
        }
        ScenarioKind::BuildArchive => {
            let archive = bench();
            let _ = finished(&archive);
            (archive, next_build())
        }
        ScenarioKind::ImportUnsettled => {
            let unsettled = bench();
            let _ = finished(&unsettled);
            drop(prepared(&unsettled, &next_build()).journal.close().unwrap());
            (unsettled, next_build())
        }
        ScenarioKind::ImportSettled => {
            let settled = bench();
            let _ = finished(&settled);
            let first = prepared(&settled, &next_build());
            drop(first.journal.close().unwrap());
            let mut input = settled.input_ending_at(settled.short_night());
            input.build = next_build();
            input.import = first.import;
            drop(run(input).journal.close().unwrap());
            (settled, next_build())
        }
        ScenarioKind::ResetBoundary => {
            let reset = bench();
            let _ = finished(&reset);
            let first = prepared(&reset, &next_build());
            drop(first.journal.close().unwrap());
            let mut input = reset.input_ending_at(reset.short_night());
            input.build = next_build();
            input.import = first.import;
            let mut dir = run(input).journal.close().unwrap();
            dir.mark_reset_all().unwrap();
            drop(dir);
            (reset, next_build())
        }
    }
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

/// Everything a preparation leaves: the journal it hands over, its import and the directory's files.
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
        prepared.import.clone(),
        files,
    )
}

#[test_case(ScenarioKind::Empty; "empty")]
#[test_case(ScenarioKind::PlainResume; "plain resume")]
#[test_case(ScenarioKind::ContextChange; "context change")]
#[test_case(ScenarioKind::BuildArchive; "build archive")]
#[test_case(ScenarioKind::ImportUnsettled; "import unsettled")]
#[test_case(ScenarioKind::ImportSettled; "import settled")]
#[test_case(ScenarioKind::ResetBoundary; "reset boundary")]
fn a_kept_journal_is_prepared_as_the_reopened_directory_is(kind: ScenarioKind) {
    let (bench, build) = scenario(kind);
    let rebooted = boot("rebooted");
    let now = bench.sim.now();
    let (closed, kept) = (copy_of(&bench.mem), copy_of(&bench.mem));
    let expected = prepare(simulated(&closed), &build, rebooted.clone(), now).unwrap();

    let mut journal = simulated(&kept).open_at(boot("crashed"), now).unwrap();
    journal.reboot(rebooted.clone()).unwrap();
    kept.record_history();
    let actual = prepare_kept(journal, &build, rebooted, now).unwrap();
    assert_eq!(
        left(&actual, &kept),
        left(&expected, &closed),
        "{kind:?}: the files, events and import differ"
    );
    if matches!(
        kind,
        ScenarioKind::Empty | ScenarioKind::PlainResume | ScenarioKind::ImportUnsettled
    ) {
        assert!(
            kept.reads()
                .iter()
                .all(|path| !path.ends_with("events.jsonl")),
            "{kind:?}: the kept journal was read from its file"
        );
    }
}

#[test]
fn a_kept_journal_this_build_refuses_is_refused_and_left_unchanged() {
    let bench = bench();
    let _ = finished(&bench);
    let older = Build {
        ruleset: 0,
        ..build()
    };
    let before = bench.mem.contents("/state/events.jsonl").unwrap();
    let now = bench.sim.now();
    let mut journal = simulated(&bench.mem).open_at(boot("crashed"), now).unwrap();
    journal.reboot(boot("rebooted")).unwrap();
    let error = prepare_kept(journal, &older, boot("rebooted"), now).unwrap_err();
    assert!(matches!(error, PrepareError::Refused(_)), "{error:?}");
    assert_eq!(bench.mem.contents("/state/events.jsonl"), Some(before));
    assert!(simulated(&bench.mem).archives().unwrap().is_empty());
}

/// An explicit predecessor stamp exercises the transition without changing the production ruleset.
fn predecessor(bench: &Bench) -> (SessionId, Vec<u8>) {
    let mut input = bench.input_ending_at(bench.short_night());
    input.build.ruleset = togi_tuner::RULESET - 1;
    let mut journal = run(input).journal;
    let id = session_of(&journal);
    let bios = journal
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::SessionContext(context) = &event.payload {
                Some(context.bios_context())
            } else {
                None
            }
        })
        .unwrap();
    let stamp = Stamp {
        time: bench.sim.now(),
        mono: None,
    };
    let offsets = journal
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::SessionBaseline(baseline) = &event.payload {
                Some(baseline.offsets)
            } else {
                None
            }
        })
        .unwrap();
    journal
        .append(
            stamp,
            Payload::DayBootStart(togi_journal::DayBootStart {
                build: Build {
                    ruleset: togi_tuner::RULESET - 1,
                    ..build()
                },
                bios,
                offsets,
                watchdog: None,
            }),
            &[],
        )
        .unwrap();
    journal
        .append(
            stamp,
            Payload::DayCheckpoint(togi_journal::DayCheckpoint {
                applied_secs: 60,
                load_mix: togi_journal::LoadMix::default(),
                watchdog: None,
            }),
            &[],
        )
        .unwrap();
    journal
        .append(
            stamp,
            Payload::DayCleanStop(togi_journal::DayCleanStop {}),
            &[],
        )
        .unwrap();
    drop(journal.close().unwrap());
    (id, bench.mem.contents("/state/events.jsonl").unwrap())
}

fn successor_build() -> Build {
    Build {
        ruleset: togi_tuner::RULESET,
        ..build()
    }
}

fn enroll(bench: &Bench) -> Ended {
    let ready = prepared(bench, &successor_build());
    drop(ready.journal.close().unwrap());
    let mut input = bench.input_ending_at(bench.short_night());
    input.build = successor_build();
    input.import = ready.import;
    run(input)
}

#[test]
fn a_ruleset13_session_is_archived_and_its_raw_history_imported_without_permission() {
    let bench = bench();
    let (id, before) = predecessor(&bench);
    let ended = enroll(&bench);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    assert_eq!(
        bench.mem.contents(format!("/state/archive/{id}.jsonl")),
        Some(before)
    );
    let prefix: Vec<_> = ended
        .journal
        .events()
        .iter()
        .take_while(|event| !matches!(event.payload, Payload::CampaignStart(_)))
        .collect();
    assert!(prefix.iter().any(|event| matches!(&event.payload,
        Payload::SessionBaseline(baseline) if baseline.offsets.iter().all(|(_, offset)| offset.0 == 0))));
    assert!(!prefix.iter().any(|event| matches!(
        event.payload,
        Payload::DayApplied(_) | Payload::CcdCap(_) | Payload::NightQualification(_)
    )));
    assert!(
        prefix
            .iter()
            .any(|event| matches!(event.payload, Payload::SuccessorImported(_)))
    );
    let start = ended
        .journal
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::CampaignStart(start) = &event.payload {
                Some(start)
            } else {
                None
            }
        })
        .unwrap();
    assert!(matches!(
        start.origin,
        togi_journal::CampaignOrigin::Successor { .. }
    ));
    assert_eq!(start.history, togi_journal::CampaignHistory::Complete {});
}

#[test]
fn the_successor_import_deduplicates_after_a_crash() {
    let bench = bench();
    predecessor(&bench);
    let ready = prepared(&bench, &successor_build());
    let expected: Vec<_> = ready
        .import
        .as_ref()
        .unwrap()
        .manifest()
        .selected()
        .collect();
    drop(ready.journal.close().unwrap());
    let mut input = bench.input_ending_at(bench.short_night());
    input.build = successor_build();
    input.import = ready.import;
    input.on_append = Some(Box::new(|event| {
        if matches!(event.payload, Payload::DayImported(_)) {
            panic!("interrupted successor import");
        }
    }));
    let interrupted = run(input);
    assert!(interrupted.panic.is_some());
    drop(interrupted.journal.close().unwrap());
    let resumed = enroll(&bench);
    assert!(resumed.errors.is_empty(), "{:?}", resumed.errors);
    let mut originals: Vec<_> = resumed
        .journal
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::DayImported(record) = &event.payload {
                Some(record.original)
            } else {
                None
            }
        })
        .collect();
    originals.sort_by_key(|original| (original.session, original.seq));
    let mut expected = expected;
    expected.sort_by_key(|original| (original.session, original.seq));
    assert_eq!(originals, expected);
    assert_eq!(
        resumed
            .journal
            .events()
            .iter()
            .filter(|event| matches!(event.payload, Payload::SuccessorImported(_)))
            .count(),
        1
    );
    assert_eq!(
        resumed
            .journal
            .events()
            .iter()
            .filter(|event| matches!(event.payload, Payload::CampaignStart(_)))
            .count(),
        1
    );
}

#[test]
fn unreconstructible_campaign_history_blocks_nonzero_exploration() {
    let bench = bench();
    predecessor(&bench);
    let mut bytes = bench.mem.contents("/state/events.jsonl").unwrap();
    bytes.extend_from_slice(b"{\"seq\":9999,\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"old\",\"kind\":\"day.applied\",\"data\":{}}\n");
    bench.mem.put("/state/events.jsonl", bytes);
    let ended = enroll(&bench);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let history = ended
        .journal
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::CampaignStart(start) = &event.payload {
                Some(&start.history)
            } else {
                None
            }
        })
        .unwrap();
    assert!(
        matches!(history, togi_journal::CampaignHistory::Unreconstructible { reason } if !reason.is_empty())
    );
}

/// Applied ancestry, not a saved nonzero profile, makes these events campaign charges.
fn charged_predecessor(bench: &Bench) -> togi_journal::CampaignId {
    use serde_json::json;
    use togi_journal::CcdVector;
    use togi_machine::{CcdId, CcdProfile, TargetOffset};

    predecessor(bench);
    let boot = bench.sim.seams().host.boot_id().unwrap();
    let mut journal = simulated(&bench.mem)
        .open_at(boot.clone(), bench.sim.now())
        .unwrap();
    let topology = match &journal.events()[0].payload {
        Payload::SessionStart(start) => start.cores.clone(),
        payload => panic!("{payload:?}"),
    };
    let campaign = journal
        .events()
        .iter()
        .find_map(|event| match &event.payload {
            Payload::CampaignStart(start) => Some(start.campaign),
            _ => None,
        })
        .unwrap();
    let changed = CcdId::new(0).unwrap();
    let vector = |depth| {
        let ccds: std::collections::BTreeMap<_, _> = topology
            .iter()
            .map(|core| {
                (
                    core.ccd,
                    TargetOffset::new(if core.ccd == changed { depth } else { 0 }).unwrap(),
                )
            })
            .collect();
        CcdVector::expand(CcdProfile::try_from(ccds).unwrap(), &topology).unwrap()
    };
    let old = vector(0);
    let proposed = vector(-5);
    let stamp = Stamp {
        time: bench.sim.now(),
        mono: None,
    };
    let decision = journal.events().last().unwrap().seq.next().unwrap();
    journal
        .append(
            stamp,
            Payload::DayStep(
                serde_json::from_value(json!({
                    "decision": decision, "cold_boot": boot, "old": old,
                    "proposed": proposed, "step": {"kind": "promote", "changed": changed},
                    "hours": [], "hold_s": 0, "causes": {}, "reason": "predecessor promotion"
                }))
                .unwrap(),
            ),
            &[],
        )
        .unwrap();
    journal
        .append(
            stamp,
            Payload::DayApplied(
                serde_json::from_value(json!({
                    "application": 1, "cause": {"kind": "step", "decision": decision},
                    "target": proposed, "readbacks": proposed.cores(),
                    "transactions": [decision], "boot": boot, "mono_ns": 0
                }))
                .unwrap(),
            ),
            &[],
        )
        .unwrap();
    let mut classify = |boot, classification| {
        journal
            .append(
                stamp,
                Payload::DayEvent(
                    serde_json::from_value(json!({
                        "event": {"kind": "boot", "boot": boot},
                        "profile": {"kind": "per_core", "cores": proposed.cores()},
                        "from": stamp.time, "to": stamp.time, "witnesses": [decision],
                        "classification": classification,
                        "resolution": {"kind": "unresolved"},
                        "reason": "predecessor classification", "reproduction": "pending"
                    }))
                    .unwrap(),
                ),
                &[],
            )
            .unwrap();
    };
    classify(
        "provisional-crash",
        json!({"kind": "cpu_suspect", "certainty": "provisional"}),
    );
    classify("provisional-crash", json!({"kind": "non_cpu"}));
    classify(
        "definite-crash",
        json!({"kind": "cpu_suspect", "certainty": "definite"}),
    );
    drop(journal.close().unwrap());
    campaign
}

#[derive(Clone, Copy, Debug)]
enum CampaignCut {
    Uninterrupted,
    SessionImported,
    FirstCharge,
    SecondCharge,
    Reclassified,
    SuccessorImported,
}

#[test_case(false, CampaignCut::Uninterrupted; "transition")]
#[test_case(true, CampaignCut::Uninterrupted; "reset")]
#[test_case(false, CampaignCut::SessionImported; "transition after session imported")]
#[test_case(true, CampaignCut::SessionImported; "reset after session imported")]
#[test_case(false, CampaignCut::FirstCharge; "transition after first charge")]
#[test_case(true, CampaignCut::FirstCharge; "reset after first charge")]
#[test_case(false, CampaignCut::SecondCharge; "transition after second charge")]
#[test_case(true, CampaignCut::SecondCharge; "reset after second charge")]
#[test_case(false, CampaignCut::Reclassified; "transition after reclassification")]
#[test_case(true, CampaignCut::Reclassified; "reset after reclassification")]
#[test_case(false, CampaignCut::SuccessorImported; "transition before campaign start")]
#[test_case(true, CampaignCut::SuccessorImported; "reset before campaign start")]
fn reconstructed_campaign_charges_survive_transition_reset_and_resume(reset: bool, cut: CampaignCut) {
    use togi_journal::{CampaignHistory, Certainty, Classification, DayEventId};
    use togi_tuner::policy::successor::Reconstruction;

    // Two physical cores per CCD let an uninterrupted run schedule neighbour-load work.
    let bench = Bench::defined(&MachineDefinition {
        cores: 4,
        model: ModelDef {
            past_limit_rate: 0.0,
            ..ModelDef::default()
        },
        ..MachineDefinition::default()
    });
    let campaign = charged_predecessor(&bench);
    if reset {
        let mut dir = simulated(&bench.mem);
        assert!(dir.mark_reset_all().unwrap().is_some());
        drop(dir);
    }
    let ready = prepared(&bench, &successor_build());
    let Reconstruction::Complete(expected) = ready.import.as_ref().unwrap().manifest().reconstruction()
    else {
        panic!("predecessor history must reconstruct");
    };
    assert_eq!(expected.campaign, Some(campaign));
    assert_eq!(expected.charges.len(), 2);
    assert_eq!(expected.stops.len(), 1);
    assert_eq!(
        expected.stops.values().next().unwrap().last_clean,
        togi_machine::TargetOffset::ZERO
    );
    drop(ready.journal.close().unwrap());
    let mut input = bench.input_ending_at(bench.short_night());
    input.build = successor_build();
    input.import = ready.import;
    let mut end_night = input.on_append.take().unwrap();
    input.on_append = Some(Box::new(move |event| {
        end_night(event);
        let reached = match cut {
            CampaignCut::Uninterrupted => false,
            CampaignCut::SessionImported => matches!(event.payload, Payload::SessionImported(_)),
            CampaignCut::FirstCharge => {
                matches!(&event.payload, Payload::CampaignCharge(charge) if charge.consumed == 1)
            }
            CampaignCut::SecondCharge => {
                matches!(&event.payload, Payload::CampaignCharge(charge) if charge.consumed == 2)
            }
            CampaignCut::Reclassified => matches!(event.payload, Payload::CampaignReclassified(_)),
            CampaignCut::SuccessorImported => matches!(event.payload, Payload::SuccessorImported(_)),
        };
        assert!(!reached, "interrupted campaign enrollment after {cut:?}");
    }));
    let first = run(input);
    assert_eq!(
        first.panic.is_some(),
        !matches!(cut, CampaignCut::Uninterrupted)
    );
    if !matches!(cut, CampaignCut::Uninterrupted) {
        assert!(!first.journal.events().iter().any(|event| matches!(
            event.payload,
            Payload::CampaignStart(_)
        )));
    }
    let resumed = if matches!(cut, CampaignCut::Uninterrupted) {
        first
    } else {
        drop(first.journal.close().unwrap());
        enroll(&bench)
    };
    assert!(resumed.errors.is_empty(), "{:?}", resumed.errors);
    let events = resumed.journal.events();
    let starts: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::CampaignStart(start) => Some(start),
            _ => None,
        })
        .collect();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].campaign, campaign);
    assert_eq!(starts[0].history, CampaignHistory::Complete {});
    let charges: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::CampaignCharge(charge) => Some(charge),
            _ => None,
        })
        .collect();
    assert_eq!(charges.len(), 2);
    assert_eq!(
        charges
            .iter()
            .map(|charge| (
                charge.campaign,
                charge.event.clone(),
                charge.provisional,
                charge.consumed,
                charge.remaining,
                charge.exhausted,
            ))
            .collect::<Vec<_>>(),
        [
            (
                campaign,
                DayEventId::Boot {
                    boot: "provisional-crash".to_owned().into()
                },
                true, 1, 1, false
            ),
            (
                campaign,
                DayEventId::Boot {
                    boot: "definite-crash".to_owned().into()
                },
                false, 2, 0, true
            ),
        ]
    );
    assert!(charges.iter().all(|charge| {
        charge
            .profile
            .cores
            .iter()
            .any(|(_, offset)| offset == togi_machine::TargetOffset::new(-5).unwrap())
    }));
    let revisions: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::CampaignReclassified(revision) => Some(revision),
            _ => None,
        })
        .collect();
    assert_eq!(revisions.len(), 1);
    assert_eq!(revisions[0].campaign, campaign);
    assert_eq!(revisions[0].event, charges[0].event);
    assert_eq!(
        revisions[0].from,
        Classification::CpuSuspect {
            certainty: Certainty::Provisional
        }
    );
    assert_eq!(revisions[0].to, Classification::NonCpu {});
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.payload, Payload::SuccessorImported(_)))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.payload, Payload::SessionImported(_)))
            .count(),
        1
    );
    drop(resumed.journal.close().unwrap());

    // A fresh transition reconstructs only durable journal bytes, including permanent stops.
    let next = Build {
        ruleset: togi_tuner::RULESET + 1,
        ..build()
    };
    let reopened = prepared(&bench, &next);
    let Reconstruction::Complete(history) = reopened.import.unwrap().manifest().reconstruction() else {
        panic!("enrolled history must remain reconstructible");
    };
    assert_eq!(history.campaign, Some(campaign));
    assert_eq!(history.charges.len(), 2);
    assert_eq!(
        history
            .charges
            .iter()
            .filter(|charge| charge.reclassified)
            .count(),
        1
    );
    assert_eq!(history.stops, expected.stops);
}
