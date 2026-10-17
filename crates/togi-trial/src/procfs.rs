use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use togi_journal::fs::Fs;
use togi_machine::CpuId;

use crate::id::ScopeName;

const EXCERPT: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Stat {
    pub(crate) state: char,
    /// `-1` for a task the kernel is tearing down: it no longer has a group to print.
    pub(crate) pgrp: i64,
    pub(crate) utime: u64,
    pub(crate) stime: u64,
    pub(crate) starttime: u64,
    pub(crate) processor: CpuId,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("malformed /proc stat ({reason}): {excerpt}")]
pub(crate) struct StatError {
    reason: &'static str,
    excerpt: String,
}

impl StatError {
    fn new(reason: &'static str, text: &str) -> Self {
        let mut end = text.len().min(EXCERPT);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let mut excerpt = format!("{:?}", &text[..end]);
        if text.len() > EXCERPT {
            excerpt.push_str(" (truncated)");
        }
        Self { reason, excerpt }
    }
}

fn field(fields: &[&str], number: usize, text: &str) -> Result<u64, StatError> {
    let value = fields[number - 3];
    match value.parse::<i64>() {
        Ok(value) if value >= 0 => Ok(value.unsigned_abs()),
        Ok(_) => Err(StatError::new("negative field", text)),
        Err(_) => Err(StatError::new("non-numeric field", text)),
    }
}

fn signed_field(fields: &[&str], number: usize, text: &str) -> Result<i64, StatError> {
    fields[number - 3]
        .parse::<i64>()
        .map_err(|_| StatError::new("non-numeric field", text))
}

/// Splits after the last `)` so a command name may hold spaces and parentheses; fields are named
/// by their number in proc(5).
pub(crate) fn parse_stat(text: &str) -> Result<Stat, StatError> {
    let close = text
        .rfind(')')
        .ok_or_else(|| StatError::new("no closing parenthesis", text))?;
    let fields: Vec<&str> = text[close + 1..].split_whitespace().collect();
    if fields.len() < 37 {
        return Err(StatError::new("too few fields", text));
    }
    let processor = field(&fields, 39, text)?;
    Ok(Stat {
        state: fields[0].chars().next().unwrap_or('?'),
        pgrp: signed_field(&fields, 5, text)?,
        utime: field(&fields, 14, text)?,
        stime: field(&fields, 15, text)?,
        starttime: field(&fields, 22, text)?,
        processor: i64::try_from(processor)
            .ok()
            .and_then(|processor| CpuId::new(processor).ok())
            .ok_or_else(|| StatError::new("processor out of range", text))?,
    })
}

/// Cumulative CPU time from utime and stime ticks; `None` on overflow.
pub(crate) fn cpu_time(stat: &Stat, ticks_per_second: u64) -> Option<Duration> {
    let ticks = stat.utime.checked_add(stat.stime)?;
    let nanos = u128::from(ticks) * 1_000_000_000 / u128::from(ticks_per_second.max(1));
    Some(Duration::new(
        u64::try_from(nanos / 1_000_000_000).ok()?,
        u32::try_from(nanos % 1_000_000_000).ok()?,
    ))
}

pub(crate) fn ticks_per_second() -> u64 {
    rustix::param::clock_ticks_per_second()
}

/// Whether some cgroup line ends in `/<scope>.scope`.
pub(crate) fn in_scope(cgroup: &str, scope: &ScopeName) -> bool {
    let suffix = format!("/{}", scope.unit());
    cgroup.lines().any(|line| line.ends_with(&suffix))
}

