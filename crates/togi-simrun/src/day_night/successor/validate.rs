#![expect(
    clippy::float_cmp,
    reason = "record boundaries and copied witnesses must agree exactly"
)]

use std::collections::BTreeSet;

use togi_machine::PerCore;

use super::{
    Classification, EdgeObservation, FinishReason, Outcome, Period, Phase, Qualification, Record,
    RestorationOutcome, RestorationResult, RiskSource, SourceAccount, SourceRisk, StepIdentity,
    StepKind, TruthCause, UnitMode, UnitPhase,
};

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("decode successor record")]
    Decode(#[from] serde_json::Error),
    #[error("validate successor record: {0}")]
    Invalid(&'static str),
}

type Result<T, E = ValidationError> = std::result::Result<T, E>;

fn require(condition: bool, operation: &'static str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(ValidationError::Invalid(operation))
    }
}

fn nonnegative(value: f64) -> bool {
    value.is_finite() && value >= 0.0
}
fn day(value: f64) -> bool {
    value.is_finite() && value >= 1.0
}

fn unique_text<'a>(values: impl IntoIterator<Item = &'a str>) -> bool {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .all(|value| !value.is_empty() && seen.insert(value))
}

pub(super) fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

impl Record {
    /// Validates topology, interval coverage, event identities and independently reproducible
    /// risk/allowance accounts. Policy permission rules remain the scorer's separate contract.
    pub fn validate(&self) -> Result<()> {
        self.validate_provenance()?;
        self.validate_epochs()?;
        self.validate_units()?;
        self.validate_intervals()?;
        self.validate_events()?;
        self.validate_steps()?;
        self.validate_accounts()?;
        self.validate_observation()?;
        Ok(())
    }

    fn validate_provenance(&self) -> Result<()> {
        let p = &self.provenance;
        require(
            !p.registration_id.is_empty()
                && !p.source_revision.is_empty()
                && !p.simulator_revision.is_empty()
                && !p.runner_revision.is_empty()
                && !p.report_revision.is_empty()
                && !p.policy_identity.is_empty()
                && !p.scenario.is_empty(),
            "provenance identities",
        )?;
        require(
            sha256(&p.registration_sha256)
                && sha256(&p.policy_source_sha256)
                && sha256(&p.parameters_sha256)
                && p.binary_sha256.as_deref().is_none_or(sha256)
                && p.input_sha256.as_deref().is_none_or(sha256),
            "provenance hashes",
        )?;
        require(p.slope.is_finite() && p.slope > 1.0, "slope")?;
        require(
            !p.topology.is_empty()
                && !p.membership.is_empty()
                && p.membership.len() <= p.topology.len(),
            "topology dimensions",
        )?;
        let mut cores = BTreeSet::new();
        let mut cpus = BTreeSet::new();
        for core in &p.topology {
            require(
                cores.insert(core.core)
                    && !core.cpus.is_empty()
                    && core.cpus.iter().all(|cpu| cpus.insert(*cpu)),
                "topology core/CPU identities",
            )?;
        }
        let mut covered = BTreeSet::new();
        let mut ccds = BTreeSet::new();
        for (index, unit) in p.membership.iter().enumerate() {
            require(
                unit.unit.index() == index && !unit.cores.is_empty(),
                "dense unit membership",
            )?;
            require(
                unit.cores
                    .iter()
                    .all(|core| cores.contains(core) && covered.insert(*core)),
                "unit partition",
            )?;
            match p.unit_mode {
                UnitMode::CcdUniform => {
                    require(
                        unit.kind == UnitPhase::Coarse && unit.parent.is_none(),
                        "coarse unit identity",
                    )?;
                    let first = p
                        .topology
                        .iter()
                        .find(|core| core.core == unit.cores[0])
                        .ok_or(ValidationError::Invalid("unit topology"))?;
                    require(
                        ccds.insert(first.ccd)
                            && p.topology
                                .iter()
                                .filter(|core| unit.cores.contains(&core.core))
                                .all(|core| core.ccd == first.ccd),
                        "CCD-uniform membership",
                    )?;
                }
                UnitMode::PerCore => require(
                    unit.kind == UnitPhase::Core && unit.cores.len() == 1,
                    "per-core membership",
                )?,
            }
        }
        require(covered == cores, "topology coverage")?;
        require(
            self.policy.unit_membership == p.membership,
            "policy membership",
        )?;
        if let Some(readback) = &self.policy.readback {
            self.validate_profile(readback)?;
        }
        Ok(())
    }

