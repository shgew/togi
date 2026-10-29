//! Durable operator pause state, distinct from the tuner's promotion-only freeze.

use togi_hardware::day::Observer;
use togi_journal::{
    DayFreeze, DayPause, FreezeChoice, FreezeSource, Journal, PauseSource, PauseState, Payload, Seq,
};
use togi_machine::{BootId, Clock};

use super::{Error, append, apply, socket};

pub(super) struct Pause {
    state: PauseState,
    boot: BootId,
    revision: Option<Seq>,
    frozen: bool,
}

impl Pause {
    pub(super) fn of(journal: &Journal) -> Self {
        let mut pause = Self {
            state: PauseState::Running {},
            boot: journal.boot().clone(),
            revision: None,
            frozen: false,
        };
        for event in journal.events() {
            if let Payload::DayPause(value) = &event.payload {
                pause.state = value.state;
                pause.boot.clone_from(&event.boot);
                pause.revision = Some(event.seq);
            }
            if let Payload::DayFreeze(value) = &event.payload {
                pause.frozen = value.choice == FreezeChoice::Freeze;
            }
        }
        pause
    }

    pub(super) fn starts_paused(&self, boot: &BootId, clock: &dyn Clock) -> bool {
        match &self.state {
            PauseState::UntilBoot {} => self.boot == *boot,
            PauseState::Until { deadline } => *deadline > clock.now(),
            PauseState::Running {} => false,
        }
    }

    pub(super) fn expired(&self, boot: &BootId, clock: &dyn Clock) -> Option<PauseSource> {
        match &self.state {
            PauseState::UntilBoot {} if self.boot != *boot => Some(PauseSource::ColdBoot {}),
            PauseState::Until { deadline } if *deadline <= clock.now() => {
                Some(PauseSource::Deadline {})
            }
            PauseState::Running {} | PauseState::UntilBoot {} | PauseState::Until { .. } => None,
        }
    }

    pub(super) fn status(&self) -> socket::ControlStatus {
        socket::ControlStatus {
            pause: self.state,
            frozen: self.frozen,
            revision: self.revision,
        }
    }

    fn persist(
        &mut self,
        journal: &mut Journal,
        clock: &dyn Clock,
        state: PauseState,
        source: PauseSource,
    ) -> Result<(), Error> {
        append(journal, clock, DayPause { state, source })?;
        self.state = state;
        self.boot = journal.boot().clone();
        self.revision = journal.events().last().map(|event| event.seq);
        Ok(())
    }

    /// Called only after the startup application or the ending transaction has verified stock.
    pub(super) fn ended(
        &mut self,
        journal: &mut Journal,
        clock: &dyn Clock,
        source: PauseSource,
    ) -> Result<(), Error> {
        self.persist(journal, clock, PauseState::Running {}, source)
    }

    pub(super) fn control(
        &mut self,
        journal: &mut Journal,
        observer: &mut dyn Observer,
        clock: &dyn Clock,
        live: &mut apply::Live,
        control: socket::Control,
    ) -> Result<socket::ControlStatus, Error> {
        match control {
            socket::Control::Status => {}
            socket::Control::Pause { state, uid } => {
                if matches!(state, PauseState::Running {})
                    || matches!(&state, PauseState::Until { deadline } if *deadline <= clock.now())
                {
                    return Err(Error::PauseDeadline);
                }
                // Persist the lifetime before touching hardware: a failed zero cannot lose the user's pause.
                self.persist(journal, clock, state, PauseSource::Command { uid })?;
                live.zero(journal, observer, clock)?;
            }
            socket::Control::Resume { uid } => {
                live.reapply(journal, observer, clock)?;
                self.ended(journal, clock, PauseSource::Command { uid })?;
            }
            socket::Control::Freeze { frozen, uid } => {
                append(
                    journal,
                    clock,
                    DayFreeze {
                        choice: if frozen {
                            FreezeChoice::Freeze
                        } else {
                            FreezeChoice::Unfreeze
                        },
                        source: FreezeSource::Command { uid },
                    },
                )?;
                self.frozen = frozen;
            }
            socket::Control::Reapply => live.reapply(journal, observer, clock)?,
        }
        Ok(self.status())
    }
}
