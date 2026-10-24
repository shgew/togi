use std::num::ParseIntError;

use togi_machine::{CpuId, KernelError, KernelErrorKind, ProcessFault, ProcessFaultKind};

use crate::{CpuCoreMap, Message};

#[derive(Debug, thiserror::Error)]
#[error("process fault {field} {text:?} is outside its numeric range")]
pub struct FaultNumberError {
    pub field: &'static str,
    pub text: String,
    #[source]
    pub source: ParseIntError,
}

#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct FaultParseFailure {
    pub faults: Vec<ProcessFault>,
    pub error: FaultNumberError,
}

/// Decode x86 userspace trap formats, retaining only explicit identity and CPU evidence.
pub fn process_faults(
    messages: &[Message],
    cores: &CpuCoreMap,
) -> Result<Vec<ProcessFault>, FaultParseFailure> {
    let mut faults = Vec::new();
    for message in messages {
        match process_fault(message, cores) {
            Ok(Some(fault)) => faults.push(fault),
            Ok(None) => {}
            Err(error) => return Err(FaultParseFailure { faults, error }),
        }
    }
    Ok(faults)
}

/// The oopses, RCU stalls and soft lockups the kernel reported. An x86 `kernel BUG at` line comes with its own
/// `Oops:` line, so only that one counts.
pub fn kernel_errors(messages: &[Message]) -> Vec<KernelError> {
    messages
        .iter()
        .filter_map(|message| {
            let text = message.text.as_str();
            let kind = if text.contains("Oops:") {
                KernelErrorKind::Oops
            } else if text.contains("detected stall") {
                KernelErrorKind::RcuStall
            } else if text.contains("BUG: soft lockup") {
                KernelErrorKind::SoftLockup
            } else {
                return None;
            };
            Some(KernelError {
                kind,
                monotonic: message.monotonic,
                lines: vec![message.text.clone()],
            })
        })
        .collect()
}

