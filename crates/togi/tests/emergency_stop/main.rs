#![allow(
    clippy::unwrap_used,
    reason = "the helpers panic on a broken fixture, as the tests do"
)]

//! `togi emergency-stop` against a simulated machine: nothing writes the SMU before the validation and the sweep
//! succeed, every core is written 0 and read back, and any failure is a nonzero status.

#[path = "../support/mod.rs"]
mod support;

use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::rc::Rc;

use support::{Host, Ran};
use togi_cli::Exit;
use togi_hostlock::{HostLock, Policy};
use togi_machine::{
    BiosContext, BootId, CancellationToken, Check, CoreId, CoreInfo, CoreRank, Error,
    Host as MachineHost, Machine, Partial, Running, SmuValidated, Stopped, Swept, TrialConditions,
    TrialSpec, Trials,
};
use togi_sim::faults::{HookPhase, SmuOp, SmuOperation};
use togi_sim::{Model, RunSettings, Simulator, file};

const CORES: u8 = 16;
const BIOS: i64 = -10;

type Log = Rc<RefCell<Vec<String>>>;

/// The machine `togi emergency-stop` finds after a crash: every core's offset at `BIOS`.
fn simulator() -> Simulator {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/bench/machines/shared-voltage.json");
    let mut definition = file::load(path).unwrap();
    definition.bios = Some(vec![BIOS; usize::from(CORES)]);
    Simulator::new(
        Model::new(&definition, 1).unwrap(),
        RunSettings {
            seed: 1,
            ..RunSettings::default()
        },
    )
}

fn core(number: u8) -> CoreId {
    CoreId::try_from(number).unwrap()
}

/// Trials that note the sweep and can refuse it.
struct Sweeping {
    inner: Box<dyn Trials>,
    log: Log,
    fails: bool,
}

impl Trials for Sweeping {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, Error> {
        self.log.borrow_mut().push("sweep".into());
        if self.fails {
            return Err(Error::Containment(
                "scope togi-trial-0001.scope would not stop".into(),
            ));
        }
        self.inner.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        self.inner.start(cancel, spec, ready)
    }

    fn ended(&mut self, id: &str) -> Result<(), Error> {
        self.inner.ended(id)
    }

    fn samples<'a>(&'a self, id: &'a str) -> Box<dyn Iterator<Item = TrialConditions> + 'a> {
        self.inner.samples(id)
    }
}

/// A host that reports no core and notes the validation.
struct NoCores {
    inner: Box<dyn MachineHost>,
    log: Log,
}

impl MachineHost for NoCores {
    fn boot_id(&mut self) -> Result<BootId, Error> {
        self.inner.boot_id()
    }

    fn topology(&mut self) -> Result<Vec<CoreInfo>, Error> {
        Ok(Vec::new())
    }

    fn validate_smu(&mut self) -> Result<SmuValidated, Error> {
        self.log.borrow_mut().push("validate".into());
        self.inner.validate_smu()
    }

    fn bios_context(&mut self) -> Result<BiosContext, Error> {
        self.inner.bios_context()
    }

    fn ranking(&mut self) -> Result<Vec<CoreRank>, Partial<Vec<CoreRank>>> {
        self.inner.ranking()
    }

    fn preflight(&mut self) -> Vec<Check> {
        self.inner.preflight()
    }

    fn watchdog(&mut self) -> Check {
        self.inner.watchdog()
    }
}

/// A fake host with a staged simulated machine, and what reached the machine's SMU and sweep, in order.
struct Rig {
    host: Host,
    sim: Simulator,
    log: Log,
    state: PathBuf,
}

impl Rig {
    fn new() -> Self {
        Self::build(false, false)
    }

    fn build(sweep_fails: bool, no_cores: bool) -> Self {
        let host = Host::new();
        let sim = simulator();
        let log = Log::default();
        let watched = Rc::clone(&log);
        sim.interrupt_smu(move |operation: &SmuOperation| {
            if operation.phase == HookPhase::Before {
                watched
                    .borrow_mut()
                    .push(match (operation.op, operation.core) {
                        (SmuOp::Read, Some(core)) => format!("read {core:02}"),
                        (SmuOp::Set, Some(core)) => format!("set {core:02}"),
                        (SmuOp::Read | SmuOp::Set, None) => {
                            unreachable!("a per-core access names its core")
                        }
                        (SmuOp::SetAll, _) => "set-all".to_owned(),
                    });
            }
            Ok(())
        });
        let seams = sim.seams();
        let machine = Machine {
            trials: Box::new(Sweeping {
                inner: seams.trials,
                log: Rc::clone(&log),
                fails: sweep_fails,
            }),
            host: if no_cores {
                Box::new(NoCores {
                    inner: seams.host,
                    log: Rc::clone(&log),
                })
            } else {
                seams.host
            },
            ..sim.seams()
        };
        *host.machine.borrow_mut() = Some(Ok(machine));
        let state = host.lock.with_file_name("state");
        Self {
            host,
            sim,
            log,
            state,
        }
    }

