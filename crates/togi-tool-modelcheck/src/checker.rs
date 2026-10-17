use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;
use std::io::Write as _;
use std::time::Duration;

use serde::{Serialize, Serializer};
use togi_facts::TrialClass;
use togi_machine::{BiosContext, CoreId, CoreSet, Regime, ResetKind, TargetOffset, WorkloadId};
use togi_sim::{Load, Model, ShapeError};
use togi_tool_trialfacts::{Class, Profile, TrialFact};

use crate::interval::{Interval, interval_with_weights, log_choose, probability_bounds};
use crate::{Check, Error, FactProblem, Status};

/// The fewest decisive trials a group needs before it is checked.
pub const MIN_GROUP_TRIALS: u32 = 10;

/// Decisive trials of one BIOS context and trial class whose shallowest loaded-core offset is the same.
///
/// A checker's groups carry only the counts. A group from [`Checker::check`] also carries the machine's
/// prediction: the mean failure probability over its trials, the interval that probability implies and
/// whether the observed count `k` is outside it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Group {
    pub context: Option<BiosContext>,
    #[serde(serialize_with = "serialize_class")]
    pub class: TrialClass,
    /// The shallowest offset of the profile over the loaded cores: any loaded core at 0 makes it 0.
    pub depth: TargetOffset,
    pub n: u32,
    pub k: u32,
    pub interval: Interval,
    pub mean_p: f64,
    pub flagged: bool,
}

#[derive(Serialize)]
struct ClassFields<'a> {
    regime: Regime,
    workload: WorkloadId,
    cores: &'a CoreSet,
    duration_s: u32,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes a field by reference"
)]
fn serialize_class<S: Serializer>(class: &TrialClass, serializer: S) -> Result<S::Ok, S::Error> {
    ClassFields {
        regime: class.workload.regime(),
        workload: class.workload,
        cores: &class.loaded,
        duration_s: class.duration_s,
    }
    .serialize(serializer)
}

/// The class, depth and counts: `R7 workload cores=0,1 duration=120s depth=-24 n=45 k=5`.
impl fmt::Display for Group {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} cores=",
            self.class.workload.regime(),
            self.class.workload
        )?;
        for (i, core) in self.class.loaded.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            write!(f, "{core}")?;
        }
        write!(
            f,
            " duration={}s depth={} n={} k={}",
            self.class.duration_s,
            self.depth.get(),
            self.n,
            self.k
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct Key<'a> {
    context: Option<&'a BiosContext>,
    class: TrialClass,
    depth: TargetOffset,
}

struct Point {
    profile: Profile,
    count: u32,
}

struct Prepared {
    group: Group,
    points: Vec<Point>,
    bounds: (f64, f64),
    choose: Vec<f64>,
}

impl Prepared {
    fn mean_p(&self, model: &Model) -> Result<f64, ShapeError> {
        let class = self.group.class;
        let load = Load {
            regime: class.workload.regime(),
            workload: class.workload.as_str(),
            cores: class.loaded,
            duration: Duration::from_secs(u64::from(class.duration_s)),
        };
        let mut total = 0.0;
        for point in &self.points {
            total += f64::from(point.count)
                * model.failure_probability(point.profile.as_slice(), &load)?;
        }
        Ok(total / f64::from(self.group.n))
    }
}

/// The real evidence of a machine, grouped once, ready to judge any number of predictions against it.
///
/// Building a checker validates and groups the facts; [`check`](Self::check) and
/// [`accepts`](Self::accepts) then cost one failure probability per distinct profile of each group, which
/// is what a fit repeats while it searches.
pub struct Checker {
    groups: Vec<Prepared>,
    idle_failures: usize,
}

