use super::fixture::{Gate, Tree, core, lock};
use crate::pm_table::{
    DecodeError, FileSource, Lane, Monotonic, PmClock, PmFile, PmReader, PmSource, State,
    TABLE_LEN, Timing, Unavailable, VERSION, decode, layout, parse_version,
};
use crate::sysfs::DRIVER_DIR;
use std::io::{self, ErrorKind};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};
use test_case::test_case;
use togi_machine::{CcdId, CoreInfo, PmTable, PmTableReader};

const VERSION_BYTES: [u8; 4] = [0x05, 0x02, 0x62, 0x00];
const BLOCKS: [(usize, Lane); 6] = [
    (301, Lane::Power),
    (317, Lane::VoltageRequest),
    (333, Lane::Temperature),
    (381, Lane::C0),
    (397, Lane::Cc1),
    (413, Lane::Cc6),
];
const BUDGET: Duration = Duration::from_millis(100);

fn synthetic() -> (Vec<u8>, PmTable) {
    let mut raw = vec![0_u8; TABLE_LEN];
    let mut table = PmTable::default();
    for (index, lane) in BLOCKS {
        let lanes = match lane {
            Lane::Power => &mut table.power_w,
            Lane::VoltageRequest => &mut table.voltage_request_v,
            Lane::Temperature => &mut table.temperature_c,
            Lane::C0 => &mut table.c0_pct,
            Lane::Cc1 => &mut table.cc1_pct,
            Lane::Cc6 => &mut table.cc6_pct,
        };
        for (core, slot) in lanes.iter_mut().enumerate() {
            let value = f32::from(u16::try_from(index + core).unwrap()) / 4.0;
            *slot = value;
            let at = (index + core) * 4;
            raw[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
    }
    for index in 349..=380 {
        raw[index * 4..index * 4 + 4].copy_from_slice(&f32::NAN.to_le_bytes());
    }
    (raw, table)
}

fn put_lane(raw: &mut [u8], index: usize, value: f32) {
    raw[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn decode_reads_six_lane_blocks_and_ignores_unknown_fields() {
    let (raw, want) = synthetic();
    assert_eq!(decode(&raw).unwrap(), want);
    assert_eq!(want.power_w[0], 301.0 / 4.0);
    assert_eq!(want.cc6_pct[15], 428.0 / 4.0);
}

#[test_case(0; "empty")]
#[test_case(100; "short")]
#[test_case(TABLE_LEN - 1; "one byte short")]
#[test_case(TABLE_LEN + 1; "one byte long")]
fn decode_refuses_any_other_size(len: usize) {
    let (raw, _) = synthetic();
    let mut sized = raw;
    sized.resize(len, 0);
    assert_eq!(decode(&sized).unwrap_err(), DecodeError::Size { len });
}

#[test]
fn decode_rejects_a_non_finite_lane_in_every_block() {
    for (index, lane) in BLOCKS {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for core in [0, 15] {
                let (mut raw, _) = synthetic();
                put_lane(&mut raw, index + core, value);
                assert_eq!(
                    decode(&raw).unwrap_err(),
                    DecodeError::NonFinite { lane, core },
                    "{lane} {value}"
                );
            }
        }
    }
}

#[test]
fn the_version_is_exactly_four_little_endian_bytes() {
    assert_eq!(parse_version(&VERSION_BYTES), Some(VERSION));
    assert_eq!(VERSION, 0x0062_0205);
}

#[test_case(b"0x620205\n"; "text")]
#[test_case(&[5, 2, 0x62]; "three bytes")]
#[test_case(&[5, 2, 0x62, 0, 0]; "five bytes")]
#[test_case(&[]; "empty")]
fn a_version_that_is_not_four_little_endian_bytes_is_refused(raw: &[u8]) {
    assert_eq!(parse_version(raw), None);
}

#[derive(Clone)]
struct Clockwork(Arc<AtomicU64>);

impl Clockwork {
    fn new() -> Self {
        Self(Arc::new(AtomicU64::new(0)))
    }

    fn advance(&self, by: Duration) {
        self.0
            .fetch_add(u64::try_from(by.as_nanos()).unwrap(), Ordering::SeqCst);
    }
}

impl PmClock for Clockwork {
    fn now(&self) -> Duration {
        Duration::from_nanos(self.0.load(Ordering::SeqCst))
    }
}

struct Block {
    file: PmFile,
    after: usize,
    gate: Gate,
}

#[derive(Clone, Default)]
struct Source {
    version: Arc<Mutex<Option<Vec<u8>>>>,
    table: Arc<Mutex<Option<Vec<u8>>>>,
    version_reads: Arc<AtomicUsize>,
    table_reads: Arc<AtomicUsize>,
    block: Arc<Mutex<Option<Block>>>,
}

impl Source {
    fn with(version: Option<&[u8]>, table: Option<&[u8]>) -> Self {
        let source = Self::default();
        source.set(version, table);
        source
    }

    fn good() -> Self {
        Self::with(Some(&VERSION_BYTES), Some(&synthetic().0))
    }

    fn set(&self, version: Option<&[u8]>, table: Option<&[u8]>) {
        *lock(&self.version) = version.map(<[u8]>::to_vec);
        *lock(&self.table) = table.map(<[u8]>::to_vec);
    }

    fn block(&self, file: PmFile, after: usize) -> Gate {
        let gate = Gate::default();
        *lock(&self.block) = Some(Block {
            file,
            after,
            gate: gate.clone(),
        });
        gate
    }

    fn reads(&self, file: PmFile) -> usize {
        match file {
            PmFile::Version => self.version_reads.load(Ordering::SeqCst),
            PmFile::Table => self.table_reads.load(Ordering::SeqCst),
        }
    }

    fn wait_for_reads(&self, file: PmFile, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.reads(file) < count {
            assert!(Instant::now() < deadline, "the worker never read {file:?}");
            thread::yield_now();
        }
    }
}

impl PmSource for Source {
    fn read(&self, file: PmFile) -> io::Result<Vec<u8>> {
        let (counter, content) = match file {
            PmFile::Version => (&self.version_reads, &self.version),
            PmFile::Table => (&self.table_reads, &self.table),
        };
        let nth = counter.fetch_add(1, Ordering::SeqCst) + 1;
        let gate = lock(&self.block)
            .as_ref()
            .filter(|block| block.file == file && nth > block.after)
            .map(|block| block.gate.clone());
        if let Some(gate) = gate {
            gate.wait();
        }
        lock(content)
            .clone()
            .ok_or_else(|| io::Error::from(ErrorKind::NotFound))
    }
}

fn cores() -> Vec<CoreInfo> {
    (0..16)
        .map(|index| CoreInfo {
            core: core(index),
            ccd: CcdId::try_from(index / 8).unwrap(),
            cpus: Vec::new(),
        })
        .collect()
}

fn start(source: &Source, clock: impl PmClock + 'static, timing: Timing) -> PmReader {
    PmReader::start(
        Box::new(source.clone()),
        layout(&cores()),
        Box::new(clock),
        timing,
    )
    .unwrap()
}

fn fake_timing() -> Timing {
    Timing {
        budget: BUDGET,
        max_age: Duration::from_secs(2),
    }
}

fn settle(reader: &PmReader) {
    assert!(
        reader.wait_idle(Duration::from_secs(10)),
        "read never finished"
    );
}

fn long_table() -> Vec<u8> {
    let mut long = synthetic().0;
    long.push(0);
    long
}

#[test_case(None, None, "table version unreadable"; "missing files")]
#[test_case(Some(&VERSION_BYTES[..]), None, "table unreadable"; "missing table")]
#[test_case(
    Some(&[0x05, 0x01, 0x62, 0x00][..]),
    Some(&synthetic().0[..]),
    "unsupported version";
    "wrong version"
)]
#[test_case(
    Some(&b"0x620205\n"[..]),
    Some(&synthetic().0[..]),
    "table version invalid";
    "text version"
)]
#[test_case(
    Some(&VERSION_BYTES[..3]),
    Some(&synthetic().0[..]),
    "table version invalid";
    "short version"
)]
#[test_case(
    Some(&[5, 2, 0x62, 0, 0][..]),
    Some(&synthetic().0[..]),
    "table version invalid";
    "long version"
)]
#[test_case(
    Some(&VERSION_BYTES[..]),
    Some(&synthetic().0[..100]),
    "table is 100 bytes";
    "short read"
)]
#[test_case(
    Some(&VERSION_BYTES[..]),
    Some(&long_table()[..]),
    "table is 2453 bytes";
    "wrong size"
)]
fn reads_cover_the_version_and_table_failures(
    version: Option<&[u8]>,
    table: Option<&[u8]>,
    reason: &str,
) {
    let source = Source::with(version, table);
    let reader = start(&source, Clockwork::new(), fake_timing());
    settle(&reader);
    assert!(reader.pm_table().is_none());
    settle(&reader);
    let check = reader.check();
    assert!(check.ok);
    assert_eq!(check.name, "pm_table");
    assert!(check.detail.contains(reason), "{}", check.detail);
}

