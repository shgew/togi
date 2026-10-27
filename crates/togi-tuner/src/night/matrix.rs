//! The coverage matrix as typed cells (`docs/spec/workloads.md`, Coverage matrix): qualification,
//! mapping, control and reproduction generators, plus the single-CCD host credit rules.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::num::NonZeroU32;

use togi_machine::{
    CcdId, CcdProfile, CcdProfileError, CoreId, CoreInfo, CoreSet, CpuId, PerCore, Purpose, Regime,
    TargetOffset, WorkloadId, pick_workload,
};

use crate::numbers::index;
use crate::{HonestChain, Secs, Topology, UncoveredReason};

const fn secs(value: u32) -> Secs {
    match NonZeroU32::new(value) {
        Some(value) => Secs::new(value),
        None => panic!("a matrix duration is never zero"),
    }
}

/// R1 and R2 breadth cells.
pub const SEARCH_DURATION: Secs = secs(90);
/// R3, R4, R5, R7, R8 and R9 breadth cells.
pub const SHORT_DURATION: Secs = secs(120);
/// The R6 idle-then-burst cell.
pub const IDLE_DURATION: Secs = secs(900);
/// Every sustained block.
pub const SUSTAINED_DURATION: Secs = secs(600);
/// Catalog entries each of R1–R4's breadth rows run: all R1/R2 entries, the first three of R3/R4.
const BREADTH_ENTRIES: usize = 3;
/// Light/heavy pairs times light schedules R8 rotates through: sustained then bursty per pair.
const R8_ROTATION: u32 = 6;

/// What a cell's trial runs.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Workloads {
    One {
        workload: WorkloadId,
    },
    /// An R8 light role (an R8 catalog entry) beside heavy neighbours (an R2 entry).
    LightHeavy {
        light: WorkloadId,
        heavy: WorkloadId,
    },
}

impl Workloads {
    #[must_use]
    pub const fn regime(self) -> Regime {
        match self {
            Self::One { workload } => workload.regime(),
            Self::LightHeavy { light, .. } => light.regime(),
        }
    }
}

/// Which packages' loaded sets an R7 cell is.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "load", rename_all = "snake_case")]
pub enum PackageLoad {
    FullCcd {
        ccd: CcdId,
    },
    /// An honest partial of `ccd`'s cores; the cell's loaded set names which.
    Partial {
        ccd: CcdId,
    },
    AllCcds,
}

/// How a cell's loaded cores are used, one variant per matrix regime family.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "roles", rename_all = "snake_case")]
pub enum Roles {
    /// R1–R4: one thread on the core's first logical CPU.
    SingleCore { core: CoreId },
    /// R5: both SMT CPUs of the core.
    SmtPair { core: CoreId },
    /// R6: every physical core, idle then bursts.
    AllCoresIdle,
    /// R7: one thread per loaded core.
    Package(PackageLoad),
    /// R8: the light role on `light`, heavy roles on the other cores of its CCD.
    Neighbours { light: CoreId },
    /// R9: every logical CPU of every loaded core; `serial` never stops.
    AllThreads { core: CoreId, serial: CpuId },
}

/// One trial a matrix schedules.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Cell {
    pub workloads: Workloads,
    pub loaded: CoreSet,
    pub roles: Roles,
    pub duration: Secs,
    pub purpose: Purpose,
    /// A sustained block rather than a breadth position.
    pub sustained: bool,
}

impl Cell {
    #[must_use]
    pub const fn regime(&self) -> Regime {
        self.workloads.regime()
    }

    /// The one physical core this cell tests, `None` for package and idle cells.
    #[must_use]
    pub const fn position(&self) -> Option<CoreId> {
        match self.roles {
            Roles::SingleCore { core }
            | Roles::SmtPair { core }
            | Roles::AllThreads { core, .. }
            | Roles::Neighbours { light: core } => Some(core),
            Roles::AllCoresIdle | Roles::Package(_) => None,
        }
    }

    /// Post-warm-up seconds a complete launch of this cell can credit.
    #[must_use]
    pub fn eligible_seconds(&self) -> u32 {
        self.duration
            .get()
            .saturating_sub(self.regime().warm_up_s())
    }