    fn run(&self) -> Ran {
        self.host.run(&self.state, &["emergency-stop"])
    }

    /// What reached the SMU and the sweep so far.
    fn ops(&self) -> Vec<String> {
        self.log.borrow().clone()
    }

    /// Every core's register, read behind the log's back.
    fn offsets(&self) -> Vec<i64> {
        self.sim.interrupt_smu(|_: &SmuOperation| Ok(()));
        let mut smu = self.sim.seams().smu;
        (0..CORES)
            .map(|number| smu.offset(core(number)).unwrap().0)
            .collect()
    }

    fn unopened(&self) -> bool {
        self.host.machine.borrow().is_some() && self.host.steps().is_empty()
    }

    fn assert_untouched(&self) {
        assert_eq!(self.offsets(), vec![BIOS; usize::from(CORES)]);
        assert!(
            !self.state.exists(),
            "emergency-stop created the state directory"
        );
    }
}

fn readbacks(text: impl Fn(u8) -> String) -> String {
    let mut lines = String::new();
    for number in 0..CORES {
        writeln!(lines, "togi emergency-stop: {}", text(number)).unwrap();
    }
    lines
}

#[test]
fn a_stop_sweeps_then_zeroes_every_core_and_reads_each_back() {
    let rig = Rig::new();
    let ran = rig.run();
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(ran.out, "");
    assert_eq!(
        ran.err,
        format!(
            "togi emergency-stop: trial scopes: simulated machine has no leftover trial scopes\n{}togi emergency-stop: every core read back 0 (16 cores)\n",
            readbacks(|number| format!("core {number:02} read back 0")),
        )
    );
    let mut expected = vec!["sweep".to_owned(), "set-all".to_owned()];
    expected.extend((0..CORES).map(|number| format!("read {number:02}")));
    assert_eq!(rig.ops(), expected);
    assert_eq!(rig.offsets(), vec![0; usize::from(CORES)]);
    assert!(
        !rig.state.exists(),
        "emergency-stop created the state directory"
    );
}

#[test]
fn a_stop_releases_the_host_lock() {
    let rig = Rig::new();
    assert_eq!(rig.run().exit, Exit::Ok);
    assert!(HostLock::acquire(&rig.host.lock, Policy::Private).is_ok());
}

#[test]
fn a_failed_sweep_exits_1_and_leaves_every_offset_as_it_was() {
    let rig = Rig::build(true, false);
    let ran = rig.run();
    assert_eq!(ran.exit, Exit::Error);
    assert_eq!(ran.out, "");
    assert_eq!(
        ran.err,
        "togi emergency-stop: stop the stale trial scopes: trial cleanup could not be confirmed: scope togi-trial-0001.scope would not stop; offsets unchanged\n"
    );
    assert_eq!(rig.ops(), ["sweep"]);
    rig.assert_untouched();
}

#[test]
fn a_failed_validation_exits_1_before_the_sweep_and_leaves_every_offset_as_it_was() {
    for (check, detail) in [("cpu", "not a Zen 5"), ("ryzen_smu", "driver not loaded")] {
        let rig = Rig::new();
        rig.sim.fail_check(check, detail);
        let ran = rig.run();
        assert_eq!(ran.exit, Exit::Error, "{check}");
        assert_eq!(
            ran.err,
            format!(
                "togi emergency-stop: validate the SMU interface: validate {check}: {detail}; offsets unchanged\n"
            ),
            "{check}"
        );
        assert_eq!(rig.ops(), Vec::<String>::new(), "{check}");
        rig.assert_untouched();
    }
}

#[test]
fn a_topology_without_cores_is_refused_before_validation_and_sweep() {
    let rig = Rig::build(false, true);
    let ran = rig.run();
    assert_eq!(ran.exit, Exit::Error);
    assert_eq!(
        ran.err,
        "togi emergency-stop: read the core topology: the topology lists no core to zero; offsets unchanged\n"
    );
    assert_eq!(rig.ops(), Vec::<String>::new());
    rig.assert_untouched();
}

#[test]
fn a_busy_host_lock_exits_3_before_the_machine_is_opened() {
    let rig = Rig::new();
    let _held = HostLock::acquire(&rig.host.lock, Policy::Private).unwrap();
    let ran = rig.run();
    assert_eq!(ran.exit, Exit::Locked);
    assert_eq!(ran.out, "");
    assert_eq!(
        ran.err,
        format!(
            "togi emergency-stop: another togi process holds the host lock {}; offsets unchanged\n",
            rig.host.lock.display()
        )
    );
    assert!(rig.unopened());
    assert_eq!(rig.ops(), Vec::<String>::new());
    rig.assert_untouched();
}

