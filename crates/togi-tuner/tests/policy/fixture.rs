use std::collections::BTreeMap;

use jiff::Timestamp;
use togi_journal::{
    Build, DayBootStart, Event, FixStamp, Payload, SCHEMA, Seq, SessionBaseline, SessionContext,
    SessionStart, Stamp, TrialEnd, TrialId, TrialStart, WorkloadContent,
};
use togi_machine::{
    Backend, BootId, CcdId, CcdProfile, ContentId, CoreId, CoreInfo, CpuId, Digest, ObservedOffset,
    PerCore, TargetOffset,
};
use togi_tuner::policy::{
    Action, Contents, DayQuery, EndReason, NightQuery, NotReady, Params, State, TrialPlan,
};

pub(crate) const START: i64 = 1_700_000_000;

pub(crate) fn at(second: i64) -> Timestamp {
    Timestamp::from_second(START + second).unwrap()
}

fn build() -> Build {
    Build {
        version: "0.1.0".into(),
        rev: None,
        ruleset: 13,
        schema: SCHEMA,
        fixes: FixStamp::none(),
        evidence: 1,
    }
}

pub(crate) fn ccd(id: u8) -> CcdId {
    CcdId::try_from(id).unwrap()
}

/// The all-zero profile of both CCDs.
pub(crate) fn zero() -> CcdProfile {
    CcdProfile::zero([ccd(0), ccd(1)]).unwrap()
}

/// Each core's observed offset under `profile`.
pub(crate) fn offsets_of(profile: &CcdProfile) -> PerCore<ObservedOffset> {
    cores()
        .into_iter()
        .map(|info| {
            let offset = profile.get(info.ccd).map_or(0, TargetOffset::get);
            (info.core, ObservedOffset(offset))
        })
        .collect()
}

fn context() -> SessionContext {
    firmware("1")
}

/// The session context under firmware `bios_version`.
pub(crate) fn firmware(bios_version: &str) -> SessionContext {
    SessionContext {
        bios_version: bios_version.into(),
        board: "board".into(),
        cpu_model: "cpu".into(),
        microcode: "mc".into(),
        boost_limit_mhz: 5000,
    }
}

/// The machine's four cores, two per CCD, each with two CPUs.
pub(crate) fn cores() -> Vec<CoreInfo> {
    (0..4_u8)
        .map(|id| CoreInfo {
            core: CoreId::new(i64::from(id)).unwrap(),
            ccd: CcdId::try_from(id / 2).unwrap(),
            cpus: vec![
                CpuId::new(i64::from(id)).unwrap(),
                CpuId::new(i64::from(id) + 32).unwrap(),
            ],
        })
        .collect()
}

/// A journal and the policy state folded from it as it grows.
pub(crate) struct Journal {
    pub(crate) events: Vec<Event>,
    pub(crate) state: State,
    /// The boot new events are recorded in.
    boot: String,
    /// Seconds every later event's time is shifted by, as after the days `advance` skipped.
    shift: i64,
}

impl Journal {
    /// An empty journal folded by a policy with `params`.
    pub(crate) fn bare_with(params: Params) -> Self {
        Self {
            events: Vec::new(),
            state: State::new(params),
            boot: "boot".to_owned(),
            shift: 0,
        }
    }

    pub(crate) fn append(&mut self, payload: Payload, cause: &[Seq]) -> Seq {
        let boot = self.boot.clone();
        self.append_in(&boot, payload, cause)
    }

    /// Records every later event in `boot`, as after a reboot.
    pub(crate) fn reboot(&mut self, boot: &str) {
        boot.clone_into(&mut self.boot);
    }

    /// Moves the clock of every later event `secs` ahead.
    pub(crate) fn advance(&mut self, secs: i64) {
        self.shift += secs;
    }

    /// The time just after the latest event.
    pub(crate) fn now(&self) -> Timestamp {
        at(i64::try_from(self.events.len()).unwrap() + 1 + self.shift)
    }