/// The last trial scope named by a path component of any cgroup line.
pub(crate) fn scope_of(cgroup: &str) -> Option<ScopeName> {
    cgroup
        .lines()
        .filter_map(|line| line.splitn(3, ':').nth(2))
        .flat_map(|path| path.split('/'))
        .filter_map(ScopeName::from_unit)
        .next_back()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Usage {
    pub(crate) cpu_time: Duration,
    pub(crate) cpu: CpuId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Thread {
    pub(crate) tid: u32,
    pub(crate) cpu: CpuId,
}

/// A `/proc` observation that did not happen. Only `Gone` is normal; every other variant is a lost
/// observation and keeps what caused it.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ProcError {
    /// The process or thread vanished while being read: normal, never a lost observation.
    #[error("process disappeared")]
    Gone,
    #[error("read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("list {}", path.display())]
    List {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("parse {}", path.display())]
    Stat {
        path: PathBuf,
        #[source]
        source: StatError,
    },
    #[error("task id {name:?} is not a thread")]
    NotThread { name: String },
    #[error("cpu time of {pid} overflows")]
    CpuTimeOverflow { pid: u32 },
    #[error("the /proc scan did not finish before its deadline")]
    Deadline,
    #[error("an earlier /proc scan is still blocked")]
    Stuck,
    #[error("the /proc scan thread failed before reporting")]
    ScanLost,
}

/// What a thread listing found: readable threads stay evidence even when others failed.
#[derive(Debug, Default)]
pub(crate) struct Threads {
    pub(crate) threads: Vec<Thread>,
    pub(crate) errors: Vec<ProcError>,
}

/// A process seen in a trial scope, identified by pid and start time and never signalled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScopeProcess {
    pub(crate) scope: ScopeName,
    pub(crate) pid: u32,
    pub(crate) starttime: u64,
}

fn disappeared(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(3)
}

/// Read-only inputs for procfs and sensors. Disk reads preserve the kernel's symlink semantics;
/// an injected filesystem lets content fixtures use the same readers without host effects.
#[derive(Clone, Debug, Default)]
pub(crate) struct Inputs {
    filesystem: Option<Arc<dyn Fs>>,
}

impl Inputs {
    #[cfg(test)]
    pub(crate) fn memory(filesystem: Arc<dyn Fs>) -> Self {
        Self {
            filesystem: Some(filesystem),
        }
    }

    #[cfg(test)]
    pub(crate) fn filesystem(&self) -> Option<&Arc<dyn Fs>> {
        self.filesystem.as_ref()
    }

    pub(crate) fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        match &self.filesystem {
            Some(filesystem) => filesystem.read(path),
            None => fs::read(path),
        }
    }

    pub(crate) fn read_to_string(&self, path: &Path) -> io::Result<String> {
        match &self.filesystem {
            Some(filesystem) => String::from_utf8(filesystem.read(path)?)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            None => fs::read_to_string(path),
        }
    }

    pub(crate) fn read_dir(&self, path: &Path) -> io::Result<Entries> {
        match &self.filesystem {
            Some(filesystem) => Ok(Entries::Memory {
                root: path.to_owned(),
                entries: filesystem.read_dir(path)?.into_iter(),
            }),
            None => fs::read_dir(path).map(Entries::Disk),
        }
    }
}

pub(crate) enum Entries {
    Disk(fs::ReadDir),
    Memory {
        root: PathBuf,
        entries: std::vec::IntoIter<togi_journal::fs::DirEntry>,
    },
}

pub(crate) enum Entry {
    Disk(fs::DirEntry),
    Memory(PathBuf),
}

impl Entry {
    pub(crate) fn path(&self) -> PathBuf {
        match self {
            Self::Disk(entry) => entry.path(),
            Self::Memory(path) => path.clone(),
        }
    }

    pub(crate) fn file_name(&self) -> std::ffi::OsString {
        match self {
            Self::Disk(entry) => entry.file_name(),
            Self::Memory(path) => path.file_name().unwrap_or_default().to_owned(),
        }
    }
}

impl Iterator for Entries {
    type Item = io::Result<Entry>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Disk(entries) => entries.next().map(|entry| entry.map(Entry::Disk)),
            Self::Memory { root, entries } => entries
                .next()
                .map(|entry| Ok(Entry::Memory(root.join(entry.name)))),
        }
    }
}

/// Reads `/proc`, rooted anywhere so fixtures can stand in.
#[derive(Clone, Debug)]
pub(crate) struct Proc {
    root: PathBuf,
    ticks: u64,
    inputs: Inputs,
}

impl Proc {
    #[cfg(test)]
    pub(crate) const fn new(root: PathBuf, ticks: u64) -> Self {
        Self::with_inputs(root, ticks, Inputs { filesystem: None })
    }

    pub(crate) const fn with_inputs(root: PathBuf, ticks: u64, inputs: Inputs) -> Self {
        Self {
            root,
            ticks,
            inputs,
        }
    }

