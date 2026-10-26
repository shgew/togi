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
fn a_journal_of_another_ruleset_archives_and_imports_only_its_day_records() {
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
        ["day.boot_start", "day.checkpoint", "day.clean_stop"]
    );
    assert!(manifest.records().iter().all(|r| r.original.session == id));
    assert!(manifest.unknown().is_empty());
    let counts = manifest.commitment().counts;
    assert_eq!(
        (counts.selected, counts.imported, counts.duplicates),
        (3, 3, 0)
    );
    assert_eq!(counts.excluded, 0);
}

#[test]
fn an_imported_record_names_what_its_payload_lacked() {
    let bench = bench();
    finished(&bench);
    let import = prepared(&bench, &next_build()).import.unwrap();
    let records = import.manifest().records();
    let (start, checkpoint, stop) = (&records[0], &records[1], &records[2]);
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
    assert_eq!(import.manifest().records().len(), 3);
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
    assert_eq!(imported, 3);
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
    assert_eq!(manifest.records(), import.manifest().records());
    let counts = manifest.commitment().counts;
    assert_eq!(
        (counts.selected, counts.imported, counts.duplicates),
        (3, 3, 1)
    );
}

#[test]
fn a_transition_out_of_a_committed_import_reads_no_further_back_than_the_committing_session() {
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
    assert_eq!((counts.selected, counts.duplicates), (3, 0));
    assert!(
        manifest
            .records()
            .iter()
            .all(|record| record.original.session == id)
    );
    let sources = manifest.commitment().sources;
    assert_eq!(
        sources
            .iter()
            .map(|source| source.session)
            .collect::<Vec<_>>(),
        [import.destination()]
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
    assert!(prepared.import.is_none());
    assert_eq!(marked(&prepared.journal), [(Marker::ResetAll, id)]);
    assert_eq!(archived(&prepared.journal), [older, id]);
    assert_eq!(
        bench.mem.contents(format!("/state/archive/{id}.jsonl")),
        Some(before)
    );
}

#[test]
fn a_pending_import_the_boundary_passed_is_dropped_without_an_import() {
    let bench = bench();
    let id = finished(&bench);
    let first = prepared(&bench, &next_build());
    let mut dir = first.journal.close().unwrap();
    dir.mark_reset_all().unwrap();
    drop::<StateDir>(dir);
    let after = prepared(&bench, &next_build());
    assert!(after.import.is_none());
    assert_eq!(marked(&after.journal), [(Marker::ResetAll, id)]);
}

#[test]
fn a_reset_boundary_ends_the_walk_so_nothing_before_it_is_imported() {
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
    assert!(
        import
            .manifest()
            .records()
            .iter()
            .all(|record| record.original.session == id)
    );
    assert_eq!(import.manifest().records().len(), 1);
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
    assert_eq!(manifest.records().len(), 3);
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
    assert_eq!(manifest.records().len(), 3);
    assert!(manifest.unknown().is_empty());
    assert!(
        manifest
            .records()
            .iter()
            .all(|record| { record.original.session == id && record.original.seq.get() != 99 })
    );
}

#[test_case(b"\n", true; "terminated")]
#[test_case(b"", false; "torn")]
fn only_a_terminated_commitment_line_ends_the_archive_walk(end: &[u8], committed: bool) {
    let mut bytes = br#"{"seq":1,"time":"2026-01-01T00:00:00Z","boot":"b","kind":"session.imported","data":{}}"#
        .to_vec();
    bytes.extend_from_slice(end);
    assert_eq!(import::commits_import(&bytes), committed);
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