    fn validate_profile(&self, profile: &PerCore<u8>) -> Result<()> {
        require(
            profile.len() == self.provenance.topology.len()
                && self
                    .provenance
                    .topology
                    .iter()
                    .all(|core| profile.get(core.core).is_some_and(|depth| depth <= 50)),
            "physical-core profile dimensions/write range",
        )?;
        for unit in &self.provenance.membership {
            let first = unit
                .cores
                .first()
                .and_then(|core| profile.get(*core))
                .ok_or(ValidationError::Invalid("unit profile membership"))?;
            require(
                unit.cores
                    .iter()
                    .all(|core| profile.get(*core) == Some(first)),
                "nonuniform unit profile",
            )?;
        }
        Ok(())
    }

    fn validate_epochs(&self) -> Result<()> {
        require(
            !self.epochs.is_empty()
                && unique_text(self.epochs.iter().map(|epoch| epoch.id.as_str())),
            "epoch identities",
        )?;
        let mut end = 1.0;
        let count = self.units.len();
        for epoch in &self.epochs {
            require(
                day(epoch.start_day)
                    && epoch.start_day == end
                    && epoch.end_day.is_finite()
                    && epoch.end_day > epoch.start_day
                    && !epoch.context.is_empty(),
                "ordered epoch schedule",
            )?;
            let reference = &epoch.reference;
            let unit_count =
                u32::try_from(count).map_err(|_| ValidationError::Invalid("unit count"))?;
            require(
                reference.operational_max == reference.certified_upper_bound
                    && reference.operational_max <= 45 * unit_count
                    && reference.physical_max >= reference.operational_max
                    && reference.physical_max <= 50 * unit_count
                    && reference.per_unit_max_feasible.len() == count
                    && reference
                        .per_unit_max_feasible
                        .iter()
                        .all(|depth| *depth <= 45)
                    && !reference.certificate.is_empty()
                    && nonnegative(reference.tolerance)
                    && reference.tolerance < 1.0,
                "certified depth reference",
            )?;
            require(
                reference.operational_max
                    <= reference
                        .per_unit_max_feasible
                        .iter()
                        .map(|depth| u32::from(*depth))
                        .sum(),
                "reference objective bounds",
            )?;
            end = epoch.end_day;
        }
        Ok(())
    }