    /// Whether a launch hosted on `host` credits this cell's position: the rotating host earns none.
    #[must_use]
    pub fn credits_position(&self, host: CoreId) -> bool {
        self.position() != Some(host)
    }
}

/// Whether a planned cell must run or is covered by a reused witness.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Fresh,
    /// An unchanged CCD's R1–R5 cell with a witness at that CCD's identical offset, class,
    /// placement, content, epoch and firmware.
    Reused,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlannedCell {
    pub cell: Cell,
    pub source: Source,
}

/// A CCD whose loaded set derives no honest partial.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Uncovered {
    pub ccd: CcdId,
    pub reason: UncoveredReason,
}

/// An ordered list of cells and the whole CCD profile they run at.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Matrix {
    pub profile: CcdProfile,
    pub cells: Vec<PlannedCell>,
    /// R7 partials the topology cannot supply, listed instead of silently dropped.
    pub uncovered: Vec<Uncovered>,
}

impl Matrix {
    /// The cells that still have to launch, in order.
    pub fn fresh(&self) -> impl Iterator<Item = &Cell> {
        self.cells
            .iter()
            .filter(|planned| planned.source == Source::Fresh)
            .map(|planned| &planned.cell)
    }

    #[must_use]
    pub fn fresh_launches(&self) -> usize {
        self.fresh().count()
    }

    /// Intended seconds of every fresh launch, before launch and cleanup overhead.
    #[must_use]
    pub fn fresh_seconds(&self) -> u64 {
        self.fresh()
            .map(|cell| u64::from(cell.duration.get()))
            .sum()
    }
}

/// Which unchanged-CCD R1–R5 cells may be reused.
#[derive(Clone, Copy, Debug)]
pub enum Reuse<'a> {
    /// First qualification, controls, firmware invalidation and monthly re-qualification.
    AllFresh,
    /// An unchanged CCD's cell is reused when it is in `witnesses`.
    Unchanged {
        changed: &'a BTreeSet<CcdId>,
        witnesses: &'a HashSet<Cell>,
    },
}

/// What a reproduction attempt tests.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "attempt", rename_all = "snake_case")]
pub enum AttemptScope {
    /// The exact failed profile.
    Exact,
    /// One CCD alone at its failed offset, the others at CO 0.
    Ccd { ccd: CcdId },
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Attempt {
    pub scope: AttemptScope,
    pub matrix: Matrix,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum MatrixError {
    #[error("core {0} is missing from the topology listing")]
    UnknownCore(CoreId),
    #[error("core {0} lists no logical CPU")]
    NoCpu(CoreId),
    #[error("CCD {0} is not in the topology")]
    UnknownCcd(CcdId),
    #[error("a CCD profile could not be built")]
    Profile(#[from] CcdProfileError),
}

/// The machine a matrix is generated for.
#[derive(Clone, Copy, Debug)]
pub struct Layout<'a> {
    pub topology: &'a Topology,
    /// Cores with their CCD and logical CPUs.
    pub cores: &'a [CoreInfo],
    /// Honest R7 partial chains per CCD; a CCD without one uses the explicit-subset fallback.
    pub partials: &'a BTreeMap<CcdId, HonestChain>,
}

impl Layout<'_> {
    fn cpus(&self, core: CoreId) -> Result<&[CpuId], MatrixError> {
        let info = self
            .cores
            .iter()
            .find(|info| info.core == core)
            .ok_or(MatrixError::UnknownCore(core))?;
        if info.cpus.is_empty() {
            return Err(MatrixError::NoCpu(core));
        }
        Ok(&info.cpus)
    }

    fn ccd_cores(&self, ccd: CcdId) -> Result<CoreSet, MatrixError> {
        self.topology
            .ccds()
            .iter()
            .find(|(id, _)| *id == ccd)
            .map(|(_, cores)| *cores)
            .ok_or(MatrixError::UnknownCcd(ccd))
    }

    /// The partial loaded sets of one CCD, from its honest chain: the measured request groups, or
    /// the explicit-subset fallback when the measured chain has none. No partial at all is a typed
    /// uncovered reason.
    fn partials_of(&self, cores: CoreSet, ccd: CcdId) -> (Vec<CoreSet>, Option<Uncovered>) {
        let fallback = || HonestChain::derive(cores, &PerCore::default());
        let chain = self.partials.get(&ccd).cloned().unwrap_or_else(fallback);
        let chain = match chain {
            HonestChain::Uncovered {
                reason: UncoveredReason::TopGroupLeavesTooFew { .. },
                ..
            } => fallback(),
            chain @ (HonestChain::Partials { .. }
            | HonestChain::Uncovered {
                reason: UncoveredReason::TooFewLoaded,
                ..
            }) => chain,
        };
        match chain {
            HonestChain::Partials { partials, .. } => (partials, None),
            HonestChain::Uncovered { reason, .. } => (Vec::new(), Some(Uncovered { ccd, reason })),
        }
    }
}

/// CCDs a matrix covers, their cores, and the rotation ordinal.
struct Scope<'a> {
    layout: &'a Layout<'a>,
    ccds: Vec<(CcdId, CoreSet)>,
    purpose: Purpose,
    ordinal: u32,
}

