use std::collections::HashMap;
use std::time::Duration;

use jiff::Timestamp;
use togi_machine::{BankType, CoreId, CoreInfo, CpuId, Mce};

const PREFIX: &str = "[Hardware Error]: ";

const DESCRIPTIONS: [&str; 5] = [
    "[Hardware Error]: Corrected error, no action required.",
    "[Hardware Error]: Deferred error, no action required.",
    "[Hardware Error]: Uncorrected, software restartable error.",
    "[Hardware Error]: Uncorrected, software containable error.",
    "[Hardware Error]: System Fatal error.",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message {
    pub time: Timestamp,
    pub text: String,
    pub monotonic: Duration,
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
#[error("cpu {} is listed under core {first} and under core {second}", cpu.0)]
pub struct DuplicateCpu {
    pub cpu: CpuId,
    pub first: CoreId,
    pub second: CoreId,
}

/// Which physical core a logical CPU belongs to, from the topology and nothing else.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CpuCoreMap(HashMap<CpuId, CoreId>);

impl CpuCoreMap {
    pub fn new(topology: &[CoreInfo]) -> Result<Self, DuplicateCpu> {
        let mut cores = HashMap::new();
        for info in topology {
            for &cpu in &info.cpus {
                if let Some(first) = cores.insert(cpu, info.core) {
                    return Err(DuplicateCpu {
                        cpu,
                        first,
                        second: info.core,
                    });
                }
            }
        }
        Ok(Self(cores))
    }

    pub fn core(&self, cpu: CpuId) -> Option<CoreId> {
        self.0.get(&cpu).copied()
    }
}

/// A status line whose CPU or bank number does not fit the machine vocabulary.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum StatusError {
    #[error("machine-check status names cpu {0}, which is not a logical CPU number")]
    Cpu(String),
    #[error("machine-check status names bank {0}, which is not a bank number")]
    Bank(String),
}

/// The machine checks found before a status line could not be read, and why it could not.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct ParseFailure {
    pub mces: Vec<Mce>,
    pub error: StatusError,
}

pub fn parse(messages: &[Message], cores: &CpuCoreMap) -> Result<Vec<Mce>, ParseFailure> {
    let mut parser = Parser::new(cores);
    for message in messages {
        if let Err(error) = parser.push(message) {
            return Err(ParseFailure {
                mces: parser.finish(),
                error,
            });
        }
    }
    Ok(parser.finish())
}

struct Record {
    mce: Mce,
    header_lines: usize,
}

/// The record the continuation lines being read belong to.
#[derive(Default)]
struct CurrentRecord {
    index: Option<usize>,
    raw: bool,
    pending: usize,
    ambiguous: bool,
}

/// Decoded statuses whose bank lines have not all arrived. It outlives description lines, so a
/// surplus bank line can still withdraw the attribution of records back to the first unresolved one.
#[derive(Default)]
struct UnresolvedOwnership {
    first: usize,
    pending: usize,
    ambiguous: bool,
    cleared: bool,
}

impl UnresolvedOwnership {
    fn start(&mut self, index: usize) {
        if self.pending == 0 {
            *self = Self {
                first: index,
                ..Self::default()
            };
        }
        self.pending += 1;
    }

    fn bank(
        &mut self,
        records: &mut [Record],
        current: usize,
        pending_in_record: usize,
        raw: bool,
    ) -> bool {
        if raw {
            return true;
        }
        self.ambiguous |= pending_in_record == 0;
        if self.ambiguous && !self.cleared {
            if let Some(unresolved) = records.get_mut(self.first..=current) {
                for record in unresolved {
                    record.mce.bank_type = BankType::Unknown;
                    record.mce.lines.truncate(record.header_lines);
                }
            }
            self.cleared = true;
        }
        self.pending = self.pending.saturating_sub(1);
        !self.ambiguous
    }
}