impl Checker {
    /// Groups the decisive trials of `records` that belong to `model`'s machine: all of them, or, when the
    /// machine declares a BIOS context, those recorded under exactly that context. Failures outside any
    /// trial are only counted. Groups under [`MIN_GROUP_TRIALS`] trials are dropped.
    ///
    /// Every trial must fit `model`: one offset per core, a loaded core that exists and a duration.
    pub fn new(model: &Model, records: &[TrialFact]) -> Result<Self, Error> {
        for reset in [ResetKind::ThermalTrip, ResetKind::PowerLoss] {
            if model.reset_mix().weight(reset) > 0.0 {
                return Err(Error::IndecisiveReset(reset));
            }
        }
        let cores = model.cores();
        let declared = model.bios_context_declared().then(|| model.bios_context());
        let mut idle_failures = 0;
        let mut index = HashMap::new();
        let mut building: Vec<(Prepared, HashMap<Profile, usize>)> = Vec::new();
        for fact in records {
            if declared.is_some_and(|context| fact.context.as_ref() != Some(context)) {
                continue;
            }
            let Class::Trial(class) = fact.class else {
                idle_failures += 1;
                continue;
            };
            let problem = |problem| Error::InvalidFact {
                session: fact.session,
                seq: fact.seq,
                problem,
            };
            let offsets = fact.profile.as_slice();
            if offsets.len() != cores {
                return Err(problem(FactProblem::ProfileSize {
                    found: offsets.len(),
                    cores,
                }));
            }
            if class.loaded.is_empty() {
                return Err(problem(FactProblem::NoLoadedCore));
            }
            if class.duration_s == 0 {
                return Err(problem(FactProblem::ZeroDuration));
            }
            if let Some(core) = class.loaded.iter().find(|core| core.index() >= cores) {
                return Err(problem(FactProblem::LoadedCore {
                    core: core.get(),
                    cores,
                }));
            }
            let depth = class
                .loaded
                .iter()
                .map(|core| offsets[core.index()])
                .max()
                .unwrap_or(TargetOffset::ZERO);
            let at = *index
                .entry(Key {
                    context: fact.context.as_ref(),
                    class,
                    depth,
                })
                .or_insert_with(|| {
                    building.push((
                        Prepared {
                            group: Group {
                                context: fact.context.clone(),
                                class,
                                depth,
                                n: 0,
                                k: 0,
                                interval: Interval { low: 0, high: 0 },
                                mean_p: 0.0,
                                flagged: false,
                            },
                            points: Vec::new(),
                            bounds: (0.0, 1.0),
                            choose: Vec::new(),
                        },
                        HashMap::new(),
                    ));
                    building.len() - 1
                });
            let (prepared, points) = &mut building[at];
            prepared.group.n += 1;
            if matches!(fact.outcome, togi_facts::Decisive::Failure { .. }) {
                prepared.group.k += 1;
            }
            let point = *points.entry(fact.profile).or_insert_with(|| {
                prepared.points.push(Point {
                    profile: fact.profile,
                    count: 0,
                });
                prepared.points.len() - 1
            });
            prepared.points[point].count += 1;
        }
        let mut groups: Vec<Prepared> = building
            .into_iter()
            .map(|(prepared, _)| prepared)
            .filter(|prepared| prepared.group.n >= MIN_GROUP_TRIALS)
            .collect();
        groups.sort_by(|a, b| order(&a.group, &b.group));
        for prepared in &mut groups {
            prepared.choose = log_choose(prepared.group.n);
            prepared.bounds =
                probability_bounds(prepared.group.n, prepared.group.k, &prepared.choose);
        }
        Ok(Self {
            groups,
            idle_failures,
        })
    }

    /// Failures outside any trial that the extract holds for this machine. They have no duration to
    /// predict over, so they are reported and never checked.
    pub fn idle_failures(&self) -> usize {
        self.idle_failures
    }

    /// Whether every group's mean failure probability under `model` keeps its observed count inside the
    /// interval; the same verdict as `check(..)?.status == Status::Ok`, without building the groups.
    pub fn accepts(&self, model: &Model) -> Result<bool, ShapeError> {
        for prepared in &self.groups {
            let p = prepared.mean_p(model)?;
            if p < prepared.bounds.0 || p > prepared.bounds.1 {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Every eligible group with `model`'s prediction filled in, ordered by context, workload, loaded
    /// cores, duration and depth.
    pub fn groups(&self, model: &Model) -> Result<Vec<Group>, ShapeError> {
        let mut weights = Vec::new();
        self.groups
            .iter()
            .map(|prepared| {
                let mut group = prepared.group.clone();
                group.mean_p = prepared.mean_p(model)?;
                weights.resize(prepared.choose.len(), 0.0);
                group.interval =
                    interval_with_weights(group.n, group.mean_p, &mut weights, &prepared.choose);
                group.flagged = !group.interval.contains(group.k);
                Ok(group)
            })
            .collect()
    }

    /// [`groups`](Self::groups) as a report of `machine` against `extract`, which only label it.
    pub fn check(
        &self,
        machine: impl Into<std::path::PathBuf>,
        extract: impl Into<std::path::PathBuf>,
        model: &Model,
    ) -> Result<Check, ShapeError> {
        let groups = self.groups(model)?;
        let status = if groups.iter().any(|group| group.flagged) {
            Status::Flagged
        } else {
            Status::Ok
        };
        Ok(Check {
            machine: machine.into(),
            extract: extract.into(),
            status,
            groups,
            idle_failures: self.idle_failures,
        })
    }
}

/// Groups in order of BIOS context, workload, loaded cores, intended duration and depth. The context
/// compares field by field as text, its boost limit included.
fn order<'g>(a: &'g Group, b: &'g Group) -> Ordering {
    let context = |group: &'g Group| {
        group.context.as_ref().map(|context| {
            (
                context.bios_version.as_str(),
                context.board.as_str(),
                context.cpu_model.as_str(),
                context.microcode.as_str(),
                decimal(context.boost_limit_mhz),
            )
        })
    };
    let loaded = |group: &'g Group| group.class.loaded.iter().map(CoreId::get);
    context(a)
        .cmp(&context(b))
        .then_with(|| a.class.workload.cmp(&b.class.workload))
        .then_with(|| loaded(a).cmp(loaded(b)))
        .then_with(|| a.class.duration_s.cmp(&b.class.duration_s))
        .then_with(|| a.depth.cmp(&b.depth))
}

/// `n` in decimal, left-aligned and padded with zero bytes, so two of them compare as their text does.
fn decimal(n: u32) -> [u8; 10] {
    let mut text = [0; 10];
    let _ = write!(&mut text[..], "{n}");
    text
}