impl Scope<'_> {
    fn positions(&self) -> CoreSet {
        self.ccds
            .iter()
            .flat_map(|(_, cores)| cores.iter())
            .collect()
    }

    fn position_at(&self, ordinal: u32) -> Option<CoreId> {
        let positions = self.positions();
        let count = u32::try_from(positions.len()).ok()?;
        positions.iter().nth(index(ordinal.checked_rem(count)?))
    }

    /// The serial logical CPU of `core`: its SMT sibling chosen by the ordinal modulo the siblings.
    fn serial(&self, core: CoreId) -> Result<CpuId, MatrixError> {
        let cpus = self.layout.cpus(core)?;
        let count = u32::try_from(cpus.len()).unwrap_or(u32::MAX);
        let slot = index(self.ordinal.checked_rem(count).unwrap_or(0));
        cpus.get(slot).copied().ok_or(MatrixError::NoCpu(core))
    }

    fn cell(
        &self,
        workloads: Workloads,
        loaded: CoreSet,
        roles: Roles,
        duration: Secs,
        sustained: bool,
    ) -> Cell {
        Cell {
            workloads,
            loaded,
            roles,
            duration,
            purpose: self.purpose,
            sustained,
        }
    }

    fn single_core(&self, regime: Regime, entries: usize) -> Vec<Cell> {
        let duration = match regime {
            Regime::R1 | Regime::R2 => SEARCH_DURATION,
            Regime::R3
            | Regime::R4
            | Regime::R5
            | Regime::R6
            | Regime::R7
            | Regime::R8
            | Regime::R9 => SHORT_DURATION,
        };
        let positions = self.positions();
        (0..entries)
            .filter_map(|entry| WorkloadId::new(regime, entry))
            .flat_map(|workload| {
                positions.iter().map(move |core| {
                    self.cell(
                        Workloads::One { workload },
                        core_set(core),
                        Roles::SingleCore { core },
                        duration,
                        false,
                    )
                })
            })
            .collect()
    }

    fn smt_pairs(&self) -> Vec<Cell> {
        let workload = pick_workload(Regime::R5, index(self.ordinal)).id();
        self.positions()
            .iter()
            .map(|core| {
                self.cell(
                    Workloads::One { workload },
                    core_set(core),
                    Roles::SmtPair { core },
                    SHORT_DURATION,
                    false,
                )
            })
            .collect()
    }

    /// R6 spans every physical core and keeps the exact-profile placement even inside an isolated
    /// scope: the placement table gives R6 the unloaded-core rules, not the mapping ones.
    fn idle(&self) -> Cell {
        let workload = pick_workload(Regime::R6, index(self.ordinal)).id();
        Cell {
            purpose: Purpose::Exact,
            ..self.cell(
                Workloads::One { workload },
                self.layout.topology.all(),
                Roles::AllCoresIdle,
                IDLE_DURATION,
                false,
            )
        }
    }

    fn package(&self, with_all_ccds: bool, uncovered: &mut Vec<Uncovered>) -> Vec<Cell> {
        let workload = pick_workload(Regime::R7, index(self.ordinal)).id();
        let one = Workloads::One { workload };
        let mut cells: Vec<Cell> = self
            .ccds
            .iter()
            .map(|&(ccd, cores)| {
                self.cell(
                    one,
                    cores,
                    Roles::Package(PackageLoad::FullCcd { ccd }),
                    SHORT_DURATION,
                    false,
                )
            })
            .collect();
        for &(ccd, cores) in &self.ccds {
            let (partials, missing) = self.layout.partials_of(cores, ccd);
            uncovered.extend(missing);
            cells.extend(partials.into_iter().map(|loaded| {
                self.cell(
                    one,
                    loaded,
                    Roles::Package(PackageLoad::Partial { ccd }),
                    SHORT_DURATION,
                    false,
                )
            }));
        }
        if with_all_ccds && self.ccds.len() > 1 {
            cells.push(self.cell(
                one,
                self.positions(),
                Roles::Package(PackageLoad::AllCcds),
                SHORT_DURATION,
                false,
            ));
        }
        cells
    }

    /// The sustained all-CCD (or the only CCD's) full block at the rotating R7 entry.
    fn package_sustained(&self) -> Cell {
        let workload = pick_workload(Regime::R7, index(self.ordinal)).id();
        let roles = match self.ccds.as_slice() {
            [(ccd, _)] => PackageLoad::FullCcd { ccd: *ccd },
            _ => PackageLoad::AllCcds,
        };
        self.cell(
            Workloads::One { workload },
            self.positions(),
            Roles::Package(roles),
            SUSTAINED_DURATION,
            true,
        )
    }

    /// The rotating R8 pair: index-matched light and heavy entries, sustained light then bursty
    /// light for each pair.
    fn neighbours_workloads(&self) -> Workloads {
        let slot = index(self.ordinal.checked_rem(R8_ROTATION).unwrap_or(0));
        let pair = slot / 2;
        let bursty = slot % 2;
        Workloads::LightHeavy {
            light: pick_workload(Regime::R8, pair + 3 * bursty).id(),
            heavy: pick_workload(Regime::R2, pair).id(),
        }
    }

    fn neighbours_cell(
        &self,
        light: CoreId,
        duration: Secs,
        sustained: bool,
    ) -> Result<Cell, MatrixError> {
        let ccd = self
            .layout
            .topology
            .ccd_of(light)
            .ok_or(MatrixError::UnknownCore(light))?;
        Ok(self.cell(
            self.neighbours_workloads(),
            self.layout.ccd_cores(ccd)?,
            Roles::Neighbours { light },
            duration,
            sustained,
        ))
    }

    fn neighbours(&self) -> Result<Vec<Cell>, MatrixError> {
        self.positions()
            .iter()
            .map(|light| self.neighbours_cell(light, SHORT_DURATION, false))
            .collect()
    }

    fn neighbours_sustained(&self) -> Result<Option<Cell>, MatrixError> {
        self.position_at(self.ordinal)
            .map(|light| self.neighbours_cell(light, SUSTAINED_DURATION, true))
            .transpose()
    }

    fn all_threads_cell(
        &self,
        core: CoreId,
        duration: Secs,
        sustained: bool,
    ) -> Result<Cell, MatrixError> {
        let workload = pick_workload(Regime::R9, index(self.ordinal)).id();
        Ok(self.cell(
            Workloads::One { workload },
            self.positions(),
            Roles::AllThreads {
                core,
                serial: self.serial(core)?,
            },
            duration,
            sustained,
        ))
    }

    fn all_threads(&self) -> Result<Vec<Cell>, MatrixError> {
        self.positions()
            .iter()
            .map(|core| self.all_threads_cell(core, SHORT_DURATION, false))
            .collect()
    }

    fn all_threads_sustained(&self) -> Result<Option<Cell>, MatrixError> {
        self.position_at(self.ordinal)
            .map(|core| self.all_threads_cell(core, SUSTAINED_DURATION, true))
            .transpose()
    }
}