    fn validate_units(&self) -> Result<()> {
        require(
            self.units.len() == self.provenance.membership.len(),
            "unit summary dimensions",
        )?;
        for (index, summary) in self.units.iter().enumerate() {
            require(
                summary.unit.index() == index
                    && nonnegative(summary.hold_hours)
                    && nonnegative(summary.qualification_hours)
                    && nonnegative(summary.night_hours)
                    && summary.qualification_hours <= summary.night_hours,
                "unit summary",
            )?;
            if let Some(finish) = &summary.finish {
                require(
                    finish.unit == summary.unit
                        && day(finish.day)
                        && self.observes_instant(finish.day)
                        && !finish.witnesses.is_empty()
                        && unique_text(finish.witnesses.iter().map(String::as_str)),
                    "finish witness",
                )?;
            }
            for ceiling in &summary.ceiling_evidence {
                require(
                    day(ceiling.day)
                        && self.epochs.iter().any(|epoch| epoch.id == ceiling.epoch)
                        && ceiling.ceiling_depth <= 45
                        && !ceiling.witnesses.is_empty()
                        && unique_text(ceiling.witnesses.iter().map(String::as_str)),
                    "ceiling evidence",
                )?;
                let expected = match ceiling.edge {
                    EdgeObservation::Measured { u_depth } => {
                        require(u_depth <= 50, "measured edge")?;
                        u_depth.saturating_sub(5)
                    }
                    EdgeObservation::CensoredWriteLimit {} => 45,
                };
                require(ceiling.ceiling_depth == expected, "five-count ceiling")?;
            }
        }
        require(
            self.policy
                .finishes
                .iter()
                .eq(self.units.iter().filter_map(|unit| unit.finish.as_ref())),
            "policy finish report",
        )?;
        let mut stopped = BTreeSet::new();
        require(
            self.policy
                .stopped_units
                .iter()
                .all(|unit| unit.index() < self.units.len() && stopped.insert(*unit)),
            "stopped unit identities",
        )?;
        let mut inherited = BTreeSet::new();
        for finish in &self.policy.inherited_stops {
            require(
                self.provenance.unit_mode == UnitMode::PerCore
                    && inherited.insert(finish.unit)
                    && self
                        .provenance
                        .membership
                        .iter()
                        .any(|unit| unit.parent == Some(finish.unit))
                    && day(finish.day)
                    && self.observes_instant(finish.day)
                    && finish.reason != FinishReason::Ceiling
                    && !finish.witnesses.is_empty()
                    && unique_text(finish.witnesses.iter().map(String::as_str)),
                "inherited coarse-stop witnesses",
            )?;
        }
        Ok(())
    }

    fn validate_intervals(&self) -> Result<()> {
        let until = self.outcome.observed_until_day();
        let mut end = 1.0;
        for interval in &self.intervals {
            require(
                interval.start_day == end
                    && day(interval.start_day)
                    && interval.end_day.is_finite()
                    && interval.end_day > interval.start_day
                    && interval.end_day <= until,
                "ordered deployed intervals",
            )?;
            self.validate_profile(&interval.profile)?;
            require(
                interval.membership == self.provenance.membership,
                "interval unit membership",
            )?;
            let epoch = self
                .epochs
                .iter()
                .find(|epoch| epoch.id == interval.epoch)
                .ok_or(ValidationError::Invalid("interval epoch"))?;
            require(
                interval.start_day >= epoch.start_day && interval.end_day <= epoch.end_day,
                "interval crosses truth epoch",
            )?;
            require(
                nonnegative(interval.verified_daily_active_hours)
                    && (interval.verified_daily_active_hours
                        <= (interval.end_day - interval.start_day) * 24.0
                        || close(
                            interval.verified_daily_active_hours,
                            (interval.end_day - interval.start_day) * 24.0,
                        ))
                    && (interval.period == Period::Day
                        || interval.verified_daily_active_hours == 0.0),
                "verified active hours",
            )?;
            match self.first_finish_day() {
                Some(finish) => {
                    require(
                        interval.end_day <= finish || interval.start_day >= finish,
                        "interval crosses first finish",
                    )?;
                    require(
                        interval.phase
                            == if interval.start_day >= finish {
                                Phase::PostFinish
                            } else {
                                Phase::Learning
                            },
                        "original post-finish boundary",
                    )?;
                }
                None => require(interval.phase == Phase::Learning, "unfinished phase")?,
            }
            if let Some(id) = &interval.restoration_learning {
                require(
                    self.restorations
                        .iter()
                        .any(|restoration| &restoration.id == id),
                    "restoration interval identity",
                )?;
            }
            let a = &interval.admissibility;
            require(
                a.unit_ratios.len() == self.units.len()
                    && nonnegative(a.joint_ratio)
                    && a.unit_ratios.iter().all(|ratio| nonnegative(*ratio)),
                "admissibility dimensions/ratios",
            )?;
            let count = u32::try_from(self.units.len())
                .map_err(|_| ValidationError::Invalid("unit count"))?;
            require(
                a.joint_ratio + epoch.reference.tolerance
                    >= a.unit_ratios.iter().sum::<f64>() / f64::from(count),
                "joint hazard source allocation",
            )?;
            require(
                a.member
                    == (a.joint_ratio <= 1.0 && a.unit_ratios.iter().all(|ratio| *ratio <= 1.0)),
                "admissibility membership",
            )?;
            self.validate_sources(&interval.sources)?;
            end = interval.end_day;
        }
        require(end == until, "complete deployed trajectory coverage")?;
        Ok(())
    }