    fn read(&self, path: &Path) -> Result<String, ProcError> {
        self.inputs
            .read(path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(|source| {
                if disappeared(&source) {
                    ProcError::Gone
                } else {
                    ProcError::Read {
                        path: path.to_owned(),
                        source,
                    }
                }
            })
    }

    fn stat(&self, relative: &str) -> Result<Stat, ProcError> {
        let path = self.root.join(relative);
        parse_stat(&self.read(&path)?).map_err(|source| ProcError::Stat { path, source })
    }

    pub(crate) fn usage(&self, pid: u32) -> Result<Usage, ProcError> {
        let stat = self.stat(&format!("{pid}/stat"))?;
        let cpu_time = cpu_time(&stat, self.ticks).ok_or(ProcError::CpuTimeOverflow { pid })?;
        Ok(Usage {
            cpu_time,
            cpu: stat.processor,
        })
    }

    pub(crate) fn threads(&self, pid: u32) -> Result<Threads, ProcError> {
        let directory = self.root.join(format!("{pid}/task"));
        let entries = self.inputs.read_dir(&directory).map_err(|source| {
            if disappeared(&source) {
                ProcError::Gone
            } else {
                ProcError::List {
                    path: directory.clone(),
                    source,
                }
            }
        })?;
        let mut found = Threads::default();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(source) if disappeared(&source) => continue,
                Err(source) => {
                    found.errors.push(ProcError::List {
                        path: directory.clone(),
                        source,
                    });
                    continue;
                }
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(tid) = name.parse::<u32>().ok().filter(|tid| *tid > 0) else {
                found.errors.push(ProcError::NotThread { name });
                continue;
            };
            match self.stat(&format!("{pid}/task/{tid}/stat")) {
                Ok(stat) => found.threads.push(Thread {
                    tid,
                    cpu: stat.processor,
                }),
                Err(ProcError::Gone) => {}
                Err(error) => found.errors.push(error),
            }
        }
        found.threads.sort_by_key(|thread| thread.tid);
        Ok(found)
    }

    pub(crate) fn in_scope(&self, pid: u32, scope: &ScopeName) -> Result<bool, ProcError> {
        let cgroup = self.read(&self.root.join(format!("{pid}/cgroup")))?;
        Ok(in_scope(&cgroup, scope))
    }

    /// Every live process whose cgroup path names a trial scope; a read failure other than a
    /// disappearance fails the whole scan, never an empty success.
    pub(crate) fn scope_processes(&self) -> Result<Vec<ScopeProcess>, ProcError> {
        let listed = |source| ProcError::List {
            path: self.root.clone(),
            source,
        };
        let entries = self.inputs.read_dir(&self.root).map_err(listed)?;
        let mut found = Vec::new();
        for entry in entries {
            let entry = entry.map_err(listed)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(pid) = name.parse::<u32>().ok().filter(|pid| *pid > 0) else {
                continue;
            };
            let cgroup = match self.read(&self.root.join(format!("{pid}/cgroup"))) {
                Ok(text) => text,
                Err(ProcError::Gone) => continue,
                Err(error) => return Err(error),
            };
            let Some(scope) = scope_of(&cgroup) else {
                continue;
            };
            let stat = match self.stat(&format!("{pid}/stat")) {
                Ok(stat) => stat,
                Err(ProcError::Gone) => continue,
                Err(error) => return Err(error),
            };
            if matches!(stat.state, 'Z' | 'X') {
                continue;
            }
            found.push(ScopeProcess {
                scope,
                pid,
                starttime: stat.starttime,
            });
        }
        found.sort_by(|a, b| (&a.scope, a.pid).cmp(&(&b.scope, b.pid)));
        Ok(found)
    }

