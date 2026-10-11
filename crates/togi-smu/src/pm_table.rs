use crate::Error;
use crate::sysfs::DRIVER_DIR;
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};
use togi_machine::{Check, CoreInfo, PER_CORE_MAX, PmTable, PmTableReader};

pub(crate) const VERSION: u32 = 0x0062_0205;
pub(crate) const TABLE_LEN: usize = 2452;
const VERSION_LEN: usize = 4;
const READ_BUDGET: Duration = Duration::from_millis(100);
const MAX_AGE: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lane {
    Power,
    VoltageRequest,
    Temperature,
    C0,
    Cc1,
    Cc6,
}

impl fmt::Display for Lane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Power => "power",
            Self::VoltageRequest => "voltage request",
            Self::Temperature => "temperature",
            Self::C0 => "C0 residency",
            Self::Cc1 => "CC1 residency",
            Self::Cc6 => "CC6 residency",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DecodeError {
    #[error("table is {len} bytes, want {TABLE_LEN}")]
    Size { len: usize },
    #[error("{lane} lane of core {core} is not finite")]
    NonFinite { lane: Lane, core: usize },
}

/// Why the PM table has no usable lanes. Informational: it never fails preflight.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Unavailable {
    #[error("table version unreadable")]
    VersionUnreadable,
    #[error("table version invalid")]
    VersionInvalid,
    #[error("unsupported version")]
    UnsupportedVersion,
    #[error("unsupported core count {0}")]
    CoreCount(usize),
    #[error(
        "unsupported topology: lanes require core IDs 0–15 in CCD/slot order (0–7 on CCD0, 8–15 on CCD1)"
    )]
    Topology,
    #[error("table unreadable")]
    TableUnreadable,
    #[error("unsupported table: {0}")]
    Table(#[from] DecodeError),
    #[error("table read exceeded {}ms", READ_BUDGET.as_millis())]
    ReadExceeded,
    #[error("table read overdue ({}ms limit)", READ_BUDGET.as_millis())]
    Overdue,
    #[error("table read pending")]
    Pending,
    #[error("table reading older than {}s", MAX_AGE.as_secs())]
    Stale,
}

const BLOCKS: [(Lane, usize); 6] = [
    (Lane::Power, 301),
    (Lane::VoltageRequest, 317),
    (Lane::Temperature, 333),
    (Lane::C0, 381),
    (Lane::Cc1, 397),
    (Lane::Cc6, 413),
];

fn block(raw: &[u8], index: usize, lane: Lane) -> Result<[f32; PER_CORE_MAX], DecodeError> {
    let start = index * 4;
    let (words, _) = raw[start..start + PER_CORE_MAX * 4].as_chunks::<4>();
    let mut lanes = [0.0; PER_CORE_MAX];
    for (core, (lane_value, word)) in lanes.iter_mut().zip(words).enumerate() {
        let value = f32::from_le_bytes(*word);
        if !value.is_finite() {
            return Err(DecodeError::NonFinite { lane, core });
        }
        *lane_value = value;
    }
    Ok(lanes)
}

/// Decodes a payload of the supported version, rejecting any non-finite decoded lane.
pub(crate) fn decode(raw: &[u8]) -> Result<PmTable, DecodeError> {
    if raw.len() != TABLE_LEN {
        return Err(DecodeError::Size { len: raw.len() });
    }
    let [power, voltage, temperature, c0, cc1, cc6] =
        BLOCKS.map(|(lane, index)| block(raw, index, lane));
    Ok(PmTable {
        power_w: power?,
        voltage_request_v: voltage?,
        temperature_c: temperature?,
        c0_pct: c0?,
        cc1_pct: cc1?,
        cc6_pct: cc6?,
    })
}

pub(crate) fn parse_version(raw: &[u8]) -> Option<u32> {
    <[u8; VERSION_LEN]>::try_from(raw)
        .ok()
        .map(u32::from_le_bytes)
}