#[test]
fn a_platform_other_than_linux_is_refused_before_the_lock_and_the_machine() {
    let mut rig = Rig::new();
    rig.host.linux = false;
    let ran = rig.run();
    assert_eq!(ran.exit, Exit::Error);
    assert_eq!(
        ran.err,
        "togi emergency-stop: hardware runs need Linux: unsupported operation\n"
    );
    assert!(!rig.host.lock.exists(), "the lock was taken");
    assert!(rig.unopened());
    rig.assert_untouched();
}

#[test]
fn a_caller_without_root_is_refused_before_the_lock_and_the_machine() {
    let mut rig = Rig::new();
    rig.host.root = false;
    let ran = rig.run();
    assert_eq!(ran.exit, Exit::Error);
    assert_eq!(
        ran.err,
        "togi emergency-stop: needs root to write the SMU; offsets unchanged\n"
    );
    assert!(!rig.host.lock.exists(), "the lock was taken");
    assert!(rig.unopened());
    rig.assert_untouched();
}

#[test]
fn a_machine_that_cannot_be_opened_exits_1_with_nothing_written() {
    let rig = Rig::new();
    *rig.host.machine.borrow_mut() = Some(Err(togi_hardware::Error::NeedsRoot {
        operation: "open the SMU driver",
        uid: 1000,
    }));
    let ran = rig.run();
    assert_eq!(ran.exit, Exit::Error);
    assert_eq!(
        ran.err,
        "togi emergency-stop: open the SMU driver: running as uid 1000; this needs root; offsets unchanged\n"
    );
    assert_eq!(rig.ops(), Vec::<String>::new());
    rig.assert_untouched();
}

#[test]
fn a_refused_zero_write_exits_1_and_reads_every_core_still_unchanged() {
    let rig = Rig::new();
    rig.sim.fail_write(1);
    let ran = rig.run();
    assert_eq!(ran.exit, Exit::Error);
    assert!(
        ran.err.contains(&readbacks(|number| format!(
            "core {number:02} reads {BIOS}"
        ))),
        "{}",
        ran.err
    );
    let last = ran.err.lines().last().unwrap();
    assert!(
        last.starts_with("togi emergency-stop: setting every core to 0 failed: ")
            && last.ends_with("; 16 of 16 cores did not read back 0"),
        "{last}"
    );
    assert!(
        rig.ops().iter().all(|op| !op.starts_with("set")),
        "{:?}",
        rig.ops()
    );
    assert_eq!(rig.offsets(), vec![BIOS; usize::from(CORES)]);
}

#[test]
fn a_readback_that_is_not_zero_fails_the_stop_and_names_the_core() {
    let rig = Rig::new();
    rig.sim.corrupt_readback(core(3));
    let ran = rig.run();
    assert_eq!(ran.exit, Exit::Error);
    assert_eq!(
        ran.err,
        format!(
            "togi emergency-stop: trial scopes: simulated machine has no leftover trial scopes\n{}togi emergency-stop: 1 of 16 cores did not read back 0\n",
            readbacks(|number| if number == 3 {
                "core 03 reads -1".to_owned()
            } else {
                format!("core {number:02} read back 0")
            }),
        )
    );
}

#[test]
fn an_unreadable_core_fails_the_stop_and_names_the_core() {
    let rig = Rig::new();
    rig.sim.interrupt_smu(|operation: &SmuOperation| {
        if operation.op == SmuOp::Read
            && operation.core == Some(core(5))
            && operation.phase == HookPhase::Before
        {
            return Err(Error::Other("mailbox timeout".into()));
        }
        Ok(())
    });
    let ran = rig.run();
    assert_eq!(ran.exit, Exit::Error);
    assert!(
        ran.err
            .contains("togi emergency-stop: core 05 unreadable: mailbox timeout\n"),
        "{}",
        ran.err
    );
    assert!(
        ran.err
            .ends_with("togi emergency-stop: 1 of 16 cores did not read back 0\n"),
        "{}",
        ran.err
    );
}

#[test]
fn a_machine_that_crashes_while_zeroing_fails_the_stop_with_every_core_unconfirmed() {
    let rig = Rig::new();
    rig.sim.crash_at_write(1);
    let ran = rig.run();
    assert_eq!(ran.exit, Exit::Error);
    assert!(
        ran.err.contains(
            "togi emergency-stop: setting every core to 0 failed: the simulated machine crashed; \
             16 of 16 cores did not read back 0\n"
        ),
        "{}",
        ran.err
    );
    assert!(
        ran.err
            .contains("togi emergency-stop: core 00 unreadable: the simulated machine crashed\n"),
        "{}",
        ran.err
    );
}
