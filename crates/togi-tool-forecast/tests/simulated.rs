//! Forecasts over journals of whole simulated sessions: the real tuner decides when a run has concluded.
#![expect(clippy::unwrap_used, reason = "helpers panic on a broken fixture")]

use std::io;
use std::num::NonZeroU32;

use jiff::tz::TimeZone;
use togi_config::Config;
use togi_journal::{
    Build, CycleEvent, Event, MemFs, Payload, SCHEMA, Seq, ShutdownReason, StateDir,
};
use togi_machine::CancellationToken;
use togi_render::Renderer;
use togi_session::{ConfigSource, Ended, PublicationPolicy, RunInput, Stop, StopPolicy, run};
use togi_sim::definition::ModelDef;
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};
use togi_tool_forecast::{Anchor, Session, Status, after, read_dir};
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

struct Simulated {
    mem: MemFs,
    ended: Ended,
}

impl Simulated {
    fn events(&self) -> &[Event] {
        self.ended.journal.events()
    }

    fn session(&self) -> Session {
        Session::from_events(self.events().to_vec()).unwrap()
    }
}

/// A two-core machine that never fails, so a session runs to its stop without a crash or a reboot.
fn simulate(cycles: u32) -> Simulated {
    let definition = MachineDefinition {
        cores: 2,
        model: ModelDef {
            past_limit_rate: 0.0,
            crash_mce: 0.0,
            ..ModelDef::default()
        },
        ..MachineDefinition::default()
    };
    let sim = Simulator::new(
        Model::new(&definition, 1).unwrap(),
        RunSettings {
            seed: 1,
            ..RunSettings::default()
        },
    );
    let mem = MemFs::new();
    let mut machine = sim.seams();
    let boot = machine.host.boot_id().unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "the journal of a simulation stays in memory"
    )]
    let dir = StateDir::simulated(mem.clone()).unwrap();
    let journal = dir.open_at(boot.clone(), sim.now()).unwrap();
    let ended = run(RunInput {
        config: Config::parse(r#"{"start_offsets":{"0":-10,"1":-10}}"#).unwrap(),
        source: ConfigSource::default(),
        build: Build {
            version: "0.0.0".into(),
            rev: None,
            ruleset: RULESET,
            schema: SCHEMA,
            fixes: 0,
            evidence: EVIDENCE_EPOCH,
        },
        boot,
        journal,
        machine,
        policy: StopPolicy {
            cycles: NonZeroU32::new(cycles),
            ..StopPolicy::default()
        },
        publication: PublicationPolicy::OnFinish,
        bootloader: None,
        prompt: None,
        defects: &[],
        resume: None,
        carry: None,
        log: None,
        renderer: Renderer::default(),
        zone: TimeZone::UTC,
        cancel: CancellationToken::new(),
        report: Box::new(io::sink()),
    });
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    Simulated { mem, ended }
}

#[test]
fn a_run_that_goes_on_past_its_conclusion_scores_as_the_cycles_one_run_with_the_same_history() {
    let stopped = simulate(1);
    assert_eq!(stopped.ended.stop, Some(Stop::Cycles));
    let longer = simulate(2);
    let diverged = stopped
        .events()
        .iter()
        .zip(longer.events())
        .position(|(a, b)| a.payload != b.payload)
        .unwrap();
    assert!(matches!(
        &longer.events()[diverged].payload,
        Payload::CheckingCycle(c) if c.event == CycleEvent::Start
    ));
    assert!(stopped.events()[diverged..].iter().any(|event| {
        matches!(&event.payload, Payload::Shutdown(s) if s.reason == ShutdownReason::Cycles)
    }));
    let anchor_seq = stopped.events()[stopped.events().len() / 2].seq;
    let anchor = stopped.session().anchor_at(anchor_seq).unwrap();
    let want = after(&[stopped.session()], &anchor).unwrap();
    let got = after(&[longer.session()], &anchor).unwrap();
    assert_eq!(want.status, Status::Concluded);
    assert_eq!(got, want);
    assert!(want.hours > 0.0);
    assert_eq!(
        want.depth,
        want.profile.iter().map(|(_, o)| o.0).sum::<i64>()
    );
    let terminal = stopped
        .events()
        .iter()
        .position(|event| {
            matches!(&event.payload, Payload::Shutdown(s) if s.reason == ShutdownReason::Cycles)
        })
        .unwrap();
    let recorded: Vec<_> = stopped.events()[..terminal]
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            Payload::ProfileChange(change) => Some(change.to),
            _ => None,
        })
        .unwrap()
        .iter()
        .map(|(core, offset)| (core, offset.observed()))
        .collect();
    let projected: Vec<_> = want.profile.iter().collect();
    assert_eq!(projected, recorded);
    let configured: Vec<_> = want.profile.iter().map(|_| -10).collect();
    assert_ne!(
        want.profile.iter().map(|(_, o)| o.0).collect::<Vec<_>>(),
        configured,
        "the checking profile goes past the start offsets, which a baseline would not"
    );
}

#[test]
fn a_simulated_state_directory_reads_back_as_the_sessions_it_holds() {
    let stopped = simulate(1);
    let dir = tempfile::tempdir().unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "writing a simulation's files to disk is the point"
    )]
    stopped.mem.dump_to(dir.path()).unwrap();
    let state = dir.path().join("state");
    let sessions = read_dir(&state, None).unwrap();
    assert_eq!(sessions.len(), 1);
    assert!(sessions[0].replayable());
    let last = stopped.events().last().unwrap().seq;
    let anchor = Anchor::of(&state).unwrap();
    assert_eq!(anchor.seq, last);
    assert_eq!(
        stopped.session().anchor_at(last),
        Some(anchor.clone()),
        "the journal's lines are the events' own encodings"
    );
    let first = Seq::new(2).unwrap();
    let early = stopped.session().anchor_at(first).unwrap();
    let outcome = after(&sessions, &early).unwrap();
    assert_eq!(outcome.status, Status::Concluded);
}
