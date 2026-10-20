use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use togi_machine::{BootId, CpuFrequency, FrequencyPolicy, FrequencyPolicyId};

#[derive(Clone)]
pub(crate) struct Fake {
    state: Arc<Mutex<State>>,
}

struct State {
    boot: BootId,
    policies: Vec<FrequencyPolicy>,
    writes: Vec<(FrequencyPolicyId, u32)>,
    refuse_write: bool,
    wrong_readback: bool,
    fail_discovery: bool,
    before_write: Option<Box<dyn Fn() + Send + Sync>>,
}

impl Fake {
    pub(crate) fn new(policies: Vec<FrequencyPolicy>) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                boot: BootId::from("boot-a".to_owned()),
                policies,
                writes: Vec::new(),
                refuse_write: false,
                wrong_readback: false,
                fail_discovery: false,
                before_write: None,
            })),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn reboot(&self) {
        self.state().boot = BootId::from("boot-b".to_owned());
    }

    pub(crate) fn refuse_write(&self, value: bool) {
        self.state().refuse_write = value;
    }

    pub(crate) fn wrong_readback(&self, value: bool) {
        self.state().wrong_readback = value;
    }

    pub(crate) fn fail_discovery(&self) {
        self.state().fail_discovery = true;
    }

    pub(crate) fn before_write(&self, hook: impl Fn() + Send + Sync + 'static) {
        self.state().before_write = Some(Box::new(hook));
    }

    pub(crate) fn writes(&self) -> Vec<(FrequencyPolicyId, u32)> {
        self.state().writes.clone()
    }

    /// Replaces the discovered policies, as a hotplug or driver change between runs would.
    pub(crate) fn set_policies(&self, policies: Vec<FrequencyPolicy>) {
        self.state().policies = policies;
    }
}

impl CpuFrequency for Fake {
    fn boot_id(&self) -> io::Result<BootId> {
        Ok(self.state().boot.clone())
    }

    fn policies(&self) -> io::Result<Vec<FrequencyPolicy>> {
        let state = self.state();
        if state.fail_discovery {
            return Err(io::Error::other("policy discovery failed"));
        }
        Ok(state.policies.clone())
    }

    fn scaling_max_khz(&self, id: FrequencyPolicyId) -> io::Result<u32> {
        let state = self.state();
        let policy = state
            .policies
            .iter()
            .find(|policy| policy.id == id)
            .ok_or_else(|| io::Error::other("policy missing"))?;
        Ok(if state.wrong_readback {
            policy.scaling_max_khz + 1
        } else {
            policy.scaling_max_khz
        })
    }

    fn set_scaling_max_khz(&self, id: FrequencyPolicyId, khz: u32) -> io::Result<()> {
        let mut state = self.state();
        if let Some(hook) = &state.before_write {
            hook();
        }
        state.writes.push((id, khz));
        if state.refuse_write {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "policy write refused",
            ));
        }
        state
            .policies
            .iter_mut()
            .find(|policy| policy.id == id)
            .ok_or_else(|| io::Error::other("policy missing"))?
            .scaling_max_khz = khz;
        Ok(())
    }
}