fn process_fault(
    message: &Message,
    cores: &CpuCoreMap,
) -> Result<Option<ProcessFault>, FaultNumberError> {
    let text = message.text.as_str();
    let signal = |number: &str| {
        text.split_once("signal ").is_some_and(|(_, tail)| {
            tail.split(|c: char| !c.is_ascii_digit()).next() == Some(number)
        })
    };
    let kind = if text.contains("segfault at ") || signal("11") {
        ProcessFaultKind::Segmentation
    } else if text.contains("general protection fault") {
        ProcessFaultKind::GeneralProtection
    } else if text.contains("invalid opcode") || signal("4") {
        ProcessFaultKind::IllegalInstruction
    } else if text.contains("bus error") || signal("7") {
        ProcessFaultKind::Bus
    } else if signal("6") || text.contains("trap abort") {
        ProcessFaultKind::Abort
    } else {
        return Ok(None);
    };
    let identity = text.strip_prefix("traps: ").unwrap_or(text);
    let Some((name, after)) = identity.split_once('[') else {
        return Ok(None);
    };
    let Some((pid, _)) = after.split_once(']') else {
        return Ok(None);
    };
    // Kernel oops headers also contain brackets. They are not comm[pid] userspace traps.
    if name.is_empty()
        || name.contains(char::is_whitespace)
        || pid.is_empty()
        || !pid.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Ok(None);
    }
    let pid = pid.parse::<u32>().map_err(|source| FaultNumberError {
        field: "pid",
        text: pid.to_owned(),
        source,
    })?;
    let cpu = text
        .split_once(" on CPU ")
        .map(|(_, tail)| {
            let number = tail
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap_or("");
            number
                .parse::<u16>()
                .map(CpuId::from)
                .map_err(|source| FaultNumberError {
                    field: "cpu",
                    text: number.to_owned(),
                    source,
                })
        })
        .transpose()?;
    Ok(Some(ProcessFault {
        pid,
        name: name.to_owned(),
        scope: None,
        kind,
        cpu,
        core: cpu.and_then(|cpu| cores.core(cpu)),
        monotonic: message.monotonic,
        lines: vec![message.text.clone()],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use togi_machine::ProcessFaultKind;

    #[test_case::test_case(
        "togi[1281]: segfault at 8 ip 000055 error 4 in togi[55+10] likely on CPU 3 (core 3, socket 0)",
        ProcessFaultKind::Segmentation,
        "togi",
        1281,
        Some(3)
    )]
    #[test_case::test_case(
        "traps: togi-dashboard[421] general protection fault ip:55 sp:77 error:0 in togi[55+10]",
        ProcessFaultKind::GeneralProtection,
        "togi-dashboard",
        421,
        None
    )]
    #[test_case::test_case(
        "traps: mprime[991] trap invalid opcode ip:55 sp:77 error:0 in mprime[55+10]",
        ProcessFaultKind::IllegalInstruction,
        "mprime",
        991,
        None
    )]
    #[test_case::test_case(
        "mprime[991]: bus error (signal 7)",
        ProcessFaultKind::Bus,
        "mprime",
        991,
        None
    )]
    #[test_case::test_case(
        "togi[432]: unhandled signal 6",
        ProcessFaultKind::Abort,
        "togi",
        432,
        None
    )]
    fn kernel_trap_examples(
        text: &str,
        kind: ProcessFaultKind,
        name: &str,
        pid: u32,
        cpu: Option<u16>,
    ) {
        let messages = [Message {
            time: jiff::Timestamp::UNIX_EPOCH,
            text: text.into(),
            monotonic: Duration::from_secs(3),
        }];
        let faults = process_faults(&messages, &CpuCoreMap::default()).unwrap();
        assert_eq!(faults.len(), 1);
        assert_eq!(faults[0].kind, kind);
        assert_eq!(faults[0].pid, pid);
        assert_eq!(faults[0].name, name);
        assert_eq!(faults[0].cpu, cpu.map(CpuId::from));
    }

    #[test_case::test_case("togi[999999999999999999999]: segfault at 8", "pid"; "pid overflow")]
    #[test_case::test_case("togi[31]: segfault at 8 on CPU 70000", "cpu"; "cpu overflow")]
    fn invalid_numeric_evidence_keeps_earlier_faults(text: &str, field: &str) {
        let message = |text: &str| Message {
            time: jiff::Timestamp::UNIX_EPOCH,
            text: text.into(),
            monotonic: Duration::from_secs(3),
        };
        let messages = [message("togi[31]: segfault at 8 on CPU 3"), message(text)];
        let failure = process_faults(&messages, &CpuCoreMap::default()).unwrap_err();
        assert_eq!(failure.faults.len(), 1);
        assert_eq!(failure.faults[0].cpu, Some(CpuId::from(3)));
        assert_eq!(failure.error.field, field);
    }

    #[test_case::test_case("general protection fault, probably for non-canonical address: 0x0 [#1] PREEMPT SMP"; "kernel oops is not a process trap")]
    #[test_case::test_case("togi[31]: unhandled signal 111"; "signal number is exact")]
    fn non_process_messages_do_not_guess_an_identity(text: &str) {
        let messages = [Message {
            time: jiff::Timestamp::UNIX_EPOCH,
            text: text.into(),
            monotonic: Duration::from_secs(3),
        }];
        assert_eq!(
            process_faults(&messages, &CpuCoreMap::default()).unwrap(),
            []
        );
    }

    #[test_case::test_case(
        "Oops: general protection fault, probably for non-canonical address 0xdead [#1] PREEMPT SMP NOPTI",
        Some(KernelErrorKind::Oops);
        "oops"
    )]
    #[test_case::test_case(
        "rcu: INFO: rcu_preempt self-detected stall on CPU",
        Some(KernelErrorKind::RcuStall);
        "self-detected RCU stall"
    )]
    #[test_case::test_case(
        "rcu: INFO: rcu_preempt detected stalls on CPUs/tasks:",
        Some(KernelErrorKind::RcuStall);
        "RCU stalls on other CPUs"
    )]
    #[test_case::test_case(
        "watchdog: BUG: soft lockup - CPU#3 stuck for 26s! [kworker/3:1:123]",
        Some(KernelErrorKind::SoftLockup);
        "soft lockup"
    )]
    #[test_case::test_case("kernel BUG at mm/slub.c:123!", None; "a BUG line comes with its own oops line")]
    #[test_case::test_case("amdgpu 0000:03:00.0: ring gfx_0.0.0 timeout", None; "an unrelated line")]
    fn kernel_error_examples(text: &str, kind: Option<KernelErrorKind>) {
        let messages = [Message {
            time: jiff::Timestamp::UNIX_EPOCH,
            text: text.into(),
            monotonic: Duration::from_secs(9),
        }];
        let errors = kernel_errors(&messages);
        assert_eq!(
            errors.iter().map(|error| error.kind).collect::<Vec<_>>(),
            kind.into_iter().collect::<Vec<_>>()
        );
        assert!(
            errors
                .iter()
                .all(|error| error.monotonic == Duration::from_secs(9) && error.lines == [text])
        );
    }
}