    /// Alive means the same start time and a state other than zombie or dead.
    pub(crate) fn alive(&self, process: &ScopeProcess) -> Result<bool, ProcError> {
        match self.stat(&format!("{}/stat", process.pid)) {
            Ok(stat) => Ok(stat.starttime == process.starttime && !matches!(stat.state, 'Z' | 'X')),
            Err(ProcError::Gone) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    pub(super) fn stat_line(comm: &str, state: char, fields: &[(usize, &str)]) -> String {
        let mut values: Vec<String> = (3..=52).map(|_| "0".to_owned()).collect();
        values[0] = state.to_string();
        for (number, value) in fields {
            values[number - 3] = (*value).to_owned();
        }
        let mut text = format!("123 ({comm})");
        for value in values {
            write!(text, " {value}").unwrap();
        }
        text
    }

    fn scope(name: &str) -> ScopeName {
        ScopeName::from_unit(&format!("{name}.scope")).unwrap()
    }

    #[test]
    fn comm_with_spaces_and_parentheses_parses_and_cpu_time_uses_the_tick_rate() {
        let text = stat_line(
            "backend (worker)",
            'R',
            &[(14, "125"), (15, "25"), (39, "7")],
        );
        let stat = parse_stat(&text).unwrap();
        assert_eq!(
            (stat.utime, stat.stime, stat.processor),
            (125, 25, CpuId::from(7))
        );
        assert_eq!(cpu_time(&stat, 100), Some(Duration::from_millis(1500)));
        assert_eq!(cpu_time(&stat, 250), Some(Duration::from_millis(600)));
    }

    #[test]
    fn cpu_time_overflow_is_none() {
        let stat = Stat {
            state: 'R',
            pgrp: 1,
            utime: u64::MAX,
            stime: 1,
            starttime: 0,
            processor: CpuId::from(0),
        };
        assert_eq!(cpu_time(&stat, 100), None);
        let large = Stat {
            utime: u64::MAX,
            stime: 0,
            ..stat
        };
        assert_eq!(cpu_time(&large, 1), Some(Duration::from_secs(u64::MAX)));
    }

    #[test]
    fn malformed_stats_are_diagnosed_with_a_capped_quoted_excerpt() {
        assert!(
            parse_stat("no delimiter here")
                .unwrap_err()
                .to_string()
                .contains("\"no delimiter here\"")
        );
        assert!(
            parse_stat("1 (x) S 1 2")
                .unwrap_err()
                .to_string()
                .contains("too few")
        );
        let long = format!("1 (x) {}", "z ".repeat(400));
        let error = parse_stat(&long).unwrap_err().to_string();
        assert!(error.ends_with(" (truncated)"));
        assert!(error.len() < 400);
        let negative = stat_line("x", 'R', &[(14, "-1")]);
        assert!(
            parse_stat(&negative)
                .unwrap_err()
                .to_string()
                .contains("negative")
        );
        let wide = stat_line("x", 'R', &[(39, "65536")]);
        assert!(
            parse_stat(&wide)
                .unwrap_err()
                .to_string()
                .contains("(processor out of range)")
        );
        let bad = stat_line("x", 'R', &[(39, "cpu")]);
        assert!(
            parse_stat(&bad)
                .unwrap_err()
                .to_string()
                .contains("non-numeric")
        );
    }

    #[test]
    fn scope_membership_is_the_path_suffix() {
        let cgroup = "0::/system.slice/togi-trial-a.scope\n";
        assert!(in_scope(cgroup, &scope("togi-trial-a")));
        assert!(!in_scope(cgroup, &scope("togi-trial-b")));
        assert!(!in_scope(
            "0::/system.slice/xtogi-trial-a.scope\n",
            &scope("togi-trial-a")
        ));
    }

    #[test]
    fn a_component_with_spaces_is_a_different_cgroup_not_a_trial_scope() {
        assert_eq!(scope_of("0::/other/ togi-trial-a.scope /child\n"), None);
        assert_eq!(scope_of("0::/other/togi-trial-a.scope \n"), None);
        assert!(!in_scope(
            "0::/other/togi-trial-a.scope \n",
            &scope("togi-trial-a")
        ));
    }

    #[test]
    fn scope_discovery_takes_a_path_component_and_the_last_match() {
        let cgroup = "1:name=x:/togi-trial-old.scope\n0::/system.slice/togi-trial-a.scope/sub\n";
        assert_eq!(scope_of(cgroup), Some(scope("togi-trial-a")));
        assert_eq!(scope_of("0::/system.slice/togi-preflight-1.scope\n"), None);
        assert_eq!(scope_of("0::/xtogi-trial-a.scope\n"), None);
    }

    struct Fixture {
        fs: Arc<togi_journal::fs::MemFs>,
    }

    impl Fixture {
        fn path() -> &'static Path {
            Path::new("/proc-fixture")
        }

        fn mkdir(&self, relative: &str) {
            let path = Self::path().join(relative);
            for ancestor in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
                if self.fs.kind(ancestor).unwrap().is_none() {
                    self.fs.create_dir(ancestor).unwrap();
                }
            }
        }
    }

    fn fixture() -> (Fixture, Proc) {
        let dir = Fixture {
            fs: Arc::new(togi_journal::fs::MemFs::new()),
        };
        dir.mkdir("");
        let proc = Proc::with_inputs(
            Fixture::path().to_owned(),
            100,
            Inputs::memory(dir.fs.clone()),
        );
        (dir, proc)
    }

