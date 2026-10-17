use std::collections::{BTreeMap, HashMap};

use togi_journal::{Activity, Decision, Event, Outcome, Seq, TunerDecision};
use togi_machine::{Condition, CoreId, CoreSet, Regime};

use super::Applied;
use super::escalation::{BackoffKind, Strikes};
use super::failed::{Ended, FailedTrial, OpenFailure, Telemetry, effective_top, order_at};
use super::measurements::{Measurement, Measurements};
use super::probe::Probes;
use super::status::SelfSufficiency;
use super::target::{PassRecord, R7Targets};
use crate::BackendId;
use crate::failures::{Pending, together_attribution};
use crate::state::Run;

/// The R7 substate: what request measurements, voltage targets, escalation strikes and
/// self-sufficiency each trial folded, and the multi-core R7 failures still owed a response with
/// the cores already answered.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct R7 {
    pub(crate) measurements: Measurements,
    pub(crate) targets: R7Targets,
    pub(crate) strikes: Strikes,
    pub(crate) status: SelfSufficiency,
    pub(crate) probes: Probes,
    pub(crate) loaded: HashMap<crate::LoadKey, (Seq, crate::Profile)>,
    pub(crate) open: BTreeMap<Seq, OpenFailure>,
    pub(crate) named: BTreeMap<Seq, CoreId>,
}

impl Run {
    /// Folds the end of an R7 trial, live or carried: its measurement, a pass as a target and as
    /// self-sufficiency and a restart of its load's strikes. A failed multi-core trial returns
    /// what its attribution reads, frozen against the measurements folded so far.
    pub(crate) fn r7_trial(
        &mut self,
        ended: Ended<'_>,
        outcome: Outcome,
        backend: BackendId,
    ) -> Option<FailedTrial> {
        let Ended {
            seq,
            class,
            profile,
            telemetry,
            ..
        } = ended;
        if class.regime() != Regime::R7 {
            return None;
        }
        let order = order_at(
            &self.r7.measurements,
            &self.topo,
            class,
            profile,
            seq,
            telemetry.requests,
        );
        match outcome {
            Outcome::Pass => {
                self.r7.strikes.pass(class.load());
                self.r7.loaded.remove(&class.load());
                self.r7.status.pass(
                    backend,
                    class.workload(),
                    class.loaded(),
                    effective_top(telemetry.top, &order),
                    profile,
                );
                self.r7.targets.insert(
                    &self.topo,
                    &self.evidence,
                    &PassRecord {
                        seq,
                        class,
                        backend,
                        profile: *profile,
                        requests: *telemetry.requests,
                        ccd_mhz: telemetry
                            .clocks
                            .iter()
                            .map(|(&ccd, &mhz)| (ccd, i64::from(mhz)))
                            .collect(),
                    },
                );
            }
            Outcome::Inconclusive | Outcome::Failure => {}
        }
        let failed = (outcome == Outcome::Failure && class.multi_r7()).then(|| {
            let mut failed = FailedTrial::freeze(&self.r7.measurements, &self.topo, ended, order);
            for (ccd, source, mhz) in self.r7.measurements.clocks(class.load(), backend) {
                if let std::collections::btree_map::Entry::Vacant(entry) = failed.clocks.entry(ccd)
                {
                    entry.insert(mhz);
                    failed.clock_sources.insert(ccd, source);
                }
            }
            failed
        });
        self.r7
            .measurements
            .record_clocks(class.load(), backend, seq, telemetry.clocks);
        if !telemetry.requests.is_empty() {
            self.r7.measurements.record(
                class.load(),
                Measurement::new(seq, *profile, *telemetry.requests),
            );
            self.refresh_round_checks();
        }
        failed
    }

    /// Folds the end of the open trial for R7 and the zero diagnostic: the frozen failed trial of
    /// a failed multi-core R7 end, and the failure the trial was the all-zero rerun of.
    pub(crate) fn end_r7_and_zero(
        &mut self,
        seq: Seq,
        open: &crate::flight::OpenTrial,
        end: &togi_journal::TrialEnd,
        cited: CoreSet,
    ) -> (Option<FailedTrial>, Option<Seq>) {
        let (_, named, _) =
            together_attribution(end.core, cited, &open.profile, open.class.multi_r7());
        let failed = self.r7_trial(
            Ended {
                seq,
                class: open.class,
                profile: &open.profile,
                telemetry: Telemetry {
                    requests: &end.voltage_requests_v,
                    top: end.top_requesters,
                    clocks: &end.ccd_mhz,
                    stalled: end.stalled_core,
                },
                named,
                crash_duration_s: (open.crashed || end.signal == Some(togi_machine::Signal::Crash))
                    .then_some(end.duration_s),
            },
            end.outcome,
            open.backend,
        );
        let zero = self.zero.end(&end.trial, seq, end.outcome);
        if let (Some(failure), Outcome::Pass) = (zero, end.outcome) {
            self.zero_passed(failure);
        }
        (failed, zero)
    }

