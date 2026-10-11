use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use togi_backend::{Backend, Error, FileName, Input, Line, Plan, Ready, check_executable};
use togi_machine::{CpuId, Workload};

const NAME: &str = "mprime";
const PRIME: FileName = FileName::from_static("prime.txt");
const LOCAL: FileName = FileName::from_static("local.txt");
const RESULTS: FileName = FileName::from_static("results.txt");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Isa {
    Sse,
    Avx2,
    Avx512,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Variant {
    isa: Isa,
    min_fft: u16,
    max_fft: u16,
}

impl Variant {
    fn of(base: &str) -> Option<Self> {
        let (isa, min_fft, max_fft) = match base {
            "mprime-sse-4k-21k" => (Isa::Sse, 4, 21),
            "mprime-avx2-36k-248k" => (Isa::Avx2, 36, 248),
            "mprime-avx512-36k-248k" => (Isa::Avx512, 36, 248),
            _ => return None,
        };
        Some(Self {
            isa,
            min_fft,
            max_fft,
        })
    }

    fn settings(self, threads: usize) -> String {
        let hyperthreading = u8::from(threads > 1);
        let avx = u8::from(self.isa != Isa::Sse);
        let avx512 = u8::from(self.isa == Isa::Avx512);
        let Self {
            min_fft, max_fft, ..
        } = self;
        format!(
            "NumCPUs=1\nCoresPerTest=1\nMinTortureFFT={min_fft}\nMaxTortureFFT={max_fft}\n\
             TortureTime=1\nTortureHyperthreading={hyperthreading}\nTortureThreads={threads}\n\
             CpuSupportsAVX={avx}\nCpuSupportsFMA3={avx}\nCpuSupportsFMA4=0\n\
             CpuSupportsAVX2={avx}\nCpuSupportsAVX512F={avx512}\n"
        )
    }
}

fn prime_txt(settings: &str) -> String {
    format!("V30OptionsConverted=1\nStressTester=1\nUsePrimenet=0\n{settings}EnableSetAffinity=0\n")
}

fn local_txt(settings: &str) -> String {
    format!("ErrorCheck=1\nSumInputsErrorCheck=1\n{settings}")
}

static COMPUTATION: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        pattern(
            r"(?i)FATAL ERROR|ERROR: ILLEGAL SUMOUT|Possible hardware failure|Hardware failure detected|Maximum number of warnings exceeded|TORTURE TEST FAILED|Torture Test completed .* - [1-9][0-9]* errors",
        ),
        pattern(
            r"(?i)ERROR: SUM\(INPUTS\) != SUM\(OUTPUTS\)|ERROR: Shift counter corrupt|ERROR: Illegal double encountered|ERROR: FFT data has been zeroed|ERROR: Jacobi error check failed|Warning: ILLEGAL SUMOUT|Warning: SUMOUT MISMATCH",
        ),
    ]
});
static SETUP: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"(?i)Error allocating memory|Out of memory|Unable to allocate memory|Cannot initialize FFT code",
    )
});
static PROGRESS: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"(?i)Self-test ([0-9]+K?)(?: \(thread [0-9]+ of [0-9]+\))? passed"));

#[expect(
    clippy::expect_used,
    reason = "the patterns are literals proven by every classify test"
)]
fn pattern(source: &str) -> Regex {
    Regex::new(source).expect("literal pattern compiles")
}

/// The mprime package at `pkg`; the program is `pkg/bin/mprime`.
#[derive(Clone, Debug)]
pub struct Mprime {
    pkg: PathBuf,
}

impl Mprime {
    pub fn new(pkg: impl Into<PathBuf>) -> Self {
        Self { pkg: pkg.into() }
    }

    fn binary(&self) -> PathBuf {
        self.pkg.join("bin/mprime")
    }
}

impl Backend for Mprime {
    fn kind(&self) -> togi_machine::Backend {
        togi_machine::Backend::Mprime
    }

    fn check(&self) -> Result<Ready, Error> {
        let program = self.binary();
        check_executable(NAME, &program)?;
        Ok(Ready {
            detail: program.display().to_string(),
        })
    }

    fn plan(&self, workload: &Workload, dir: &Path, cpus: &[CpuId]) -> Result<Plan, Error> {
        let backend = self.kind();
        let variant = Variant::of(workload.base).ok_or(Error::Unsupported {
            backend,
            workload: workload.id,
        })?;
        if cpus.is_empty() {
            return Err(Error::NoCpus {
                backend,
                workload: workload.id,
            });
        }
        let program = self.binary();
        check_executable(NAME, &program)?;
        let settings = variant.settings(cpus.len());
        let mut work_dir = OsString::from("-W");
        work_dir.push(dir);
        Ok(Plan {
            program,
            args: vec![OsString::from("-t"), work_dir],
            inputs: vec![
                Input {
                    name: PRIME,
                    contents: prime_txt(&settings).into_bytes(),
                },
                Input {
                    name: LOCAL,
                    contents: local_txt(&settings).into_bytes(),
                },
            ],
            watched: vec![RESULTS],
        })
    }

    fn classify(&self, line: &str) -> Line {
        if COMPUTATION.iter().any(|pattern| pattern.is_match(line)) {
            Line::ComputationError
        } else if SETUP.is_match(line) {
            Line::SetupError
        } else if let Some(size) = PROGRESS.captures(line).and_then(|c| c.get(1)) {
            Line::Progress(format!("self-test {} passed", size.as_str()))
        } else {
            Line::Other
        }
    }
}

#[cfg(test)]
mod tests;