    /// Appends `payload` as recorded in `boot`.
    fn append_in(&mut self, boot: &str, payload: Payload, cause: &[Seq]) -> Seq {
        let seq = Seq::new(self.events.len() as u64 + 1).unwrap();
        let event = Event::new(
            seq,
            Stamp {
                time: at(i64::try_from(seq.get()).unwrap() + self.shift),
                mono: None,
            },
            BootId::from(boot.to_owned()),
            cause.to_vec(),
            payload,
        );
        self.state.fold(&event).unwrap();
        self.events.push(event);
        seq
    }

    pub(crate) fn started() -> Self {
        Self::started_with(Params::SPEC)
    }

    fn started_with(params: Params) -> Self {
        let mut journal = Self::bare_with(params);
        journal.append(
            SessionStart {
                build: build(),
                session: "20231114T221320Z".parse().unwrap(),
                cores: cores(),
            }
            .into(),
            &[],
        );
        journal
    }

    pub(crate) fn contextual() -> Self {
        Self::contextual_with(Params::SPEC)
    }

    fn contextual_with(params: Params) -> Self {
        let mut journal = Self::started_with(params);
        journal.append(context().into(), &[]);
        journal
    }

    /// Reboots into the normal boot `boot` and appends its observer start, at all-zero offsets.
    pub(crate) fn boot_start(&mut self, boot: &str) -> Seq {
        self.boot_start_at(boot, "1", &zero())
    }

    /// Reboots into the normal boot `boot` under firmware `bios_version` and appends its observer start,
    /// each core at its CCD's offset in `profile`.
    pub(crate) fn boot_start_at(
        &mut self,
        boot: &str,
        bios_version: &str,
        profile: &CcdProfile,
    ) -> Seq {
        self.reboot(boot);
        self.append(
            DayBootStart {
                build: build(),
                bios: firmware(bios_version).bios_context(),
                offsets: offsets_of(profile),
                watchdog: None,
            }
            .into(),
            &[],
        )
    }

    /// Asks the day query until it owes nothing, committing each decision, and returns what it committed.
    pub(crate) fn settle_day(&mut self, query: &DayQuery) -> Vec<Payload> {
        let mut committed = Vec::new();
        for _ in 0..50 {
            let owed = self.state.day(query).unwrap();
            if owed.is_empty() {
                return committed;
            }
            for decision in owed {
                let payload: Payload = decision.payload.into();
                committed.push(payload.clone());
                self.append(payload, &decision.causes);
            }
        }
        panic!("the day kept deciding: {committed:?}");
    }

    pub(crate) fn ready() -> Self {
        Self::ready_with(Params::SPEC)
    }

    /// A journal ready for its first night, folded by a policy with `params`.
    pub(crate) fn ready_with(params: Params) -> Self {
        let mut journal = Self::contextual_with(params);
        journal.append(
            SessionBaseline {
                offsets: offsets_of(&zero()),
            }
            .into(),
            &[],
        );
        journal
    }

    pub(crate) fn next(&self, now: Timestamp) -> Result<Action, NotReady> {
        let contents = Contents::new(std::iter::empty());
        self.state.next(&NightQuery {
            now,
            contents: &contents,
            unavailable: &BTreeMap::new(),
        })
    }

    /// Appends what a `Decide` action says to commit.
    pub(crate) fn commit(&mut self, action: Action) -> Seq {
        let Action::Decide(decision) = action else {
            panic!("expected a decision, got {action:?}");
        };
        self.append(decision.payload.into(), &decision.causes)
    }
}

fn content() -> ContentId {
    ContentId {
        backend: Backend::Mprime,
        executable: Digest::from_bytes([1; 32]),
        configuration: Digest::from_bytes([2; 32]),
    }
}

/// Every workload of every regime, as one build of the backends.
pub(crate) fn contents() -> Contents {
    Contents::new(togi_machine::ALL_REGIMES.into_iter().flat_map(|regime| {
        togi_machine::workloads(regime)
            .iter()
            .map(|w| (w.id(), content()))
    }))
}

/// What a driven trial ends as.
#[derive(Clone, Copy)]
pub(crate) enum Ending {
    Pass,
    Interrupted,
    /// A conclusive failure, on `core` when it names one.
    Fail(Option<u8>),
}