    /// A multi-core R7 failure that can be answered, carried or live and not a parked rerun's, is
    /// owed a response until its charged cores are handled.
    pub(crate) fn r7_open_failure(
        &mut self,
        seq: Seq,
        pending: Pending,
        trial: Option<FailedTrial>,
    ) {
        let answerable = pending.class.multi_r7()
            && (pending.carried || pending.failure.condition != Some(Condition::Parked));
        if let (true, Some(r7)) = (answerable, trial) {
            self.r7.open.insert(
                seq,
                OpenFailure {
                    seq,
                    pending,
                    r7,
                    handled: CoreSet::default(),
                },
            );
        }
    }

    /// A backoff decision answers the multi-core R7 failure it cites first: the core it moved and
    /// the cores its CCD charged are handled, and a backoff of the unattributed kind counts toward
    /// the load's escalation. Deepening backoffs and named charges do not count.
    pub(crate) fn r7_decided(&mut self, event: &Event, decision: &TunerDecision) {
        if !matches!(decision.decision, Decision::Backoff | Decision::Hold) {
            return;
        }
        let Some(record) = event.cause.iter().find_map(|seq| self.r7.open.get(seq)) else {
            return;
        };
        let seq = record.seq;
        let load = record.pending.class.load();
        let recorded_name = record.pending.failure.core;
        let view = self.failed(record);
        let named = self.named_culprit(&view);
        let targets = self.failure_targets(&view);
        let located_named = self
            .hunts
            .located
            .get(&seq)
            .and_then(|located| located.end.as_ref())
            .is_some_and(|end| end.named.is_some());
        let probe = (!named && !located_named)
            .then(|| self.r7_probe_part(&view, decision.core))
            .flatten();
        if decision.activity != Activity::Deepening && decision.decision == Decision::Backoff {
            let kind = if located_named {
                BackoffKind::LocatedNamed
            } else if named {
                BackoffKind::Named
            } else {
                BackoffKind::Unattributed
            };
            self.r7.strikes.backoff(load, kind, event.seq, seq);
            if let Some((class, _, stalled_elsewhere)) = probe {
                self.r7.probes.charge(
                    (load, decision.core),
                    class,
                    event.seq,
                    seq,
                    stalled_elsewhere,
                );
            }
            if let (false, Some(core)) = (named || located_named, recorded_name) {
                self.r7.named.insert(seq, core);
            }
        }
        let ccd = self.topo.ccd_of(decision.core);
        let marked: Vec<CoreId> = targets
            .iter()
            .copied()
            .filter(|&core| named || self.topo.ccd_of(core) == ccd)
            .collect();
        let Some(open) = self.r7.open.get_mut(&seq) else {
            return;
        };
        open.handled.insert(decision.core);
        open.handled.extend(marked);
        if located_named || targets.iter().all(|&core| open.handled.contains(core)) {
            self.r7.open.remove(&seq);
            self.hunts.located.remove(&seq);
        }
    }

    /// A reset of `core` discards the measurements, passes and self-sufficiency of every load
    /// that includes it, the strikes of those loads and of failures that named it, and the
    /// answers owed to the failures it discards.
    pub(crate) fn r7_reset(&mut self, core: CoreId) {
        self.r7.measurements.reset_core(core);
        self.refresh_round_checks();
        self.r7.targets.reset_core(core);
        self.r7.status.reset_core(core);
        let named = &self.r7.named;
        self.r7.probes.reset_core(core);
        self.r7.loaded.clear();
        let named_core = |seq: Seq| named.get(&seq).copied();
        self.r7.strikes.reset_core(core, named_core);
        self.r7.open.retain(|_, open| {
            let class = open.pending.class;
            !(class.loaded().contains(core) || open.pending.failure.core == Some(core))
        });
        for open in self.r7.open.values_mut() {
            open.r7.backoffs.forget(|seq| named_core(seq) == Some(core));
        }
        self.r7.named.retain(|_, &mut named| named != core);
        let failures = &self.failures;
        let open = &self.r7.open;
        self.zero
            .retain(|seq| failures.pending(seq).is_some() || open.contains_key(&seq));
    }

    /// A loaded result answers only this exact load while its unloaded tuning offsets stay
    /// unchanged. A later loaded-core move cannot change what the locate tested at CO 0.
    pub(crate) fn r7_located_loaded(&mut self, source: Seq, end: Seq) {
        if let Some(record) = self.r7.open.get(&source) {
            self.r7
                .loaded
                .insert(record.pending.class.load(), (end, record.pending.profile));
        }
    }

    pub(crate) fn r7_offset_changed(
        &mut self,
        core: CoreId,
        before: togi_machine::TargetOffset,
        after: togi_machine::TargetOffset,
    ) {
        if before != after {
            self.r7.loaded.retain(|load, _| load.loaded.contains(core));
        }
    }

    pub(crate) fn r7_loaded_verdict(
        &self,
        load: crate::LoadKey,
        profile: &crate::Profile,
    ) -> Option<Seq> {
        let (seq, located) = self.r7.loaded.get(&load)?;
        self.topo
            .all()
            .iter()
            .filter(|&core| !load.loaded.contains(core))
            .all(|core| profile.at(core) == located.at(core))
            .then_some(*seq)
    }
}
