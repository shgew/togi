use std::io::{self, Write};
use togi_machine::{CoreId, ObservedOffset, PerCore, Smu, TargetOffset, error_chain};

#[derive(Debug, thiserror::Error)]
pub(super) enum RoundTripError {
    #[error("access offset witness")]
    Access(#[from] togi_machine::Error),
    #[error("validate offset witness")]
    Readback(#[from] WitnessError),
}

// This file is compiled only by in-memory unit tests and the hardware-feature test.
// The production library shares only the pure fixture and readback validation.
use super::{OffsetWitness, WitnessError};

#[must_use = "dropping this guard restores and verifies stock"]
pub(super) struct Restoring<S: Smu> {
    driver: Option<S>,
}

impl<S: Smu> Restoring<S> {
    pub(super) fn new(driver: S) -> Self {
        Self {
            driver: Some(driver),
        }
    }

    pub(super) fn driver(&mut self) -> &mut S {
        self.driver
            .as_mut()
            .unwrap_or_else(|| panic!("guard remains armed until finish"))
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "test-only witness: host-locked hardware test or in-memory fake, with stock cleanup"
    )]
    pub(super) fn round_trip(
        &mut self,
        witness: &OffsetWitness,
    ) -> Result<PerCore<ObservedOffset>, RoundTripError> {
        for (core, target) in witness.targets().iter() {
            self.driver().set_offset(core, target)?;
        }
        let mut observed = PerCore::default();
        for (core, _) in witness.targets().iter() {
            observed.set(core, self.driver().offset(core)?);
        }
        witness.verify(&observed)?;
        Ok(observed)
    }

    pub(super) fn finish(mut self) -> Vec<String> {
        let failures = restore(self.driver());
        if failures.is_empty() {
            drop(self.driver.take());
        }
        failures
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "test-only cleanup: attempt stock on every core even after another core fails"
)]
fn restore(driver: &mut impl Smu) -> Vec<String> {
    let mut failures = Vec::new();
    for id in 0..16 {
        let core = CoreId::new(id).unwrap_or_else(|error| panic!("stock cleanup core: {error}"));
        if let Err(error) = driver.set_offset(core, TargetOffset::ZERO) {
            failures.push(format!("restore core {core:02}: {}", error_chain(&error)));
        }
    }
    for id in 0..16 {
        let core = CoreId::new(id).unwrap_or_else(|error| panic!("stock cleanup core: {error}"));
        match driver.offset(core) {
            Ok(ObservedOffset(0)) => {}
            Ok(read) => failures.push(format!("core {core:02}: stock read back {}", read.0)),
            Err(error) => {
                failures.push(format!("read back core {core:02}: {}", error_chain(&error)));
            }
        }
    }
    failures
}

impl<S: Smu> Drop for Restoring<S> {
    fn drop(&mut self) {
        let Some(driver) = self.driver.as_mut() else {
            return;
        };
        let failures = restore(driver);
        if failures.is_empty() {
            return;
        }
        let report = failures.join("; ");
        if std::thread::panicking() {
            let _ = writeln!(io::stderr(), "stock not verified: {report}");
        } else {
            panic!("stock not verified: {report}");
        }
    }
}