impl Journal {
    /// Journals one trial of `plan`: intent, start and an end as `ending` says, as long as intended.
    pub(crate) fn run(&mut self, plan: &TrialPlan, ending: Ending) -> Seq {
        self.run_built(plan, ending, &content())
    }

    /// Journals one trial of `plan` whose every workload ran the backend build `built`.
    pub(crate) fn run_built(&mut self, plan: &TrialPlan, ending: Ending, built: &ContentId) -> Seq {
        self.run_from(plan, ending, built, None)
    }

    /// Journals one passing trial of `plan` whose task began `waited` seconds before its intent, as when the
    /// session prepared the profile and waited for a backend first.
    pub(crate) fn run_after_wait(&mut self, plan: &TrialPlan, waited: i64) -> Seq {
        let intent_at = at(i64::try_from(self.events.len()).unwrap() + 1);
        let task_start = intent_at
            .checked_sub(jiff::SignedDuration::from_secs(waited))
            .unwrap();
        self.run_from(plan, Ending::Pass, &content(), Some(task_start))
    }

    fn run_from(
        &mut self,
        plan: &TrialPlan,
        ending: Ending,
        built: &ContentId,
        task_start: Option<Timestamp>,
    ) -> Seq {
        let id = TrialId::from(format!("t{}", self.events.len() + 1));
        let content = plan
            .workloads()
            .into_iter()
            .map(|workload| WorkloadContent {
                workload,
                content: built.clone(),
            })
            .collect();
        let cores = cores();
        let mut intent = plan.intent(id.clone(), &cores, content).unwrap();
        intent.task_start = task_start;
        let duration_s = intent.duration_s;
        let intent = self.append(intent.into(), &plan.causes);
        let mut start: TrialStart = serde_json::from_value(serde_json::json!({
            "trial": id.as_str(), "scope": "scope", "pid": 1, "cpus": [], "argv": []
        }))
        .unwrap();
        start.roles = plan.roles(1, &cores).unwrap();
        self.append(start.into(), &[intent]);
        let (outcome, interrupted, core) = match ending {
            Ending::Pass => ("pass", false, None),
            Ending::Interrupted => ("failure", true, None),
            Ending::Fail(core) => ("failure", false, core),
        };
        let mut end = serde_json::json!({
            "trial": id.as_str(), "outcome": outcome, "duration_s": duration_s,
            "interrupted": interrupted
        });
        if let Some(core) = core {
            end["core"] = serde_json::json!(core);
        }
        let end: TrialEnd = serde_json::from_value(end).unwrap();
        self.append(end.into(), &[intent])
    }

    /// Drives the night until it asks for a trial, committing what it owes on the way.
    pub(crate) fn until_trial(&mut self, now: Timestamp) -> TrialPlan {
        match self.until_end(now, &BTreeMap::new()) {
            Ok(plan) => plan,
            Err(reason) => panic!("the night ended: {reason:?}"),
        }
    }

    /// Drives the night, each trial ending as `ending` says, until it asks for its first filler trial.
    pub(crate) fn until_filler(&mut self, ending: impl Fn(&TrialPlan) -> Ending) -> TrialPlan {
        for step in 0..3_500_i64 {
            let plan = match self.until_end(at(100 + step), &BTreeMap::new()) {
                Ok(plan) => plan,
                Err(reason) => panic!("the night ended at trial {step}: {reason:?}"),
            };
            if plan.task.queue == togi_journal::Queue::Filler {
                return plan;
            }
            self.run(&plan, ending(&plan));
        }
        panic!("the filler never ran");
    }

    /// Drives the night with `unavailable` excluded until it asks for a trial or ends.
    pub(crate) fn until_end(
        &mut self,
        now: Timestamp,
        unavailable: &BTreeMap<Backend, Vec<Seq>>,
    ) -> Result<TrialPlan, EndReason> {
        let contents = contents();
        for _ in 0..200 {
            let action = self
                .state
                .next(&NightQuery {
                    now,
                    contents: &contents,
                    unavailable,
                })
                .unwrap();
            match action {
                Action::Decide(_) => {
                    self.commit(action);
                }
                Action::RunTrial(plan) => return Ok(plan),
                Action::EndNight(reason) => return Err(reason),
            }
        }
        panic!("the night kept deciding");
    }
}
