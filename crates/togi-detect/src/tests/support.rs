use std::ffi::OsString;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use togi_command::Output;
use togi_machine::{BootId, CcdId, CoreId, CoreInfo, CpuId};

use crate::runner::{JOURNALCTL_BUDGET, Journalctl, RunError};
use crate::{CpuCoreMap, Kernel, Message};

pub(super) const OLD: &str = "aaaaaaaabbbbccccddddeeeeeeeeeeee";
pub(super) const LATER: &str = "11111111222233334444555555555555";
pub(super) const CURRENT: &str = "99999999888877776666555555555555";
pub(super) const MISSING: &str = "0123456789abcdef0123456789abcdef";

pub(super) fn boot(id: &str) -> BootId {
    BootId::from(id.to_owned())
}

pub(super) fn core(number: i64) -> CoreId {
    CoreId::new(number).unwrap()
}

pub(super) fn topology(cores: &[(i64, &[u16])]) -> Vec<CoreInfo> {
    cores
        .iter()
        .map(|&(number, cpus)| CoreInfo {
            core: core(number),
            ccd: CcdId(0),
            cpus: cpus.iter().copied().map(CpuId).collect(),
        })
        .collect()
}

pub(super) fn cores(cores: &[(i64, &[u16])]) -> CpuCoreMap {
    CpuCoreMap::new(&topology(cores)).unwrap()
}

pub(super) fn at(micros: i64) -> Timestamp {
    Timestamp::from_microsecond(micros).unwrap()
}

pub(super) fn message(micros: i64, text: &str) -> Message {
    Message {
        time: at(micros),
        text: text.to_owned(),
        monotonic: Duration::ZERO,
    }
}

pub(super) fn stamped(message: &Message) -> String {
    let micros = message.time.as_microsecond();
    format!(
        "{}.{:06} {}",
        micros.div_euclid(1_000_000),
        micros.rem_euclid(1_000_000),
        message.text
    )
}

pub(super) fn fixture(name: &str) -> Vec<Message> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(name);
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| {
            let entry: serde_json::Value = serde_json::from_str(line).unwrap();
            message(
                entry["__REALTIME_TIMESTAMP"]
                    .as_str()
                    .unwrap()
                    .parse()
                    .unwrap(),
                entry["MESSAGE"].as_str().unwrap(),
            )
        })
        .collect()
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "a fake reply has the type of a real one"
)]
pub(super) fn exit(code: i32, stdout: &[u8], stderr: &str) -> Result<Output, RunError> {
    Ok(Output {
        status: ExitStatus::from_raw(code << 8),
        stdout: stdout.to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    })
}

pub(super) fn ok(stdout: impl AsRef<[u8]>) -> Result<Output, RunError> {
    exit(0, stdout.as_ref(), "")
}

pub(super) fn lines(entries: &[serde_json::Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for entry in entries {
        out.extend(serde_json::to_vec(entry).unwrap());
        out.push(b'\n');
    }
    out
}

pub(super) fn kernel_lines(texts: &[&str]) -> Vec<u8> {
    lines(
        &texts
            .iter()
            .map(|text| serde_json::json!({ "MESSAGE": text }))
            .collect::<Vec<_>>(),
    )
}

pub(super) type Reply = Box<dyn Fn() -> Result<Output, RunError> + Send>;

pub(super) type Calls = Arc<Mutex<Vec<Call>>>;

pub(super) fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Debug)]
pub(super) struct Call {
    pub(super) args: Vec<String>,
    pub(super) remaining: Duration,
}

struct Scripted<F> {
    calls: Calls,
    reply: F,
}

impl<F> Journalctl for Scripted<F>
where
    F: Fn(&[String]) -> Result<Output, RunError> + Send,
{
    fn run(&self, args: &[OsString], remaining: Duration) -> Result<Output, RunError> {
        let args: Vec<String> = args
            .iter()
            .map(|arg| arg.to_str().unwrap().to_owned())
            .collect();
        locked(&self.calls).push(Call {
            args: args.clone(),
            remaining,
        });
        (self.reply)(&args)
    }
}

pub(super) fn scripted<F>(
    topology: &[CoreInfo],
    pstore: impl Into<PathBuf>,
    budget: Duration,
    reply: F,
) -> (Kernel, Calls)
where
    F: Fn(&[String]) -> Result<Output, RunError> + Send + 'static,
{
    let calls = Calls::default();
    let runner = Scripted {
        calls: Arc::clone(&calls),
        reply,
    };
    let kernel = Kernel::with_parts(topology, Box::new(runner), budget, pstore.into()).unwrap();
    (kernel, calls)
}

pub(super) fn kernel<F>(cores: &[(i64, &[u16])], reply: F) -> (Kernel, Calls)
where
    F: Fn(&[String]) -> Result<Output, RunError> + Send + 'static,
{
    scripted(
        &topology(cores),
        "/nonexistent-togi-pstore",
        JOURNALCTL_BUDGET,
        reply,
    )
}

pub(super) fn argv(call: &Call) -> Vec<&str> {
    call.args.iter().map(String::as_str).collect()
}

pub(super) fn caused_by<'a, E: std::error::Error + 'static>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a E> {
    let mut next = Some(error);
    while let Some(error) = next {
        if let Some(found) = error.downcast_ref::<E>() {
            return Some(found);
        }
        next = error.source();
    }
    None
}
