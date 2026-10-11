use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crate::id::ScopeName;

const EXCERPT: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Stat {
    pub(crate) state: char,
    pub(crate) pgrp: u64,
    pub(crate) utime: u64,
    pub(crate) stime: u64,
    pub(crate) starttime: u64,
    pub(crate) processor: u32,
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
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
        pgrp: field(&fields, 5, text)?,
        utime: field(&fields, 14, text)?,
        stime: field(&fields, 15, text)?,
        starttime: field(&fields, 22, text)?,
        processor: u32::try_from(processor).map_err(|_| StatError::new("processor", text))?,
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
    pub(crate) cpu: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Thread {
    pub(crate) tid: u32,
    pub(crate) cpu: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ProcError {
    /// The process or thread vanished while being read: normal, never a lost observation.
    #[error("process disappeared")]
    Gone,
    #[error("{0}")]
    Observation(String),
}

/// What a thread listing found: readable threads stay evidence even when others failed.
#[derive(Debug, Default, PartialEq)]
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

/// Reads `/proc`, rooted anywhere so fixtures can stand in.
#[derive(Clone, Debug)]
pub(crate) struct Proc {
    root: PathBuf,
    ticks: u64,
}

impl Proc {
    pub(crate) const fn new(root: PathBuf, ticks: u64) -> Self {
        Self { root, ticks }
    }

    fn read(&self, relative: &str) -> Result<String, ProcError> {
        let path = self.root.join(relative);
        fs::read(&path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(|error| {
                if disappeared(&error) {
                    ProcError::Gone
                } else {
                    ProcError::Observation(format!("read {}: {error}", path.display()))
                }
            })
    }

    fn stat(&self, relative: &str) -> Result<Stat, ProcError> {
        parse_stat(&self.read(relative)?).map_err(|error| ProcError::Observation(error.to_string()))
    }

    pub(crate) fn usage(&self, pid: u32) -> Result<Usage, ProcError> {
        let stat = self.stat(&format!("{pid}/stat"))?;
        let cpu_time = cpu_time(&stat, self.ticks)
            .ok_or_else(|| ProcError::Observation(format!("cpu time of {pid} overflows")))?;
        Ok(Usage {
            cpu_time,
            cpu: stat.processor,
        })
    }

    pub(crate) fn threads(&self, pid: u32) -> Result<Threads, ProcError> {
        let directory = self.root.join(format!("{pid}/task"));
        let entries = fs::read_dir(&directory).map_err(|error| {
            if disappeared(&error) {
                ProcError::Gone
            } else {
                ProcError::Observation(format!("list {}: {error}", directory.display()))
            }
        })?;
        let mut found = Threads::default();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) if disappeared(&error) => continue,
                Err(error) => {
                    found.errors.push(ProcError::Observation(format!(
                        "list {}: {error}",
                        directory.display()
                    )));
                    continue;
                }
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(tid) = name.parse::<u32>().ok().filter(|tid| *tid > 0) else {
                found.errors.push(ProcError::Observation(format!(
                    "task id {name:?} is not a thread"
                )));
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
        Ok(in_scope(&self.read(&format!("{pid}/cgroup"))?, scope))
    }

    /// Every live process whose cgroup path names a trial scope; a read failure other than a
    /// disappearance fails the whole scan, never an empty success.
    pub(crate) fn scope_processes(&self) -> Result<Vec<ScopeProcess>, ProcError> {
        let entries = fs::read_dir(&self.root).map_err(|error| {
            ProcError::Observation(format!("list {}: {error}", self.root.display()))
        })?;
        let mut found = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                ProcError::Observation(format!("list {}: {error}", self.root.display()))
            })?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(pid) = name.parse::<u32>().ok().filter(|pid| *pid > 0) else {
                continue;
            };
            let cgroup = match self.read(&format!("{pid}/cgroup")) {
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
        assert_eq!((stat.utime, stat.stime, stat.processor), (125, 25, 7));
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
            processor: 0,
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
        assert!(!in_scope("0::/other/togi-trial-a.scope \n", &scope("togi-trial-a")));
    }

    #[test]
    fn scope_discovery_takes_a_path_component_and_the_last_match() {
        let cgroup = "1:name=x:/togi-trial-old.scope\n0::/system.slice/togi-trial-a.scope/sub\n";
        assert_eq!(scope_of(cgroup), Some(scope("togi-trial-a")));
        assert_eq!(scope_of("0::/system.slice/togi-preflight-1.scope\n"), None);
        assert_eq!(scope_of("0::/xtogi-trial-a.scope\n"), None);
    }

    fn fixture() -> (tempfile::TempDir, Proc) {
        let dir = tempfile::tempdir().unwrap();
        let proc = Proc::new(dir.path().to_owned(), 100);
        (dir, proc)
    }

    fn write(dir: &tempfile::TempDir, relative: &str, text: &str) {
        let path = dir.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn usage_distinguishes_missing_from_malformed() {
        let (dir, proc) = fixture();
        assert_eq!(proc.usage(10), Err(ProcError::Gone));
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
            proc.usage(10),
            Ok(Usage {
                cpu_time: Duration::from_millis(1500),
                cpu: 7
            })
        );
        write(&dir, "11/stat", "11 (x) S");
        assert!(matches!(proc.usage(11), Err(ProcError::Observation(_))));
        write(&dir, "12/stat", &stat_line("x", 'R', &[(15, "-4")]));
        assert!(matches!(proc.usage(12), Err(ProcError::Observation(_))));
    }

    #[test]
    fn threads_keep_the_readable_ones_when_siblings_fail_or_vanish() {
        let (dir, proc) = fixture();
        write(&dir, "10/task/123/stat", &stat_line("w", 'R', &[(39, "3")]));
        fs::create_dir_all(dir.path().join("10/task/124")).unwrap();
        write(&dir, "10/task/125/stat", "garbage");
        write(&dir, "10/task/abc/stat", "x");
        write(&dir, "10/task/0/stat", "x");
        let found = proc.threads(10).unwrap();
        assert_eq!(found.threads, [Thread { tid: 123, cpu: 3 }]);
        assert_eq!(found.errors.len(), 3);
    }

    #[test]
    fn a_missing_task_directory_is_a_disappearance() {
        let (_dir, proc) = fixture();
        assert_eq!(proc.threads(99), Err(ProcError::Gone));
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
        assert_eq!(proc.alive(&found[0]), Ok(true));
        let reused = ScopeProcess {
            starttime: 778,
            ..found[0].clone()
        };
        assert_eq!(proc.alive(&reused), Ok(false));
    }

    #[test]
    fn scope_scan_fails_on_unreadable_data() {
        let (dir, proc) = fixture();
        write(&dir, "20/cgroup", "0::/a/togi-trial-x.scope\n");
        write(&dir, "20/stat", "broken");
        assert!(matches!(
            proc.scope_processes(),
            Err(ProcError::Observation(_))
        ));
        let missing = Proc::new(dir.path().join("nowhere"), 100);
        assert!(matches!(
            missing.scope_processes(),
            Err(ProcError::Observation(_))
        ));
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
        assert_eq!(proc.alive(&process), Ok(false));
        write(&dir, "30/stat", "broken");
        assert!(proc.alive(&process).is_err());
        assert_eq!(proc.alive(&ScopeProcess { pid: 31, ..process }), Ok(false));
    }
}