fn core_set(core: CoreId) -> CoreSet {
    std::iter::once(core).collect()
}

fn scope<'a>(
    layout: &'a Layout<'a>,
    ccds: Vec<(CcdId, CoreSet)>,
    purpose: Purpose,
    ordinal: u32,
) -> Scope<'a> {
    Scope {
        layout,
        ccds,
        purpose,
        ordinal,
    }
}

fn machine_ccds(layout: &Layout<'_>) -> Vec<(CcdId, CoreSet)> {
    layout.topology.ccds().to_vec()
}

fn fresh(cells: impl IntoIterator<Item = Cell>) -> impl Iterator<Item = PlannedCell> {
    cells.into_iter().map(|cell| PlannedCell {
        cell,
        source: Source::Fresh,
    })
}

/// `profile` with every CCD at CO 0.
#[must_use]
pub fn zeroed(profile: &CcdProfile) -> CcdProfile {
    profile.iter().fold(profile.clone(), |zero, (ccd, _)| {
        zero.with(ccd, TargetOffset::ZERO)
    })
}

/// `profile` zeroed except `ccd` at `offset`.
fn isolated_profile(
    layout: &Layout<'_>,
    ccd: CcdId,
    offset: TargetOffset,
) -> Result<CcdProfile, MatrixError> {
    let zero = CcdProfile::zero(layout.topology.ccds().iter().map(|(id, _)| *id))?;
    Ok(zero.with(ccd, offset))
}