#[test]
fn a_failed_refresh_drops_the_old_lanes() {
    let (_, want) = synthetic();
    let source = Source::good();
    let reader = start(&source, Clockwork::new(), fake_timing());
    settle(&reader);
    assert_eq!(reader.pm_table(), Some(want));
    settle(&reader);
    *lock(&source.table) = None;
    assert_eq!(reader.pm_table(), Some(want));
    settle(&reader);
    assert!(
        reader.pm_table().is_none(),
        "a failed refresh kept the old lanes"
    );
}

#[test]
fn a_decoded_table_is_reported_with_its_version() {
    let reader = start(&Source::good(), Clockwork::new(), fake_timing());
    settle(&reader);
    let check = reader.check();
    assert!(check.ok);
    assert_eq!(
        check.detail,
        "pm_table version 0x620205: decoding per-core power, voltage request, temperature and C-state lanes"
    );
}

#[test]
fn layout_accepts_cores_0_to_15_on_ccd_index_over_8() {
    assert_eq!(layout(&cores()), Ok(()));
    assert_eq!(layout(&cores()[..8]), Err(Unavailable::CoreCount(8)));
}

fn noncontiguous_topology() -> Vec<CoreInfo> {
    let mut duplicated = cores();
    duplicated[9].core = core(10);
    duplicated
}