    fn validate_sources(&self, sources: &[SourceRisk]) -> Result<()> {
        for (index, source) in sources.iter().enumerate() {
            self.validate_source(&source.source)?;
            require(
                nonnegative(source.expected_failures)
                    && !sources[..index]
                        .iter()
                        .any(|other| other.source == source.source),
                "unique interval risk sources",
            )?;
        }
        for unit in &self.units {
            require(
                sources
                    .iter()
                    .any(|source| source.source == RiskSource::Unit { unit: unit.unit }),
                "missing unit risk source",
            )?;
        }
        Ok(())
    }

    fn validate_source(&self, source: &RiskSource) -> Result<()> {
        match source {
            RiskSource::Unit { unit } => {
                require(unit.index() < self.units.len(), "risk source unit")
            }
            RiskSource::Interaction { identity } => {
                require(!identity.is_empty(), "interaction source identity")
            }
        }
    }

    fn validate_events(&self) -> Result<()> {
        require(
            unique_text(
                self.events
                    .iter()
                    .map(|event| event.policy_event.id.as_str()),
            ),
            "unique machine event identities",
        )?;
        let mut previous = 1.0;
        for event in &self.events {
            let observed = &event.policy_event;
            require(
                day(observed.day)
                    && observed.day >= previous
                    && self.observes_instant(observed.day),
                "ordered event days",
            )?;
            previous = observed.day;
            let finishes_on_event = self
                .units
                .iter()
                .filter_map(|unit| unit.finish.as_ref())
                .any(|finish| {
                    finish.day == observed.day && finish.witnesses.contains(&observed.id)
                });
            let expected_phase = if self
                .first_finish_day()
                .is_some_and(|finish| observed.day >= finish)
                && !finishes_on_event
            {
                Phase::PostFinish
            } else {
                Phase::Learning
            };
            require(
                event.phase == observed.phase && event.phase == expected_phase,
                "event first-finish phase",
            )?;
            self.validate_profile(&event.profile)?;
            // An interruption belongs to the profile that ended at its timestamp, before
            // the zero-time retreat application at that same boundary.
            let deployed = self.intervals.iter().find(|interval| {
                interval.period == event.period
                    && (interval.start_day < observed.day
                        || (observed.day == 1.0 && interval.start_day == 1.0))
                    && observed.day <= interval.end_day
            });
            require(
                deployed.is_some_and(|interval| interval.profile == event.profile),
                "event pre-interruption profile",
            )?;
            if observed.classification == Classification::Unreported {
                require(
                    !observed.charged
                        && !observed.provisional
                        && observed.reclassified_from.is_none()
                        && observed.newest_step.is_none(),
                    "unreported policy classification has no invented decision",
                )?;
            }
            if let Some(id) = &event.restoration_learning {
                require(
                    self.restorations
                        .iter()
                        .any(|restoration| &restoration.id == id),
                    "restoration event identity",
                )?;
            }
            if let TruthCause::Co {
                source,
                implicated_units,
            } = &event.truth
            {
                self.validate_source(source)?;
                let mut units = BTreeSet::new();
                require(
                    !implicated_units.is_empty()
                        && implicated_units
                            .iter()
                            .all(|unit| unit.index() < self.units.len() && units.insert(*unit)),
                    "true CO implicated units",
                )?;
                if let RiskSource::Unit { unit } = source {
                    require(implicated_units.contains(unit), "true CO source membership")?;
                }
            }
            if let Some(step) = &observed.newest_step {
                self.validate_step_identity(step)?;
            }
        }
        require(
            self.events
                .iter()
                .filter(|event| {
                    event.period == Period::Day
                        && event.policy_event.classification != Classification::Unreported
                })
                .map(|event| &event.policy_event)
                .eq(self.policy.events.iter()),
            "policy event report",
        )?;
        if let Some(step) = &self.policy.newest_step {
            self.validate_step_identity(step)?;
        }
        let charged = || {
            self.events
                .iter()
                .filter(|event| event.policy_event.charged)
        };
        let Some(ledger) = &self.policy.allowance else {
            return require(
                charged().next().is_none(),
                "charged event without a machine allowance",
            );
        };
        let consumed = u64::try_from(charged().count())
            .map_err(|_| ValidationError::Invalid("event count"))?;
        let provisional = u64::try_from(
            charged()
                .filter(|event| event.policy_event.provisional)
                .count(),
        )
        .map_err(|_| ValidationError::Invalid("provisional event count"))?;
        let reclassified = u64::try_from(
            charged()
                .filter(|event| event.policy_event.reclassified_from.is_some())
                .count(),
        )
        .map_err(|_| ValidationError::Invalid("reclassified event count"))?;
        require(
            ledger.limit == 2
                && ledger.consumed == consumed
                && ledger.provisional == provisional
                && ledger.reclassified == reclassified,
            "machine allowance accounting",
        )?;
        let exhausted = charged().nth(1).map(|event| event.policy_event.day);
        require(
            ledger.exhausted_day == exhausted,
            "allowance exhaustion witness",
        )?;
        Ok(())
    }

