use crate::error::{Error, Operation, chain};
use crate::files::{Files, Readers};
use crate::identity;
use crate::mailbox::{COMMAND_DEADLINE, Command, Transport, Worker};
use crate::pm_table::{
    self, FileSource, Gated, Monotonic, PmReader, PmSource, Timing, Unavailable,
};
use crate::slots::{self, Location, SlotError, SlotMap};
use crate::sysfs::Sysfs;
use crate::topology::{self, RawCpu};
use std::fmt;
use std::io;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use togi_machine::{
    BiosContext, Check, CoreId, CoreInfo, ObservedOffset, Smu, SmuValidated, Stopped, TargetOffset,
};

pub(crate) struct Pending {
    transport: Box<dyn Transport>,
    pm: Arc<dyn PmSource>,
    deadline: Duration,
}

pub(crate) struct Validated {
    worker: Worker,
    slots: Result<SlotMap, Arc<SlotError>>,
    pm_source: Arc<dyn PmSource>,
    pm: Option<Arc<PmReader>>,
}

impl Validated {
    fn location(&self, core: CoreId, operation: Operation) -> Result<Location, Error> {
        let slots = self.slots.as_ref().map_err(|source| Error::SlotMapping {
            operation,
            source: Arc::clone(source),
        })?;
        slots.get(core).ok_or(Error::UnknownCore { core })
    }
}

/// How far the driver may go. The mailbox lives only in `Validated`, so no path reaches it
/// before the CPU and driver identity checked out.
pub(crate) enum Stage {
    Unvalidated(Option<Pending>),
    IdentityOnly { loaded: bool },
    Validated(Validated),
    Lost,
}

fn validated(stage: &mut Stage, operation: Operation) -> Result<&mut Validated, Error> {
    match stage {
        Stage::Validated(validated) => Ok(validated),
        Stage::Unvalidated(_) => Err(Error::NotValidated { operation }),
        Stage::IdentityOnly { .. } => Err(Error::IdentityOnly { operation }),
        Stage::Lost => Err(Error::MailboxLost),
    }
}

fn low_word(offset: i64) -> u32 {
    let bytes = offset.to_le_bytes();
    u32::from(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn signed_low_word(word: u32) -> i64 {
    let bytes = word.to_le_bytes();
    i64::from(i16::from_le_bytes([bytes[0], bytes[1]]))
}

fn check(name: &str, outcome: Result<String, Error>) -> Check {
    match outcome {
        Ok(detail) => Check {
            name: name.to_owned(),
            detail,
            ok: true,
        },
        Err(error) => Check {
            name: name.to_owned(),
            detail: chain(&error),
            ok: false,
        },
    }
}

/// The only writer of Curve Optimizer offsets.
pub struct Driver {
    root: PathBuf,
    files: Files,
    cpus: Vec<RawCpu>,
    stage: Stage,
}

impl fmt::Debug for Driver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Driver")
            .field("root", &self.root)
            .field("cpus", &self.cpus.len())
            .finish_non_exhaustive()
    }
}

impl Driver {
    /// Opens the machine's own sysfs and procfs. Needs Linux.
    pub fn system() -> Result<Self, Error> {
        Self::at_system_root(Self::open)
    }

    /// Opens for the CPU and driver identity checks only; never opens the mailbox.
    pub fn system_identity_only() -> Result<Self, Error> {
        Self::at_system_root(Self::identity_only)
    }

    #[cfg(target_os = "linux")]
    fn at_system_root(open: fn(&Path) -> Result<Self, Error>) -> Result<Self, Error> {
        open(Path::new("/"))
    }

    #[cfg(not(target_os = "linux"))]
    fn at_system_root(_open: fn(&Path) -> Result<Self, Error>) -> Result<Self, Error> {
        Err(Error::Unsupported)
    }

    /// Reads the CPU topology under `root`. Touches no mailbox, SMN register or PM table file.
    pub fn open(root: &Path) -> Result<Self, Error> {
        let pending = Sysfs::locate(root).map(|sysfs| Pending {
            transport: Box::new(sysfs),
            pm: Arc::new(FileSource::new(root, Files::System)),
            deadline: COMMAND_DEADLINE,
        });
        Self::with_stage(root, Files::System, Stage::Unvalidated(pending))
    }

    pub fn identity_only(root: &Path) -> Result<Self, Error> {
        let loaded = Sysfs::locate(root).is_some();
        Self::with_stage(root, Files::System, Stage::IdentityOnly { loaded })
    }