fn swapped_topology() -> Vec<CoreInfo> {
    let mut swapped = cores();
    swapped.swap(0, 1);
    swapped
}

#[test_case(&noncontiguous_topology(); "noncontiguous")]
#[test_case(&reversed_topology(); "reversed CCDs")]
#[test_case(&swapped_topology(); "wrong slot order")]
fn layout_refuses_a_topology_that_is_not_cores_0_to_15_on_ccd_index_over_8(topology: &[CoreInfo]) {
    assert_eq!(layout(topology), Err(Unavailable::Topology));
}

fn reversed_topology() -> Vec<CoreInfo> {
    let mut reversed = cores();
    for info in &mut reversed {
        info.ccd = CcdId::new(1 - i64::from(info.ccd.get())).unwrap();
    }
    reversed
}

#[test_case(&reversed_topology(); "reversed CCDs")]
#[test_case(&cores()[..8]; "one CCD only")]
fn an_unsupported_topology_reads_the_version_but_no_payload(topology: &[CoreInfo]) {
    let source = Source::good();
    let reader = PmReader::start(
        Box::new(source.clone()),
        layout(topology),
        Box::new(Clockwork::new()),
        fake_timing(),
    )
    .unwrap();
    settle(&reader);
    let check = reader.check();
    assert!(
        check.ok && check.detail.contains("unsupported"),
        "{}",
        check.detail
    );
    assert!(reader.pm_table().is_none());
    settle(&reader);
    assert_eq!(source.reads(PmFile::Table), 0);
    assert!(source.reads(PmFile::Version) >= 1);
}