    fn write(dir: &Fixture, relative: &str, text: &str) {
        dir.fs.put(Fixture::path().join(relative), text.as_bytes());
    }

    fn disk_fixture() -> (tempfile::TempDir, Proc) {
        let dir = tempfile::tempdir().unwrap();
        let proc = Proc::new(dir.path().to_owned(), 100);
        (dir, proc)
    }

    fn disk_write(dir: &tempfile::TempDir, relative: &str, text: &str) {
        let path = dir.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn injected(source: &io::Error) -> bool {
        source.kind() == io::ErrorKind::Other && source.to_string() == "injected Read fault"
    }

    fn stat_reason(error: &ProcError) -> Option<&'static str> {
        if let ProcError::Stat { source, .. } = error {
            Some(source.reason)
        } else {
            None
        }
    }

    #[test]
    fn injected_read_and_listing_errors_are_lost_observations_that_keep_their_cause() {
        use togi_journal::fs::{Fault, Op};

        let (dir, proc) = fixture();
        write(&dir, "10/stat", &stat_line("w", 'R', &[]));
        dir.fs.inject(Fault::before(Op::Read, 1));
        let error = proc.usage(10).unwrap_err();
        assert!(
            matches!(&error, ProcError::Read { path, source }
                if path.ends_with("10/stat") && injected(source)),
            "{error:?}"
        );
        assert_eq!(
            togi_machine::error_chain(&error),
            "read /proc-fixture/10/stat: injected Read fault"
        );
        let cause = std::error::Error::source(&error).unwrap();
        assert!(cause.downcast_ref::<io::Error>().is_some_and(injected));
        dir.fs.clear_faults();
        dir.fs.inject(Fault::before(Op::Read, 1));
        assert!(matches!(
            proc.threads(10),
            Err(ProcError::List { path, source }) if path.ends_with("10/task") && injected(&source)
        ));
        dir.fs.clear_faults();
        dir.fs.inject(Fault::before(Op::Read, 1));
        assert!(matches!(
            proc.scope_processes(),
            Err(ProcError::List { path, source })
                if path == Fixture::path() && injected(&source)
        ));
    }