pub(crate) struct Parser<'a> {
    cores: &'a CpuCoreMap,
    records: Vec<Record>,
    preceding: Option<String>,
    current: CurrentRecord,
    ownership: UnresolvedOwnership,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(cores: &'a CpuCoreMap) -> Self {
        Self {
            cores,
            records: Vec::new(),
            preceding: None,
            current: CurrentRecord::default(),
            ownership: UnresolvedOwnership::default(),
        }
    }

    pub(crate) fn push(&mut self, message: &Message) -> Result<(), StatusError> {
        let text = message.text.as_str();
        if DESCRIPTIONS.contains(&text) {
            self.preceding = Some(stamped(message));
            self.current = CurrentRecord::default();
            return Ok(());
        }
        if let Some(status) = Status::parse(text)? {
            self.start_record(message, &status);
            return Ok(());
        }
        self.preceding = None;
        if !text.starts_with(PREFIX) {
            self.current.index = None;
            return Ok(());
        }
        self.continue_record(message);
        Ok(())
    }

    pub(crate) fn finish(self) -> Vec<Mce> {
        self.records.into_iter().map(|record| record.mce).collect()
    }

    fn start_record(&mut self, message: &Message, status: &Status) {
        if !status.raw {
            self.current.ambiguous = self.current.pending != 0;
            self.current.pending += 1;
            self.ownership.start(self.records.len());
        }
        let mut lines = Vec::with_capacity(2);
        lines.extend(self.preceding.take());
        lines.push(stamped(message));
        self.records.push(Record {
            header_lines: lines.len(),
            mce: Mce {
                cpu: status.cpu,
                core: self.cores.core(status.cpu),
                bank: status.bank,
                bank_type: BankType::Unknown,
                corrected: status.corrected,
                time: message.time,
                monotonic: message.monotonic,
                lines,
            },
        });
        self.current.index = Some(self.records.len() - 1);
        self.current.raw = status.raw;
    }

    fn continue_record(&mut self, message: &Message) {
        let Some(index) = self.current.index else {
            return;
        };
        let text = message.text.as_str();
        let bank = bank_name(text);
        let reserved =
            text.starts_with("[Hardware Error]: Bank ") && text.ends_with(" is reserved.");
        if bank.is_none() && !reserved {
            if !self.current.ambiguous {
                self.append(index, message);
            }
            return;
        }
        if !self.ownership.bank(
            &mut self.records,
            index,
            self.current.pending,
            self.current.raw,
        ) {
            self.current.ambiguous = true;
        }
        if !self.current.ambiguous && (self.current.raw || self.current.pending == 1) {
            if !self.current.raw {
                if let Some(record) = self.records.get_mut(index) {
                    if record.mce.core.is_some() {
                        record.mce.bank_type = bank.map_or(BankType::Other, bank_type);
                    }
                }
            }
            self.append(index, message);
        }
        self.current.pending = self.current.pending.saturating_sub(1);
    }

    fn append(&mut self, index: usize, message: &Message) {
        if let Some(record) = self.records.get_mut(index) {
            record.mce.lines.push(stamped(message));
        }
    }
}

fn stamped(message: &Message) -> String {
    let micros = message.time.as_microsecond();
    format!(
        "{}.{:06} {}",
        micros.div_euclid(1_000_000),
        micros.rem_euclid(1_000_000),
        message.text
    )
}

struct Status {
    cpu: CpuId,
    bank: u16,
    corrected: bool,
    raw: bool,
}

impl Status {
    fn parse(text: &str) -> Result<Option<Self>, StatusError> {
        let Some(found) = Self::decoded(text).or_else(|| Self::raw(text)) else {
            return Ok(None);
        };
        let cpu = found
            .cpu
            .parse()
            .map(CpuId)
            .map_err(|_| StatusError::Cpu(found.cpu.to_owned()))?;
        let bank = found
            .bank
            .parse()
            .map_err(|_| StatusError::Bank(found.bank.to_owned()))?;
        Ok(Some(Self {
            cpu,
            bank,
            corrected: found.corrected,
            raw: found.raw,
        }))
    }