#[test_case(PmFile::Version; "version")]
#[test_case(PmFile::Table; "table")]
fn check_waits_the_read_budget_for_a_stuck_read_and_stays_informational(file: PmFile) {
    let source = Source::good();
    let gate = source.block(file, 0);
    let budget = Duration::from_millis(40);
    let created = Instant::now();
    let reader = start(
        &source,
        Monotonic::new(),
        Timing {
            budget,
            max_age: Duration::from_secs(2),
        },
    );
    assert!(reader.pm_table().is_none());
    assert!(created.elapsed() < budget, "pm_table waited for the read");
    source.wait_for_reads(file, 1);
    let check = reader.check();
    assert!(created.elapsed() >= budget, "{file:?}");
    assert!(
        check.ok && check.detail.contains("overdue"),
        "{}",
        check.detail
    );
    if file == PmFile::Table {
        assert!(check.detail.contains("0x620205"), "{}", check.detail);
    } else {
        assert!(
            check.detail.contains("version unavailable"),
            "{}",
            check.detail
        );
    }
    let again = Instant::now();
    for _ in 0..20 {
        assert!(reader.pm_table().is_none());
        assert!(reader.check().ok);
    }
    assert!(
        again.elapsed() < budget * 10,
        "an overdue read delayed its callers"
    );
    assert_eq!(source.reads(file), 1);
    gate.open();
}

#[test_case(PmFile::Version; "version")]
#[test_case(PmFile::Table; "table")]
fn a_stuck_read_is_never_replaced_and_its_late_result_is_discarded(file: PmFile) {
    let source = Source::good();
    let gate = source.block(file, 0);
    let clock = Clockwork::new();
    let reader = start(&source, clock.clone(), fake_timing());
    source.wait_for_reads(file, 1);
    assert!(reader.pm_table().is_none());
    clock.advance(Duration::from_secs(10));
    for _ in 0..20 {
        assert!(reader.pm_table().is_none());
        let check = reader.check();
        assert!(
            check.ok && check.detail.contains("overdue"),
            "{}",
            check.detail
        );
        if file == PmFile::Table {
            assert!(check.detail.contains("0x620205"), "{}", check.detail);
        }
    }
    assert_eq!(
        source.reads(file),
        1,
        "{file:?}: a replacement read started"
    );

    gate.open();
    settle(&reader);
    assert!(
        reader.pm_table().is_none(),
        "{file:?}: the late result was accepted"
    );
    settle(&reader);
    assert_eq!(reader.pm_table(), Some(synthetic().1), "{file:?}");
    settle(&reader);
}

#[test_case(Duration::from_secs(1), true; "within the window")]
#[test_case(Duration::from_secs(2), true; "at the window edge")]
#[test_case(Duration::from_secs(2) + Duration::from_nanos(1), false; "past the window")]
fn a_cached_table_is_reused_for_two_seconds_from_its_read_start(age: Duration, reused: bool) {
    let (_, want) = synthetic();
    let source = Source::good();
    let clock = Clockwork::new();
    let reader = start(&source, clock.clone(), fake_timing());
    settle(&reader);
    let gate = source.block(PmFile::Table, source.reads(PmFile::Table));
    clock.advance(age);
    assert_eq!(reader.pm_table(), reused.then_some(want), "{age:?}");
    source.wait_for_reads(PmFile::Table, 2);
    clock.advance(BUDGET);
    assert!(
        reader.pm_table().is_none(),
        "{age:?}: an overdue refresh reused old lanes"
    );
    gate.open();
    settle(&reader);
}

#[test]
fn check_refreshes_a_completed_read() {
    let source = Source::good();
    let reader = start(&source, Clockwork::new(), fake_timing());
    settle(&reader);
    source.set(None, None);
    let check = reader.check();
    assert!(check.ok);
    assert!(
        check.detail.contains("version unavailable")
            && check.detail.contains("per-core lanes absent"),
        "{}",
        check.detail
    );
    assert!(reader.pm_table().is_none());
    settle(&reader);
}

#[test]
fn dropping_the_reader_does_not_wait_for_a_stuck_read() {
    let source = Source::good();
    let gate = source.block(PmFile::Version, 0);
    let reader = start(&source, Clockwork::new(), fake_timing());
    source.wait_for_reads(PmFile::Version, 1);
    drop(reader);
    gate.open();
}

#[test]
fn the_reader_is_shared_with_the_trial_thread() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PmReader>();
    let source = Source::good();
    let reader = start(&source, Clockwork::new(), fake_timing());
    settle(&reader);
    let seen = thread::scope(|scope| {
        scope
            .spawn(|| (&reader as &dyn PmTableReader).pm_table())
            .join()
            .unwrap()
    });
    assert_eq!(seen, Some(synthetic().1));
}