    /// Opens supplied CPU directory names and file readers, without accessing the host.
    /// Paths passed to the closures are rooted at `root`. Mailbox writes still require
    /// successful CPU and driver validation; no system-file fallback is used.
    pub fn from_readers(
        root: &Path,
        cpu_names: Vec<String>,
        loaded: bool,
        read: impl Fn(&Path) -> io::Result<Vec<u8>> + Send + Sync + 'static,
        write: impl Fn(&Path, &[u8]) -> io::Result<usize> + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        let files = Files::Readers(Arc::new(Readers {
            cpu_names,
            read: Arc::new(read),
            write: Arc::new(write),
        }));
        let pending = loaded.then(|| Pending {
            transport: Box::new(Sysfs::from_readers(root, files.clone())),
            pm: Arc::new(FileSource::new(root, files.clone())),
            deadline: COMMAND_DEADLINE,
        });
        Self::with_stage(root, files, Stage::Unvalidated(pending))
    }

    /// Like `identity_only`, using only the supplied readers, never host files.
    pub fn identity_only_from_readers(
        root: &Path,
        cpu_names: Vec<String>,
        loaded: bool,
        read: impl Fn(&Path) -> io::Result<Vec<u8>> + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        let files = Files::Readers(Arc::new(Readers {
            cpu_names,
            read: Arc::new(read),
            write: Arc::new(|_, _| Err(io::ErrorKind::PermissionDenied.into())),
        }));
        Self::with_stage(root, files, Stage::IdentityOnly { loaded })
    }

    #[cfg(test)]
    pub(crate) fn with_transport(
        root: &Path,
        files: Files,
        transport: Box<dyn Transport>,
        pm: Arc<dyn PmSource>,
        deadline: Duration,
    ) -> Result<Self, Error> {
        let pending = Pending {
            transport,
            pm,
            deadline,
        };
        Self::with_stage(root, files, Stage::Unvalidated(Some(pending)))
    }

    /// Replaces the mailbox deadline of a validated driver, so a test can validate generously
    /// and then time out quickly.
    #[cfg(test)]
    pub(crate) fn set_deadline(&mut self, deadline: Duration) {
        if let Stage::Validated(validated) = &mut self.stage {
            validated.worker.set_deadline(deadline);
        }
    }

    fn with_stage(root: &Path, files: Files, stage: Stage) -> Result<Self, Error> {
        Ok(Self {
            root: root.to_owned(),
            cpus: topology::read(root, &files)?,
            files,
            stage,
        })
    }

    fn loaded(&self) -> bool {
        match &self.stage {
            Stage::Unvalidated(pending) => pending.is_some(),
            Stage::IdentityOnly { loaded } => *loaded,
            Stage::Validated(_) => true,
            Stage::Lost => false,
        }
    }

    pub fn topology(&self) -> Result<Vec<CoreInfo>, Error> {
        topology::group(&self.cpus)?
            .into_iter()
            .map(|core| {
                Ok(CoreInfo {
                    core: CoreId::new(core.core_id).map_err(|_| Error::CoreOutOfRange {
                        core_id: core.core_id,
                        ccd: core.ccd.0,
                    })?,
                    ccd: core.ccd,
                    cpus: core.cpus,
                })
            })
            .collect()
    }