/// The whole qualification matrix in R1..R9 order: R1–R5 for every core, then the package and
/// interaction rows, each followed by its sustained block.
fn qualification_rows(
    scope: &Scope<'_>,
    reuse: &Reuse<'_>,
) -> Result<(Vec<PlannedCell>, Vec<Uncovered>), MatrixError> {
    let mut uncovered = Vec::new();
    let mut cells = Vec::new();
    let per_core = [
        scope.single_core(Regime::R1, BREADTH_ENTRIES),
        scope.single_core(Regime::R2, BREADTH_ENTRIES),
        scope.single_core(Regime::R3, BREADTH_ENTRIES),
        scope.single_core(Regime::R4, BREADTH_ENTRIES),
        scope.smt_pairs(),
    ];
    for cell in per_core.into_iter().flatten() {
        let reused = match *reuse {
            Reuse::AllFresh => false,
            Reuse::Unchanged { changed, witnesses } => {
                let ccd = cell
                    .position()
                    .and_then(|core| scope.layout.topology.ccd_of(core));
                ccd.is_some_and(|ccd| !changed.contains(&ccd)) && witnesses.contains(&cell)
            }
        };
        cells.push(PlannedCell {
            cell,
            source: if reused {
                Source::Reused
            } else {
                Source::Fresh
            },
        });
    }
    cells.extend(fresh([scope.idle()]));
    cells.extend(fresh(scope.package(true, &mut uncovered)));
    cells.extend(fresh([scope.package_sustained()]));
    cells.extend(fresh(scope.neighbours()?));
    cells.extend(fresh(scope.neighbours_sustained()?));
    cells.extend(fresh(scope.all_threads()?));
    cells.extend(fresh(scope.all_threads_sustained()?));
    Ok((cells, uncovered))
}

/// The exact-profile qualification certificate's cells in R1..R9 order. R1–R5 cells of unchanged
/// CCDs are marked [`Source::Reused`] when `reuse` holds their witness; every package and
/// interaction cell runs fresh at the complete proposed `profile`. `ordinal` is the durable
/// qualification ordinal that rotates the selected entries and positions.
pub fn qualification_cells(
    layout: &Layout<'_>,
    profile: &CcdProfile,
    ordinal: u32,
    reuse: &Reuse<'_>,
) -> Result<Matrix, MatrixError> {
    let scope = scope(layout, machine_ccds(layout), Purpose::Exact, ordinal);
    let (cells, uncovered) = qualification_rows(&scope, reuse)?;
    Ok(Matrix {
        profile: profile.clone(),
        cells,
        uncovered,
    })
}

/// The whole matrix at all-zero for first-night and changed-firmware stock controls: every cell
/// fresh, no reuse.
pub fn control_cells(
    layout: &Layout<'_>,
    profile: &CcdProfile,
    ordinal: u32,
) -> Result<Matrix, MatrixError> {
    qualification_cells(layout, &zeroed(profile), ordinal, &Reuse::AllFresh)
}