/// A clock that records how long `check` is willing to wait instead of waiting, so the bound is
/// asserted exactly and without real time.
#[derive(Clone)]
struct Probe {
    clock: Clockwork,
    waits: Arc<Mutex<Vec<Duration>>>,
}

impl PmClock for Probe {
    fn now(&self) -> Duration {
        self.clock.now()
    }

    fn wait<'a>(
        &self,
        _changed: &Condvar,
        state: MutexGuard<'a, State>,
        limit: Duration,
    ) -> MutexGuard<'a, State> {
        lock(&self.waits).push(limit);
        state
    }
}

#[test_case(PmFile::Version; "version")]
#[test_case(PmFile::Table; "table")]
fn check_never_waits_longer_than_what_is_left_of_the_read_budget(file: PmFile) {
    let source = Source::good();
    let gate = source.block(file, 0);
    let probe = Probe {
        clock: Clockwork::new(),
        waits: Arc::default(),
    };
    let reader = start(&source, probe.clone(), fake_timing());
    source.wait_for_reads(file, 1);
    for step in [
        Duration::ZERO,
        Duration::from_millis(30),
        Duration::from_millis(70),
        Duration::from_secs(3600),
    ] {
        probe.clock.advance(step);
        assert!(reader.check().ok);
    }
    assert_eq!(
        *lock(&probe.waits),
        [
            BUDGET,
            Duration::from_millis(70),
            Duration::ZERO,
            Duration::ZERO
        ],
        "{file:?}: the wait was not the remaining budget"
    );
    gate.open();
}

fn pm_file(tree: &Tree, name: &str, bytes: &[u8]) -> FileSource {
    tree.put_bytes(&format!("{DRIVER_DIR}/{name}"), bytes);
    FileSource::new(tree.root(), tree.source())
}

#[test_case(0, 0; "empty")]
#[test_case(3, 3; "three bytes")]
#[test_case(4, 4; "exact length")]
#[test_case(5, 5; "five bytes")]
#[test_case(6, 5; "six bytes is cut")]
#[test_case(64 * 1024, 5; "sixty-four kibibytes is cut")]
fn the_file_source_reads_at_most_one_byte_past_the_version_length(len: usize, returned: usize) {
    let tree = Tree::new(8, true);
    let bytes = if len == 4 {
        VERSION_BYTES.to_vec()
    } else {
        vec![0xab; len]
    };
    let source = pm_file(&tree, "pm_table_version", &bytes);
    let read = source.read(PmFile::Version).unwrap();
    assert_eq!(read.len(), returned, "{len} input bytes");
    assert_eq!(
        parse_version(&read),
        (len == 4).then_some(VERSION),
        "{len} input bytes"
    );
}

#[test_case(0, 0; "empty")]
#[test_case(100, 100; "short")]
#[test_case(TABLE_LEN - 1, TABLE_LEN - 1; "one byte short")]
#[test_case(TABLE_LEN, TABLE_LEN; "exact length")]
#[test_case(TABLE_LEN + 1, TABLE_LEN + 1; "one byte long")]
#[test_case(TABLE_LEN + 2, TABLE_LEN + 1; "two bytes long is cut")]
#[test_case(1 << 20, TABLE_LEN + 1; "a mebibyte is cut")]
fn the_file_source_reads_at_most_one_byte_past_the_table_length(len: usize, returned: usize) {
    let tree = Tree::new(8, true);
    let source = pm_file(&tree, "pm_table", &vec![0_u8; len]);
    let read = source.read(PmFile::Table).unwrap();
    assert_eq!(read.len(), returned, "{len} input bytes");
    match decode(&read) {
        Ok(_) => assert_eq!(len, TABLE_LEN),
        Err(error) => assert_eq!(error, DecodeError::Size { len: returned }),
    }
}

#[test_case(5; "version length plus one")]
#[test_case(TABLE_LEN + 1; "table length plus one")]
fn the_file_source_stops_reading_an_endless_reader(limit: usize) {
    let read = crate::files::read_limited(io::repeat(0), limit as u64, limit).unwrap();
    assert_eq!(read.len(), limit);
}