    /// Checks the CPU and driver identity, then opens the mailbox and maps the slots. A slot
    /// mapping that is refused leaves the driver validated but refusing per-core access.
    pub fn validate(&mut self) -> Result<SmuValidated, Error> {
        match &self.stage {
            Stage::Validated(_) => return Ok(Self::proof()),
            Stage::IdentityOnly { .. } => {
                return Err(Error::IdentityOnly {
                    operation: Operation::Validate,
                });
            }
            Stage::Lost => return Err(Error::MailboxLost),
            Stage::Unvalidated(_) => {}
        }
        identity::cpu(&self.root, &self.files)?;
        identity::driver(&self.root, &self.files, self.loaded())?;
        let Stage::Unvalidated(Some(pending)) = mem::replace(&mut self.stage, Stage::Lost) else {
            return Err(Error::DriverNotLoaded);
        };
        let mut worker =
            Worker::spawn(pending.transport, pending.deadline).map_err(Error::WorkerSpawn)?;
        let slots = slots::map(&self.cpus, &mut |address| worker.read_smn(address));
        self.stage = Stage::Validated(Validated {
            worker,
            slots: slots.map_err(Arc::new),
            pm_source: pending.pm,
            pm: None,
        });
        Ok(Self::proof())
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "validate is the one place that mints the proof, after the identity checks passed"
    )]
    fn proof() -> SmuValidated {
        SmuValidated::checked()
    }

    pub fn check_cpu(&self) -> Check {
        check(
            "cpu",
            identity::cpu(&self.root, &self.files).map(|cpu| cpu.to_string()),
        )
    }

    pub fn check_driver(&self) -> Check {
        check(
            "ryzen_smu",
            identity::driver(&self.root, &self.files, self.loaded())
                .map(|driver| driver.to_string()),
        )
    }

    pub fn check_readback(&mut self) -> Check {
        check("readback", self.read_back_all())
    }
    pub fn check_slot_mapping(&mut self) -> Check {
        let outcome = validated(&mut self.stage, Operation::MapSlots).and_then(|validated| {
            validated
                .slots
                .as_ref()
                .map(|slots| slots.detail().to_owned())
                .map_err(|refusal| Error::Mapping(Arc::clone(refusal)))
        });
        check("slot_mapping", outcome)
    }

    fn read_back_all(&mut self) -> Result<String, Error> {
        let cores = self.topology()?;
        let mut values = Vec::with_capacity(cores.len());
        for info in &cores {
            let offset = self
                .read_offset(info.core)
                .map_err(|error| Error::ReadBack {
                    core: info.core,
                    source: Box::new(error),
                })?;
            values.push(format!("{:02}:{}", info.core, offset.0));
        }
        Ok(format!(
            "{} cores read back: {}",
            cores.len(),
            values.join(" ")
        ))
    }

    /// BIOS, board, CPU model and microcode strings (lossy UTF-8) and the SMU's boost limit.
    pub fn bios_context(&mut self) -> Result<BiosContext, Error> {
        let operation = Operation::ReadBiosContext;
        let validated = validated(&mut self.stage, operation)?;
        let firmware = identity::firmware(&self.root, &self.files)?;
        let response = validated
            .worker
            .command(Command::BoostLimit, [0; 6])
            .map_err(|source| Error::Mailbox { operation, source })?;
        Ok(BiosContext {
            bios_version: firmware.bios_version,
            board: firmware.board,
            cpu_model: firmware.cpu_model,
            microcode: firmware.microcode,
            boost_limit_mhz: response[0],
        })
    }

    /// The driver's one PM table reader, started on first use; every call returns the same
    /// reader, so at most one read is ever in flight. Reading the table sends RSMU command 0x03,
    /// so it exists only once the driver validated, and neither a new nor an existing reader
    /// starts a read once a mailbox transaction timed out.
    pub fn pm_reader(&mut self) -> Result<Arc<PmReader>, Error> {
        let layout = self
            .topology()
            .map_or(Err(Unavailable::Topology), |cores| pm_table::layout(&cores));
        let operation = Operation::ReadPmTable;
        let validated = validated(&mut self.stage, operation)?;
        let poison = validated.worker.poison();
        poison
            .check()
            .map_err(|source| Error::Mailbox { operation, source })?;
        if let Some(reader) = &validated.pm {
            return Ok(Arc::clone(reader));
        }
        let source = Gated::new(Arc::clone(&validated.pm_source), poison);
        let reader = Arc::new(PmReader::start(
            Box::new(source),
            layout,
            Box::new(Monotonic::new()),
            Timing::DEFAULT,
        )?);
        validated.pm = Some(Arc::clone(&reader));
        Ok(reader)
    }

    fn read_offset(&mut self, core: CoreId) -> Result<ObservedOffset, Error> {
        let operation = Operation::ReadOffset(core);
        let validated = validated(&mut self.stage, operation)?;
        let location = validated.location(core, operation)?;
        let response = validated
            .worker
            .command(Command::ReadOffset, [location.argument(), 0, 0, 0, 0, 0])
            .map_err(|source| Error::Mailbox { operation, source })?;
        Ok(ObservedOffset(signed_low_word(response[0])))
    }

    fn write_offset(&mut self, core: CoreId, offset: TargetOffset) -> Result<(), Error> {
        let operation = Operation::SetOffset(core);
        let validated = validated(&mut self.stage, operation)?;
        let location = validated.location(core, operation)?;
        let argument = location.argument() | low_word(offset.get());
        validated
            .worker
            .command(Command::SetOffset, [argument, 0, 0, 0, 0, 0])
            .map(drop)
            .map_err(|source| Error::Mailbox { operation, source })
    }

    fn write_all(&mut self, operation: Operation, offset: TargetOffset) -> Result<(), Error> {
        validated(&mut self.stage, operation)?
            .worker
            .command(
                Command::SetAllOffsets,
                [low_word(offset.get()), 0, 0, 0, 0, 0],
            )
            .map(drop)
            .map_err(|source| Error::Mailbox { operation, source })
    }
}

impl Smu for Driver {
    fn offset(&mut self, core: CoreId) -> Result<ObservedOffset, togi_machine::Error> {
        Ok(self.read_offset(core)?)
    }

    fn set_offset(
        &mut self,
        core: CoreId,
        offset: TargetOffset,
    ) -> Result<(), togi_machine::Error> {
        Ok(self.write_offset(core, offset)?)
    }

    fn set_all_offsets(&mut self, offset: TargetOffset) -> Result<(), togi_machine::Error> {
        Ok(self.write_all(Operation::SetAllOffsets, offset)?)
    }

    fn emergency_zero(
        &mut self,
        _validated: SmuValidated,
        _stopped: &Stopped,
    ) -> Result<(), togi_machine::Error> {
        Ok(self.write_all(Operation::EmergencyZero, TargetOffset::ZERO)?)
    }
}
