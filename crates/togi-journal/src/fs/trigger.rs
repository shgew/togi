use std::cell::Cell;
use std::rc::Rc;

use super::{Fault, MemFs, Op};
use crate::Event;
use crate::dir::EVENTS_FILE;

/// Where an interruption at a selected journal append takes effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppendPoint {
    /// The append fails before writing its line, so the journal ends one line short of it.
    BeforeWrite,
    /// The append writes its line and then fails to sync it, so the line is on disk but the append reports a failure.
    BeforeSync,
    /// The append completes; the caller interrupts when [`AppendTrigger::lands`] reports its event.
    Landed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Waiting,
    /// Armed for a boot whose `nth` append is the selected one.
    Armed {
        nth: u64,
    },
    /// Fired during the boot it was armed for, which has not been disarmed yet.
    Hit,
    Fired,
}

/// An interruption at the journal's `ordinal`th line, counted from 1 across every boot, that fires once.
///
/// A test arms it before each boot with the lines the journal already holds and disarms it after the boot; the
/// trigger works out which append of that boot is the selected one and whether it fired. What the interruption does
/// (a crash, a power loss, a failed I/O operation) stays with the caller. Clones share one trigger, so a boot's hooks
/// and the test driving it see the same state.
#[derive(Clone, Debug)]
pub struct AppendTrigger {
    ordinal: usize,
    point: AppendPoint,
    state: Rc<Cell<State>>,
}

impl AppendTrigger {
    /// # Panics
    ///
    /// If `ordinal` is 0: lines count from 1.
    pub fn new(ordinal: usize, point: AppendPoint) -> Self {
        assert!(ordinal > 0, "journal lines count from 1");
        Self {
            ordinal,
            point,
            state: Rc::new(Cell::new(State::Waiting)),
        }
    }

    /// Arms the trigger for a boot whose journal starts with `lines` lines, injecting into `fs` the fault its point
    /// needs. Arms nothing and returns false once the trigger fired or when the journal already holds the line.
    ///
    /// # Panics
    ///
    /// If the trigger is still armed for an earlier boot.
    pub fn arm(&self, fs: &MemFs, lines: usize) -> bool {
        match self.state.get() {
            State::Fired => return false,
            State::Armed { .. } | State::Hit => {
                panic!("the trigger was not disarmed after its last boot")
            }
            State::Waiting => {}
        }
        if self.ordinal <= lines {
            return false;
        }
        let nth = (self.ordinal - lines) as u64;
        if let Some(fault) = self.fault(nth) {
            fs.inject(fault);
        }
        self.state.set(State::Armed { nth });
        true
    }

    /// Whether `event`, just appended, is the selected line of an armed [`AppendPoint::Landed`] trigger; true once.
    ///
    /// # Panics
    ///
    /// If the trigger's point is a fault, which fires inside the append rather than after it.
    pub fn lands(&self, event: &Event) -> bool {
        assert_eq!(
            self.point,
            AppendPoint::Landed,
            "only a landed trigger fires on an appended event"
        );
        let selected = matches!(self.state.get(), State::Armed { .. })
            && event.seq.get() == self.ordinal as u64;
        if selected {
            self.state.set(State::Hit);
        }
        selected
    }

    /// Ends the boot the trigger was armed for, withdrawing its fault from `fs` if it never fired. True when the
    /// interruption fired during that boot; false when the boot was not armed.
    pub fn disarm(&self, fs: &MemFs) -> bool {
        let fired = match self.state.get() {
            State::Waiting | State::Fired => return false,
            State::Hit => true,
            State::Armed { nth } => self.fault(nth).is_some_and(|fault| !fs.withdraw(&fault)),
        };
        self.state
            .set(if fired { State::Fired } else { State::Waiting });
        fired
    }

    /// Whether the interruption has fired, during the current boot or an earlier one.
    pub fn fired(&self) -> bool {
        matches!(self.state.get(), State::Hit | State::Fired)
    }

    /// Whether a journal holding `lines` lines got past the selected line: the trigger fired, or the line was
    /// already there.
    pub fn reached(&self, lines: usize) -> bool {
        self.fired() || self.ordinal <= lines
    }

    fn fault(&self, nth: u64) -> Option<Fault> {
        let fault = match self.point {
            AppendPoint::BeforeWrite => Fault::before(Op::Write, nth),
            AppendPoint::BeforeSync => Fault::before(Op::Sync, nth),
            AppendPoint::Landed => return None,
        };
        Some(fault.named(EVENTS_FILE))
    }
}

#[cfg(test)]
mod tests;
