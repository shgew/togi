//! The unclean-end question in words, shared by `togi status` and the dashboard: what togi saw of the crashed
//! boot and its successor, and the four answers the operator can give. Wording lives here once; callers lay the
//! lines out.

use std::fmt::Write as _;

use togi_journal::{DayAnswerKind, DayUncleanEnd, EndClass};
use togi_machine::{
    BootId, EndEvidence, GraphicsDriver, KernelFault, KernelTail, MceCount, ResetReason,
};

/// The four answers, in the order the question lists them.
pub const ANSWERS: [DayAnswerKind; 4] = DayAnswerKind::ALL;

/// What the operator would say for `kind`, as the question's choice reads.
#[must_use]
pub const fn choice_words(kind: DayAnswerKind) -> &'static str {
    match kind {
        DayAnswerKind::Frozen => "it froze",
        DayAnswerKind::Graphics => "graphics died but sound went on",
        DayAnswerKind::PowerOff => "I turned it off while it was working normally",
        DayAnswerKind::Unsure => "not sure",
    }
}

/// The argument `togi day answer` takes for `kind`.
#[must_use]
pub const fn answer_arg(kind: DayAnswerKind) -> &'static str {
    match kind {
        DayAnswerKind::Frozen => "frozen",
        DayAnswerKind::Graphics => "graphics",
        DayAnswerKind::PowerOff => "power-off",
        DayAnswerKind::Unsure => "unsure",
    }
}

/// The command that records `kind` for `boot`.
#[must_use]
pub fn answer_command(boot: &BootId, kind: DayAnswerKind) -> String {
    format!("togi day answer {} {}", boot.as_str(), answer_arg(kind))
}

/// The first eight characters of a boot id, as the journal's messages shorten it.
#[must_use]
pub fn short_boot(boot: &BootId) -> String {
    boot.as_str().chars().take(8).collect()
}

/// The four choices on one line: `1 it froze · 2 graphics died but sound went on · …`.
#[must_use]
pub fn choices_line() -> String {
    let mut out = String::new();
    for (at, kind) in ANSWERS.into_iter().enumerate() {
        if at > 0 {
            out.push_str(" · ");
        }
        let _ = write!(out, "{} {}", at + 1, choice_words(kind));
    }
    out
}

/// The answer words in the command's argument form: `frozen|graphics|power-off|unsure`.
#[must_use]
pub fn answer_args() -> String {
    let mut out = String::new();
    for (at, kind) in ANSWERS.into_iter().enumerate() {
        if at > 0 {
            out.push('|');
        }
        out.push_str(answer_arg(kind));
    }
    out
}

/// The dashboard's first row: the boot, the reset reason and togi's own reading, ending in the question.
#[must_use]
pub fn headline(end: &DayUncleanEnd) -> String {
    format!(
        "Boot {} stopped unclean; {}; reads as {}. What happened?",
        short_boot(&end.boot),
        reset_words(&end.reset_reason),
        class_words(end.class())
    )
}

/// What togi's own evidence points at while the operator has not answered.
#[must_use]
pub const fn class_words(class: EndClass) -> &'static str {
    match class {
        EndClass::CpuFault => "a CPU fault",
        EndClass::Thermal => "overheating",
        EndClass::GraphicsSurvived => "a graphics fault the machine outlived",
        EndClass::GraphicsLast => "a graphics fault that was the last thing logged",
        EndClass::Sleep => "a watchdog reset while the machine was going to sleep",
        EndClass::Watchdog => "a watchdog reset",
        EndClass::PowerButton => "a power-button shutdown",
        EndClass::PowerLoss => "a power loss",
        EndClass::KernelFault => "a kernel fault",
        EndClass::Unknown => "nothing conclusive",
    }
}

/// What togi saw, one fact per line: the reset reason, the machine checks of both boots and the kernel log tail.
#[must_use]
pub fn observed(end: &DayUncleanEnd) -> Vec<String> {
    let EndEvidence {
        crashed_mces,
        successor_mces,
        tail,
        asleep,
    } = &end.evidence;
    let mut facts = vec![
        reset_words(&end.reset_reason),
        format!("machine checks, crashed boot: {}", mce_words(crashed_mces)),
        format!("machine checks, next boot: {}", mce_words(successor_mces)),
        tail_words(tail),
    ];
    if let Some(asleep) = asleep {
        facts.push(format!("the log ended at {}", asleep.entry_words()));
    }
    facts
}

fn reset_words(reason: &ResetReason) -> String {
    let kind = reason.kind.map_or("unknown", |kind| kind.as_str());
    format!("reset reason: {kind}")
}

fn mce_words(count: &MceCount) -> String {
    match count {
        MceCount::Unknown {} => "unknown".to_owned(),
        MceCount::Read {
            corrected,
            uncorrected,
        } => format!("{corrected} corrected, {uncorrected} uncorrected"),
    }
}

fn tail_words(tail: &KernelTail) -> String {
    match tail {
        KernelTail::Missing {} => "kernel log tail: missing, so unknown".to_owned(),
        KernelTail::Unreadable {} => "kernel log tail: unreadable, so unknown".to_owned(),
        KernelTail::Quiet {} => "kernel log tail: no faults".to_owned(),
        KernelTail::Faults {
            faults,
            logging_continued,
        } => {
            let mut out = String::from("kernel log tail: ");
            for (at, fault) in faults.iter().enumerate() {
                if at > 0 {
                    out.push_str(", ");
                }
                fault_words(&mut out, *fault);
            }
            out.push_str(if *logging_continued {
                "; logging went on after the last"
            } else {
                "; nothing was logged after the last"
            });
            out
        }
    }
}

fn fault_words(out: &mut String, fault: KernelFault) {
    match fault {
        KernelFault::MachineCheck {} => out.push_str("machine check"),
        KernelFault::GraphicsTimeout { driver } => {
            let _ = write!(out, "{} ring timeout", driver_words(driver));
        }
        KernelFault::GraphicsReset { driver } => {
            let _ = write!(out, "{} reset", driver_words(driver));
        }
        KernelFault::NvidiaXid { code } => {
            let _ = write!(out, "nvidia Xid {code}");
        }
        KernelFault::Oops {} => out.push_str("oops"),
        KernelFault::HungTask {} => out.push_str("hung task"),
    }
}

const fn driver_words(driver: GraphicsDriver) -> &'static str {
    match driver {
        GraphicsDriver::Amdgpu => "amdgpu",
        GraphicsDriver::I915 => "i915",
        GraphicsDriver::Xe => "xe",
    }
}