/// Lanes are indexed by core 0–15 on CCD `core / 8`, so the topology must be exactly that.
pub(crate) fn layout(cores: &[CoreInfo]) -> Result<(), Unavailable> {
    if cores.len() != PER_CORE_MAX {
        return Err(Unavailable::CoreCount(cores.len()));
    }
    let in_order = cores
        .iter()
        .enumerate()
        .all(|(index, core)| core.core.index() == index && usize::from(core.ccd.0) == index / 8);
    if in_order {
        Ok(())
    } else {
        Err(Unavailable::Topology)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PmFile {
    Version,
    Table,
}

pub(crate) trait PmSource: Send + Sync {
    fn read(&self, file: PmFile) -> io::Result<Vec<u8>>;
}

pub(crate) struct FileSource {
    dir: PathBuf,
}

impl FileSource {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            dir: root.join(DRIVER_DIR),
        }
    }
}

impl PmSource for FileSource {
    fn read(&self, file: PmFile) -> io::Result<Vec<u8>> {
        let (name, limit) = match file {
            PmFile::Version => ("pm_table_version", VERSION_LEN + 1),
            PmFile::Table => ("pm_table", TABLE_LEN + 1),
        };
        let mut bytes = Vec::with_capacity(limit);
        File::open(self.dir.join(name))?
            .take(limit as u64)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}

/// Time since an arbitrary start, so tests can inject their own.
pub(crate) trait PmClock: Send + Sync {
    fn now(&self) -> Duration;
}

pub(crate) struct Monotonic(Instant);

impl Monotonic {
    pub(crate) fn new() -> Self {
        Self(Instant::now())
    }
}

impl PmClock for Monotonic {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    pub(crate) budget: Duration,
    pub(crate) max_age: Duration,
}

impl Timing {
    pub(crate) const DEFAULT: Self = Self {
        budget: READ_BUDGET,
        max_age: MAX_AGE,
    };
}

type Table = Result<PmTable, Unavailable>;

struct Outcome {
    version: Option<u32>,
    table: Table,
}

fn read_once(
    source: &dyn PmSource,
    layout: Result<(), Unavailable>,
    seen: &mut dyn FnMut(u32),
) -> Outcome {
    let failed = |version, reason| Outcome {
        version,
        table: Err(reason),
    };
    let Ok(raw_version) = source.read(PmFile::Version) else {
        return failed(None, Unavailable::VersionUnreadable);
    };
    let Some(version) = parse_version(&raw_version) else {
        return failed(None, Unavailable::VersionInvalid);
    };
    seen(version);
    if version != VERSION {
        return failed(Some(version), Unavailable::UnsupportedVersion);
    }
    if let Err(reason) = layout {
        return failed(Some(version), reason);
    }
    let Ok(raw) = source.read(PmFile::Table) else {
        return failed(Some(version), Unavailable::TableUnreadable);
    };
    Outcome {
        version: Some(version),
        table: decode(&raw).map_err(Unavailable::from),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flight {
    Idle,
    Requested(Duration),
    Reading(Duration),
}

impl Flight {
    const fn started(self) -> Option<Duration> {
        match self {
            Self::Idle => None,
            Self::Requested(started) | Self::Reading(started) => Some(started),
        }
    }
}

struct Completed {
    version: Option<u32>,
    table: Table,
    started: Duration,
}

struct State {
    flight: Flight,
    seen: Option<u32>,
    latest: Option<Completed>,
    closed: bool,
}

struct View {
    version: Option<u32>,
    table: Table,
}

impl State {
    fn view(&self, now: Duration, timing: Timing) -> View {
        let age = |started: Duration| now.saturating_sub(started);
        if self
            .flight
            .started()
            .is_some_and(|started| age(started) >= timing.budget)
        {
            return View {
                version: self.seen,
                table: Err(Unavailable::Overdue),
            };
        }
        match &self.latest {
            None => View {
                version: self.seen,
                table: Err(Unavailable::Pending),
            },
            Some(done) if age(done.started) > timing.max_age => View {
                version: done.version,
                table: Err(Unavailable::Stale),
            },
            Some(done) => View {
                version: done.version,
                table: done.table,
            },
        }
    }

    fn request(&mut self, now: Duration) {
        if self.flight == Flight::Idle {
            self.flight = Flight::Requested(now);
        }
    }
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    clock: Box<dyn PmClock>,
    timing: Timing,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn run(&self, source: &dyn PmSource, layout: Result<(), Unavailable>) {
        loop {
            let started = {
                let mut state = self.lock();
                loop {
                    if state.closed {
                        return;
                    }
                    if let Flight::Requested(started) = state.flight {
                        state.flight = Flight::Reading(started);
                        break started;
                    }
                    state = self
                        .changed
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            };
            let read = read_once(source, layout, &mut |version| {
                self.lock().seen = Some(version);
            });
            let late = self.clock.now().saturating_sub(started) >= self.timing.budget;
            let mut state = self.lock();
            state.seen = read.version;
            state.latest = Some(Completed {
                version: read.version,
                table: if late {
                    Err(Unavailable::ReadExceeded)
                } else {
                    read.table
                },
                started,
            });
            state.flight = Flight::Idle;
            drop(state);
            self.changed.notify_all();
        }
    }
}

/// The PM table, read by one worker thread that never has two reads in flight.
///
/// A stuck read cannot be cancelled, so it is kept instead of replaced, its late result is
/// discarded and dropping the reader does not wait for it.
pub struct PmReader {
    shared: Arc<Shared>,
}

impl fmt::Debug for PmReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PmReader").finish_non_exhaustive()
    }
}

impl PmReader {
    pub(crate) fn start(
        source: Box<dyn PmSource>,
        layout: Result<(), Unavailable>,
        clock: Box<dyn PmClock>,
        timing: Timing,
    ) -> Result<Self, Error> {
        let first = clock.now();
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                flight: Flight::Requested(first),
                seen: None,
                latest: None,
                closed: false,
            }),
            changed: Condvar::new(),
            clock,
            timing,
        });
        let worker = Arc::clone(&shared);
        thread::Builder::new()
            .name("togi-smu-pm-table".to_owned())
            .spawn(move || worker.run(&*source, layout))
            .map_err(Error::WorkerSpawn)?;
        Ok(Self { shared })
    }

    /// Reports the table version and whether lanes are decoded, waiting at most for what is left
    /// of the current read's budget. Always ok: a missing table is informational.
    pub fn check(&self) -> Check {
        let shared = &*self.shared;
        let mut state = shared.lock();
        let now = shared.clock.now();
        state.request(now);
        let remaining = state.flight.started().map_or(Duration::ZERO, |started| {
            shared
                .timing
                .budget
                .saturating_sub(now.saturating_sub(started))
        });
        shared.changed.notify_all();
        let (state, _) = shared
            .changed
            .wait_timeout_while(state, remaining, |state| state.flight != Flight::Idle)
            .unwrap_or_else(PoisonError::into_inner);
        let view = state.view(shared.clock.now(), shared.timing);
        drop(state);
        let version = view.version.map_or_else(
            || "unavailable".to_owned(),
            |version| format!("{version:#x}"),
        );
        let detail = match view.table {
            Ok(_) => format!(
                "pm_table version {version}: decoding per-core power, voltage request, temperature and C-state lanes"
            ),
            Err(reason) => {
                format!("pm_table version {version}: per-core lanes absent ({reason})")
            }
        };
        Check {
            name: "pm_table".to_owned(),
            detail,
            ok: true,
        }
    }
}

impl PmTableReader for PmReader {
    fn pm_table(&self) -> Option<PmTable> {
        let shared = &*self.shared;
        let mut state = shared.lock();
        let now = shared.clock.now();
        let view = state.view(now, shared.timing);
        state.request(now);
        drop(state);
        shared.changed.notify_all();
        view.table.ok()
    }
}

#[cfg(test)]
impl PmReader {
    pub(crate) fn wait_idle(&self, timeout: Duration) -> bool {
        let (state, _) = self
            .shared
            .changed
            .wait_timeout_while(self.shared.lock(), timeout, |state| {
                state.flight != Flight::Idle
            })
            .unwrap_or_else(PoisonError::into_inner);
        state.flight == Flight::Idle
    }
}

impl Drop for PmReader {
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        self.shared.changed.notify_all();
    }
}