    fn validate_step_identity(&self, identity: &StepIdentity) -> Result<()> {
        match identity {
            StepIdentity::Unit { unit, proposal } => require(
                unit.index() < self.units.len()
                    && self.steps.iter().any(|step| &step.id == proposal
                        && matches!(step.kind, StepKind::Climb { unit: proposed, .. } if proposed == *unit)),
                "newest unit step identity",
            ),
            StepIdentity::Restoration { restoration, units } => require(
                self.restorations.iter().any(|item| &item.id == restoration)
                    && !units.is_empty()
                    && units.iter().all(|unit| unit.index() < self.units.len()),
                "newest restoration identity",
            ),
        }
    }

    fn validate_qualification(&self, qualification: &Qualification) -> Result<()> {
        require(
            !qualification.id.is_empty()
                && day(qualification.day)
                && self
                    .epochs
                    .iter()
                    .any(|epoch| epoch.id == qualification.epoch),
            "qualification identity",
        )?;
        self.validate_profile(&qualification.profile)?;
        require(
            unique_text(
                qualification
                    .stock_controls
                    .iter()
                    .chain(&qualification.fresh_witnesses)
                    .chain(&qualification.reused_witnesses)
                    .map(String::as_str),
            ),
            "qualification witness identities",
        )
    }