    fn decoded(text: &str) -> Option<Found<'_>> {
        let rest = text.strip_prefix("[Hardware Error]: CPU:")?;
        let (cpu, rest) = digits(rest)?;
        let rest = rest.strip_prefix(" (")?;
        let (_, rest) = hex(rest)?;
        let (_, rest) = hex(rest.strip_prefix(':')?)?;
        let (_, rest) = hex(rest.strip_prefix(':')?)?;
        let (bank, rest) = digits(rest.strip_prefix(") MC")?)?;
        let (flags, rest) = rest.strip_prefix("_STATUS[")?.split_once(']')?;
        sixteen_hex(rest.strip_prefix(": 0x")?)?;
        Some(Found {
            cpu,
            bank,
            corrected: flags.split('|').nth(1) == Some("CE"),
            raw: false,
        })
    }

    fn raw(text: &str) -> Option<Found<'_>> {
        const OVERFLOW_OR_UNCORRECTED: u64 = 1 << 61 | 1 << 44;
        let rest = text.strip_prefix("[Hardware Error]: CPU ")?;
        let (cpu, rest) = digits(rest)?;
        let rest = rest.strip_prefix(": Machine Check")?;
        let rest = rest.strip_prefix(" Exception").unwrap_or(rest);
        let (_, rest) = hex(rest.strip_prefix(": ")?)?;
        let (bank, rest) = digits(rest.strip_prefix(" Bank ")?)?;
        let status = sixteen_hex(rest.strip_prefix(": ")?)?;
        Some(Found {
            cpu,
            bank,
            corrected: status & OVERFLOW_OR_UNCORRECTED == 0,
            raw: true,
        })
    }
}

struct Found<'a> {
    cpu: &'a str,
    bank: &'a str,
    corrected: bool,
    raw: bool,
}

fn split_while(text: &str, keep: impl Fn(char) -> bool) -> Option<(&str, &str)> {
    let end = text.find(|c| !keep(c)).unwrap_or(text.len());
    (end > 0).then(|| text.split_at(end))
}

pub(crate) fn digits(text: &str) -> Option<(&str, &str)> {
    split_while(text, |c| c.is_ascii_digit())
}

fn is_lower_hex(c: char) -> bool {
    matches!(c, '0'..='9' | 'a'..='f')
}

fn hex(text: &str) -> Option<(&str, &str)> {
    split_while(text, is_lower_hex)
}

fn sixteen_hex(text: &str) -> Option<u64> {
    (text.len() == 16 && text.chars().all(is_lower_hex))
        .then(|| u64::from_str_radix(text, 16).ok())?
}

pub(crate) fn bank_name(text: &str) -> Option<&str> {
    const MARKER: &str = " Ext. Error Code: ";
    let line = text.strip_prefix(PREFIX)?.split('\n').next()?;
    line.rmatch_indices(MARKER)
        .find(|(at, _)| {
            *at > 0 && line[at + MARKER.len()..].starts_with(|c: char| c.is_ascii_digit())
        })
        .map(|(at, _)| &line[..at])
}

fn bank_type(name: &str) -> BankType {
    match name {
        "Load Store Unit" => BankType::LoadStore,
        "Instruction Fetch Unit" => BankType::InstructionFetch,
        "L2 Cache" => BankType::L2Cache,
        "Decode Unit" => BankType::DecodeUnit,
        "Execution Unit" => BankType::ExecutionUnit,
        "Floating Point Unit" => BankType::FloatingPoint,
        "L3 Cache" => BankType::L3Cache,
        "Unified Memory Controller" | "Unified Memory Controller v2" => BankType::MemoryController,
        "Coherent Station" | "Power, Interrupts, etc." => BankType::DataFabric,
        _ => BankType::Other,
    }
}