    #[test]
    fn proc_text_is_lossy_but_sensor_text_requires_utf8() {
        let (dir, proc) = fixture();
        let mut bytes = stat_line("w", 'R', &[(39, "3")]).into_bytes();
        bytes[5] = 0xff;
        let path = Fixture::path().join("10/stat");
        dir.fs.put(&path, bytes);
        assert_eq!(proc.usage(10).unwrap().cpu, CpuId::from(3));
        assert_eq!(
            proc.inputs.read_to_string(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn usage_distinguishes_missing_from_malformed() {
        let (dir, proc) = fixture();
        assert!(matches!(proc.usage(10), Err(ProcError::Gone)));
        write(
            &dir,
            "10/stat",
            &stat_line(
                "backend (worker)",
                'R',
                &[(14, "125"), (15, "25"), (39, "7")],
            ),
        );
        assert_eq!(
            proc.usage(10).unwrap(),
            Usage {
                cpu_time: Duration::from_millis(1500),
                cpu: CpuId::from(7)
            }
        );
        write(&dir, "11/stat", "11 (x) S");
        let malformed = proc.usage(11).unwrap_err();
        assert!(
            matches!(&malformed, ProcError::Stat { path, source }
                if path.ends_with("11/stat") && source.reason == "too few fields"),
            "{malformed:?}"
        );
        let cause = std::error::Error::source(&malformed).unwrap();
        assert_eq!(
            cause.downcast_ref::<StatError>().map(|stat| stat.reason),
            Some("too few fields")
        );
        assert_eq!(
            togi_machine::error_chain(&malformed),
            r#"parse /proc-fixture/11/stat: malformed /proc stat (too few fields): "11 (x) S""#
        );
        write(&dir, "12/stat", &stat_line("x", 'R', &[(15, "-4")]));
        assert_eq!(
            stat_reason(&proc.usage(12).unwrap_err()),
            Some("negative field")
        );
    }

    #[test]
    fn threads_keep_the_readable_ones_when_siblings_fail_or_vanish() {
        let (dir, proc) = fixture();
        write(&dir, "10/task/123/stat", &stat_line("w", 'R', &[(39, "3")]));
        dir.mkdir("10/task/124");
        write(&dir, "10/task/125/stat", "garbage");
        write(&dir, "10/task/abc/stat", "x");
        write(&dir, "10/task/0/stat", "x");
        let found = proc.threads(10).unwrap();
        assert_eq!(
            found.threads,
            [Thread {
                tid: 123,
                cpu: CpuId::from(3)
            }]
        );
        assert_eq!(found.errors.len(), 3);
        let not_threads: Vec<&str> = found
            .errors
            .iter()
            .filter_map(|error| {
                if let ProcError::NotThread { name } = error {
                    Some(name.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(not_threads.len(), 2);
        assert!(not_threads.contains(&"abc") && not_threads.contains(&"0"));
        assert!(found.errors.iter().any(|error| matches!(
            error,
            ProcError::Stat { path, .. } if path.ends_with("10/task/125/stat")
        )));
    }

    #[test]
    fn a_missing_task_directory_is_a_disappearance() {
        let (_dir, proc) = fixture();
        assert!(matches!(proc.threads(99), Err(ProcError::Gone)));
    }

    #[test]
    fn real_fs_a_task_entry_that_is_not_a_directory_is_lost_observation() {
        let (dir, proc) = disk_fixture();
        disk_write(&dir, "99/task", "");
        match proc.threads(99) {
            Err(ProcError::List { path, source }) => {
                assert!(path.ends_with("99/task"), "{}", path.display());
                assert_eq!(source.kind(), io::ErrorKind::NotADirectory, "{source}");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn scope_scan_captures_identity_skips_zombies_and_disappeared() {
        let (dir, proc) = fixture();
        write(&dir, "20/cgroup", "0::/a/togi-trial-x.scope\n");
        write(&dir, "20/stat", &stat_line("w", 'S', &[(22, "777")]));
        write(&dir, "21/cgroup", "0::/a/togi-trial-x.scope\n");
        write(&dir, "21/stat", &stat_line("w", 'Z', &[(22, "5")]));
        write(&dir, "22/cgroup", "0::/a/other.scope\n");
        write(&dir, "23/cgroup", "0::/a/togi-trial-y.scope\n");
        write(&dir, "self/cgroup", "0::/a/togi-trial-x.scope\n");
        let found = proc.scope_processes().unwrap();
        assert_eq!(
            found,
            [ScopeProcess {
                scope: scope("togi-trial-x"),
                pid: 20,
                starttime: 777
            }]
        );
        assert!(proc.alive(&found[0]).unwrap());
        let reused = ScopeProcess {
            starttime: 778,
            ..found[0].clone()
        };
        assert!(!proc.alive(&reused).unwrap());
    }

    #[test]
    fn scope_scan_fails_on_unreadable_data() {
        let (dir, proc) = fixture();
        write(&dir, "20/cgroup", "0::/a/togi-trial-x.scope\n");
        write(&dir, "20/stat", "broken");
        assert_eq!(
            stat_reason(&proc.scope_processes().unwrap_err()),
            Some("no closing parenthesis")
        );
        let missing = Proc::with_inputs(Fixture::path().join("nowhere"), 100, proc.inputs.clone());
        assert!(matches!(
            missing.scope_processes(),
            Err(ProcError::List { source, .. }) if source.kind() == io::ErrorKind::NotFound
        ));
    }

    fn readable_sibling(dir: &Fixture) {
        write(dir, "20/cgroup", "0::/a/togi-trial-x.scope\n");
        write(dir, "20/stat", &stat_line("w", 'S', &[(22, "777")]));
    }

    #[test]
    fn scope_scan_skips_a_vanished_cgroup_and_keeps_the_readable_siblings() {
        let (dir, proc) = fixture();
        readable_sibling(&dir);
        // A pid whose directory lost its files between listing and reading.
        dir.mkdir("40");
        let found = proc.scope_processes().unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pid, 20);
    }

    #[test]
    fn scope_scan_fails_when_the_cgroup_or_stat_is_not_a_readable_file() {
        for broken in ["cgroup", "stat"] {
            let (dir, proc) = fixture();
            readable_sibling(&dir);
            if broken == "stat" {
                write(&dir, "30/cgroup", "0::/a/togi-trial-y.scope\n");
            }
            dir.mkdir(&format!("30/{broken}"));
            let Err(ProcError::Read { path, source }) = proc.scope_processes() else {
                panic!("a {broken} that is a directory must fail the scan");
            };
            assert!(path.ends_with(format!("30/{broken}")), "{}", path.display());
            assert_eq!(source.kind(), io::ErrorKind::IsADirectory);
        }
    }

    #[test]
    fn real_fs_scope_scan_fails_on_permission_denied_but_not_on_a_vanished_file() {
        use std::os::unix::fs::PermissionsExt as _;

        for denied in ["cgroup", "stat"] {
            let (dir, proc) = disk_fixture();
            disk_write(&dir, "20/cgroup", "0::/a/togi-trial-x.scope\n");
            disk_write(&dir, "20/stat", &stat_line("w", 'S', &[(22, "777")]));
            disk_write(&dir, "30/cgroup", "0::/a/togi-trial-y.scope\n");
            disk_write(&dir, "30/stat", &stat_line("w", 'S', &[(22, "5")]));
            let path = dir.path().join(format!("30/{denied}"));
            fs::set_permissions(&path, fs::Permissions::from_mode(0o0)).unwrap();
            if fs::read(&path).is_ok() {
                // Running as root: permissions do not apply, so denial cannot be staged.
                return;
            }
            let Err(ProcError::Read { path, source }) = proc.scope_processes() else {
                panic!("an unreadable {denied} must fail the scan");
            };
            assert!(path.ends_with(format!("30/{denied}")), "{}", path.display());
            assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
            fs::remove_file(&path).unwrap();
            assert_eq!(proc.scope_processes().unwrap().len(), 1);
        }
    }

    #[test]
    fn only_a_missing_process_counts_as_gone_not_io_errors_or_denials() {
        assert!(disappeared(&io::Error::from(io::ErrorKind::NotFound)));
        assert!(disappeared(&io::Error::from_raw_os_error(3)));
        for errno in [5, 13, 21] {
            assert!(
                !disappeared(&io::Error::from_raw_os_error(errno)),
                "errno {errno} is a lost observation"
            );
        }
    }

    #[test]
    fn per_process_reads_fail_on_a_non_file_stat_and_threads_keep_readable_ones() {
        let (dir, proc) = fixture();
        dir.mkdir("50/stat");
        let directory = |error: &ProcError| {
            if let ProcError::Read { path, source } = error
                && source.kind() == io::ErrorKind::IsADirectory
            {
                Some(path.clone())
            } else {
                None
            }
        };
        let stat = Fixture::path().join("50/stat");
        assert_eq!(directory(&proc.usage(50).unwrap_err()), Some(stat.clone()));
        let process = ScopeProcess {
            scope: scope("togi-trial-x"),
            pid: 50,
            starttime: 1,
        };
        assert_eq!(directory(&proc.alive(&process).unwrap_err()), Some(stat));
        dir.mkdir("50/cgroup");
        assert_eq!(
            directory(&proc.in_scope(50, &process.scope).unwrap_err()),
            Some(Fixture::path().join("50/cgroup"))
        );
        write(&dir, "51/task/1/stat", &stat_line("w", 'R', &[(39, "2")]));
        dir.mkdir("51/task/2/stat");
        let found = proc.threads(51).unwrap();
        assert_eq!(
            found.threads,
            [Thread {
                tid: 1,
                cpu: CpuId::from(2)
            }]
        );
        let [error] = found.errors.as_slice() else {
            panic!("{:?}", found.errors);
        };
        assert_eq!(
            directory(error),
            Some(Fixture::path().join("51/task/2/stat"))
        );
    }

    #[test]
    fn alive_rejects_zombies_and_surfaces_read_errors() {
        let (dir, proc) = fixture();
        write(&dir, "30/stat", &stat_line("w", 'Z', &[(22, "9")]));
        let process = ScopeProcess {
            scope: scope("togi-trial-x"),
            pid: 30,
            starttime: 9,
        };
        assert!(!proc.alive(&process).unwrap());
        write(
            &dir,
            "30/stat",
            &stat_line("w", 'X', &[(5, "-1"), (6, "-1"), (22, "9")]),
        );
        assert!(
            !proc.alive(&process).unwrap(),
            "a task being torn down prints -1 for its group and session"
        );
        write(&dir, "30/stat", "broken");
        assert!(matches!(proc.alive(&process), Err(ProcError::Stat { .. })));
        assert!(!proc.alive(&ScopeProcess { pid: 31, ..process }).unwrap());
    }

    #[test]
    fn a_stat_excerpt_is_exactly_the_first_256_bytes_and_says_when_it_was_cut() {
        let message = |text: &str| parse_stat(text).unwrap_err().to_string();
        assert_eq!(
            message("123 (worker\n"),
            r#"malformed /proc stat (no closing parenthesis): "123 (worker\n""#
        );
        assert_eq!(
            message("123 (worker) R 1\n"),
            r#"malformed /proc stat (too few fields): "123 (worker) R 1\n""#
        );
        let exact = "x".repeat(256);
        assert_eq!(
            message(&exact),
            format!("malformed /proc stat (no closing parenthesis): \"{exact}\"")
        );
        assert_eq!(
            message(&"x".repeat(300)),
            format!("malformed /proc stat (no closing parenthesis): \"{exact}\" (truncated)")
        );
        assert_eq!(
            message(&format!("1 (w){}", "x".repeat(300))),
            format!(
                "malformed /proc stat (too few fields): \"1 (w){}\" (truncated)",
                "x".repeat(251)
            )
        );
        assert_eq!(
            message(&format!("{}é", "x".repeat(255))),
            format!(
                "malformed /proc stat (no closing parenthesis): \"{}\" (truncated)",
                "x".repeat(255)
            )
        );
    }

    #[test]
    fn a_captured_process_that_left_its_scope_stays_alive_until_its_start_time_changes() {
        let (dir, proc) = fixture();
        write(
            &dir,
            "20/cgroup",
            "0::/system.slice/togi-trial-x.scope/descendant\n",
        );
        write(
            &dir,
            "20/stat",
            &stat_line("name with spaces", 'S', &[(22, "100")]),
        );
        let found = proc.scope_processes().unwrap();
        assert_eq!(
            found,
            [ScopeProcess {
                scope: scope("togi-trial-x"),
                pid: 20,
                starttime: 100
            }]
        );
        write(&dir, "20/cgroup", "0::/elsewhere\n");
        assert!(proc.scope_processes().unwrap().is_empty());
        assert!(
            proc.alive(&found[0]).unwrap(),
            "leaving the scope does not end the captured process"
        );
        write(
            &dir,
            "20/stat",
            &stat_line("name with spaces", 'S', &[(22, "101")]),
        );
        assert!(
            !proc.alive(&found[0]).unwrap(),
            "a reused pid is not the captured process"
        );
    }

    #[test]
    fn each_numeric_stat_field_that_is_not_a_number_is_a_lost_observation_not_a_disappearance() {
        let (dir, proc) = fixture();
        for (pid, field) in [(60, 14), (61, 15), (62, 39)] {
            write(
                &dir,
                &format!("{pid}/stat"),
                &stat_line("w", 'R', &[(field, "bad")]),
            );
            assert_eq!(
                stat_reason(&proc.usage(pid).unwrap_err()),
                Some("non-numeric field"),
                "field {field}"
            );
        }
    }

    #[test]
    fn thread_ids_that_are_not_positive_numbers_are_lost_observations_beside_the_readable_one() {
        let (dir, proc) = fixture();
        for name in ["bad", "-1", "0"] {
            write(
                &dir,
                &format!("10/task/{name}/stat"),
                &stat_line("w", 'R', &[(39, "7")]),
            );
        }
        write(&dir, "10/task/123/stat", &stat_line("w", 'R', &[(39, "7")]));
        let found = proc.threads(10).unwrap();
        assert_eq!(
            found.threads,
            [Thread {
                tid: 123,
                cpu: CpuId::from(7)
            }]
        );
        let mut names: Vec<&str> = found
            .errors
            .iter()
            .map(|error| {
                let ProcError::NotThread { name } = error else {
                    panic!("{error:?}")
                };
                name.as_str()
            })
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["-1", "0", "bad"]);
    }

    #[test]
    fn a_scope_process_with_an_invalid_start_time_fails_the_scan() {
        let (dir, proc) = fixture();
        write(&dir, "42/cgroup", "0::/togi-trial-1.scope\n");
        for (start, reason) in [("-1", "negative field"), ("bad", "non-numeric field")] {
            write(&dir, "42/stat", &stat_line("w", 'S', &[(22, start)]));
            assert_eq!(
                stat_reason(&proc.scope_processes().unwrap_err()),
                Some(reason),
                "{start}"
            );
        }
        let process = ScopeProcess {
            scope: scope("togi-trial-1"),
            pid: 42,
            starttime: 100,
        };
        assert_eq!(
            stat_reason(&proc.alive(&process).unwrap_err()),
            Some("non-numeric field")
        );
    }
}