/// One mapping step of `ccd` at `offset`, other CCDs at zero: R1, R2, the rotating R8 pair at every
/// light position, R7 full and honest partials, and the selected R9 serial position, in that
/// order. `ordinal` is the mapping-step ordinal. Mapping has no sustained blocks and never
/// qualifies a day profile.
pub fn mapping_cells(
    layout: &Layout<'_>,
    ccd: CcdId,
    offset: TargetOffset,
    ordinal: u32,
) -> Result<Matrix, MatrixError> {
    let cores = layout.ccd_cores(ccd)?;
    let scope = scope(layout, vec![(ccd, cores)], Purpose::Isolated, ordinal);
    let mut uncovered = Vec::new();
    let mut cells: Vec<PlannedCell> = Vec::new();
    cells.extend(fresh(scope.single_core(Regime::R1, BREADTH_ENTRIES)));
    cells.extend(fresh(scope.single_core(Regime::R2, BREADTH_ENTRIES)));
    cells.extend(fresh(scope.neighbours()?));
    cells.extend(fresh(scope.package(false, &mut uncovered)));
    if let Some(core) = scope.position_at(ordinal) {
        cells.extend(fresh([scope.all_threads_cell(
            core,
            SHORT_DURATION,
            false,
        )?]));
    }
    Ok(Matrix {
        profile: isolated_profile(layout, ccd, offset)?,
        cells,
        uncovered,
    })
}

/// A reproduction night's attempts for the failed `profile`, in order: the exact profile, then each
/// CCD alone at its failed offset in ascending CCD order (a CCD at CO 0 has nothing to isolate).
/// Every attempt lists the qualification matrix's classes in qualification order, all fresh; the
/// caller stops an attempt at its first conclusive failure.
pub fn reproduction_cells(
    layout: &Layout<'_>,
    profile: &CcdProfile,
    ordinal: u32,
) -> Result<Vec<Attempt>, MatrixError> {
    let mut attempts = vec![Attempt {
        scope: AttemptScope::Exact,
        matrix: qualification_cells(layout, profile, ordinal, &Reuse::AllFresh)?,
    }];
    for (ccd, offset) in profile
        .iter()
        .filter(|(_, offset)| *offset != TargetOffset::ZERO)
    {
        let cores = layout.ccd_cores(ccd)?;
        let isolated = scope(layout, vec![(ccd, cores)], Purpose::Isolated, ordinal);
        let (cells, uncovered) = qualification_rows(&isolated, &Reuse::AllFresh)?;
        attempts.push(Attempt {
            scope: AttemptScope::Ccd { ccd },
            matrix: Matrix {
                profile: isolated_profile(layout, ccd, offset)?,
                cells,
                uncovered,
            },
        });
    }
    Ok(attempts)
}

/// Where a single-CCD machine's capped controller may sit (`docs/spec/workloads.md`, Controller
/// placement). A load that leaves a physical core unloaded rotates over the unloaded cores, so
/// every loaded position keeps its credit; with every core loaded the host rotates over the loaded
/// cores and earns its position no credit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostPool {
    Unloaded(CoreSet),
    Loaded(CoreSet),
}

impl HostPool {
    /// The pool of a single-CCD machine with `all` physical cores of which `loaded` are loaded.
    #[must_use]
    pub fn single_ccd(all: CoreSet, loaded: CoreSet) -> Self {
        let unloaded: CoreSet = all.iter().filter(|core| !loaded.contains(*core)).collect();
        if unloaded.is_empty() {
            Self::Loaded(loaded)
        } else {
            Self::Unloaded(unloaded)
        }
    }

    fn cores(self) -> CoreSet {
        match self {
            Self::Unloaded(cores) | Self::Loaded(cores) => cores,
        }
    }

    /// The host of the trial with the persistent trial `ordinal`: core-ID order, modulo the pool.
    #[must_use]
    pub fn host(&self, ordinal: u32) -> Option<CoreId> {
        let cores = self.cores();
        let count = u32::try_from(cores.len()).ok()?;
        cores.iter().nth(index(ordinal.checked_rem(count)?))
    }

    /// Whether a trial with `ordinal` credits `position`: only a loaded-pool host costs it.
    #[must_use]
    pub fn credits(&self, ordinal: u32, position: CoreId) -> bool {
        self.host(ordinal) != Some(position)
    }
}

/// Eligible seconds of a multi-position requirement: the minimum across the required positions, never
/// the sum of simultaneous core-hours. A position without credit counts zero.
#[must_use]
pub fn minimum_position_seconds(required: CoreSet, credited: &PerCore<u32>) -> u32 {
    required
        .iter()
        .map(|core| credited.get(core).unwrap_or(0))
        .min()
        .unwrap_or(0)
}