    fn validate_steps(&self) -> Result<()> {
        require(
            unique_text(self.steps.iter().map(|step| step.id.as_str()))
                && unique_text(
                    self.restorations
                        .iter()
                        .map(|restoration| restoration.id.as_str()),
                ),
            "application identities",
        )?;
        let mut last_day = 1.0;
        for step in &self.steps {
            require(
                day(step.day) && step.day >= last_day && self.observes_instant(step.day),
                "ordered application days",
            )?;
            last_day = step.day;
            self.validate_profile(&step.before)?;
            self.validate_profile(&step.after)?;
            match &step.kind {
                StepKind::Climb {
                    unit,
                    ceiling_depth,
                    clean_hold_hours,
                    qualification,
                } => {
                    require(
                        unit.index() < self.units.len()
                            && *ceiling_depth <= 45
                            && nonnegative(*clean_hold_hours),
                        "climb step dimensions",
                    )?;
                    self.validate_qualification(qualification)?;
                }
                StepKind::Restoration { restoration } => require(
                    self.restorations.iter().any(|item| &item.id == restoration),
                    "restoration step identity",
                )?,
                StepKind::Retreat { event, units } => require(
                    event.as_ref().is_none_or(|id| {
                        self.events.iter().any(|event| &event.policy_event.id == id)
                    }) && !units.is_empty()
                        && units.iter().all(|unit| unit.index() < self.units.len()),
                    "retreat step identities",
                )?,
                StepKind::StockReset { epoch } => require(
                    self.epochs.iter().any(|item| &item.id == epoch),
                    "stock reset epoch",
                )?,
            }
        }
        for restoration in &self.restorations {
            require(
                day(restoration.transition_day)
                    && day(restoration.stock_reset_day)
                    && restoration.membership == self.provenance.membership
                    && self.epochs.iter().any(|epoch| {
                        epoch.id == restoration.epoch
                            && epoch.start_day == restoration.transition_day
                    }),
                "firmware transition identities",
            )?;
            self.validate_profile(&restoration.previous_profile)?;
            self.validate_profile(&restoration.stock_readback)?;
            match &restoration.outcome {
                RestorationOutcome::Blocked { reason } => {
                    require(!reason.is_empty(), "blocked restoration reason")?;
                }
                RestorationOutcome::Applied {
                    day: applied_day,
                    qualification,
                    clean_stock_hours,
                    result,
                } => {
                    require(
                        day(*applied_day) && nonnegative(*clean_stock_hours),
                        "restoration application",
                    )?;
                    self.validate_qualification(qualification)?;
                    match result {
                        RestorationResult::Retained {} => {}
                        RestorationResult::Crashed {
                            event,
                            stock_step_back_day,
                            stock_readback,
                            stopped_units,
                            witnesses,
                        } => {
                            require(
                                day(*stock_step_back_day)
                                    && self
                                        .events
                                        .iter()
                                        .any(|item| &item.policy_event.id == event)
                                    && stopped_units
                                        .iter()
                                        .all(|unit| unit.index() < self.units.len())
                                    && !witnesses.is_empty()
                                    && unique_text(witnesses.iter().map(String::as_str)),
                                "restoration crash witnesses",
                            )?;
                            self.validate_profile(stock_readback)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn observes_instant(&self, day: f64) -> bool {
        day < self.outcome.observed_until_day()
            || (!self.outcome.is_complete() && day == self.outcome.observed_until_day())
    }

    fn validate_accounts(&self) -> Result<()> {
        self.validate_phase_accounts(Phase::Learning, &self.accounts.learning)?;
        self.validate_phase_accounts(Phase::PostFinish, &self.accounts.post_finish)
    }

    fn validate_phase_accounts(&self, phase: Phase, accounts: &[SourceAccount]) -> Result<()> {
        let intervals = || {
            self.intervals
                .iter()
                .filter(|interval| interval.period == Period::Day && interval.phase == phase)
        };
        let hours: f64 = intervals()
            .map(|interval| interval.verified_daily_active_hours)
            .sum();
        for (index, account) in accounts.iter().enumerate() {
            self.validate_source(&account.source)?;
            require(
                !accounts[..index]
                    .iter()
                    .any(|other| other.source == account.source)
                    && nonnegative(account.active_hours)
                    && nonnegative(account.expected_failures),
                "source account dimensions",
            )?;
            let h: f64 = intervals()
                .flat_map(|interval| &interval.sources)
                .filter(|source| source.source == account.source)
                .map(|source| source.expected_failures)
                .sum();
            let k = self.events.iter().filter(|event| event.period == Period::Day && event.phase == phase && matches!(&event.truth, TruthCause::Co { source, .. } if *source == account.source)).count();
            require(
                close(account.active_hours, hours)
                    && close(account.expected_failures, h)
                    && u64::try_from(k).ok() == Some(account.k),
                "source K/T/H account",
            )?;
        }
        for unit in &self.units {
            require(
                accounts
                    .iter()
                    .any(|account| account.source == RiskSource::Unit { unit: unit.unit }),
                "missing unit account",
            )?;
        }
        for source in intervals()
            .flat_map(|interval| &interval.sources)
            .map(|source| &source.source)
            .chain(
                self.events
                    .iter()
                    .filter(|event| event.period == Period::Day && event.phase == phase)
                    .filter_map(|event| {
                        if let TruthCause::Co { source, .. } = &event.truth {
                            Some(source)
                        } else {
                            None
                        }
                    }),
            )
        {
            require(
                accounts.iter().any(|account| &account.source == source),
                "missing risk source account",
            )?;
        }
        Ok(())
    }

    fn validate_observation(&self) -> Result<()> {
        let until = self.outcome.observed_until_day();
        require(
            day(until) && until <= self.epochs.last().map_or(1.0, |epoch| epoch.end_day),
            "observation end",
        )?;
        let e = &self.exposure;
        require(
            nonnegative(e.intended_seconds)
                && nonnegative(e.observed_seconds)
                && nonnegative(e.eligible_seconds)
                && nonnegative(e.onset_active_seconds)
                && e.observed_seconds <= e.intended_seconds
                && e.eligible_seconds <= e.observed_seconds
                && e.onset_active_seconds <= e.observed_seconds,
            "exposure seconds",
        )?;
        let hours: f64 = self
            .intervals
            .iter()
            .map(|interval| interval.verified_daily_active_hours)
            .sum();
        require(
            e.observed_seconds + 1e-6 >= hours * 3600.0,
            "actual daily exposure",
        )?;
        require(
            nonnegative(self.runtime.wall_seconds)
                && nonnegative(self.runtime.compilation_seconds)
                && self.runtime.jobs > 0
                && !self.runtime.resource_class.is_empty(),
            "runtime manifest",
        )?;
        if let Some(invocation) = &self.runtime.invocation {
            require(
                !invocation.id.trim().is_empty()
                    && nonnegative(invocation.elapsed_seconds)
                    && invocation.elapsed_seconds >= self.runtime.wall_seconds,
                "invocation runtime manifest",
            )?;
        }
        let daily = self
            .events
            .iter()
            .filter(|event| event.period == Period::Day)
            .count();
        let co = self
            .events
            .iter()
            .filter(|event| {
                event.period == Period::Day && matches!(event.truth, TruthCause::Co { .. })
            })
            .count();
        match &self.outcome {
            Outcome::CompleteConverged { .. } => {}
            Outcome::CompleteUnconvergedCrashFree { .. } => {
                require(co == 0, "crash-free means zero true daily CO crashes")?;
            }
            Outcome::CompleteUnconvergedWithCoCrashes { .. } => {
                require(co > 0, "daily CO crash outcome")?;
            }
            Outcome::Truncated {
                reason,
                lower_bound,
                ..
            } => require(
                !reason.is_empty()
                    && lower_bound.daily_interruptions == daily as u64
                    && lower_bound.daily_co_crashes == co as u64
                    && close(lower_bound.active_hours, hours),
                "truncation lower-bound observations",
            )?,
            Outcome::HarnessError { error, .. } => {
                require(!error.is_empty(), "harness error text")?;
            }
        }
        if self.outcome.is_complete() {
            require(
                until == self.epochs.last().map_or(1.0, |epoch| epoch.end_day),
                "full-horizon observation",
            )?;
        }
        Ok(())
    }
}

fn close(left: f64, right: f64) -> bool {
    (left - right).abs() <= 1e-9 * left.abs().max(right.abs()).max(1.0)
}
