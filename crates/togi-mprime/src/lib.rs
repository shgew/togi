//! The mprime backend: [`Mprime`] checks its package, plans workload inputs and launch arguments,
//! and classifies progress and failure output through the backend contract.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use regex::Regex;
use togi_backend::{
    Backend, Error, FileName, Input, Line, Plan, Ready, check_executable, configuration_bytes,
};
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

/// How mprime launches in `dir`, the directory it runs in; with no directory the arguments are
/// the directory-independent part of the configuration.
fn launch_args(dir: &Path) -> Vec<OsString> {
    let mut work_dir = OsString::from("-W");
    work_dir.push(dir);
    vec![OsString::from("-t"), work_dir]
}

/// The input files for `threads` torture threads.
fn inputs(variant: Variant, threads: usize) -> Vec<Input> {
    let settings = variant.settings(threads);
    vec![
        Input {
            name: PRIME,
            contents: prime_txt(&settings).into_bytes(),
        },
        Input {
            name: LOCAL,
            contents: local_txt(&settings).into_bytes(),
        },
    ]
}

/// The output patterns, compiled once when the adapter is built.
#[derive(Clone, Debug)]
struct Patterns {
    computation: [Regex; 2],
    setup: Regex,
    progress: Regex,
}

fn pattern(source: &str) -> Result<Regex, Error> {
    Regex::new(source).map_err(|source| Error::Pattern { name: NAME, source })
}

impl Patterns {
    fn new() -> Result<Self, Error> {
        Ok(Self {
            computation: [
                pattern(
                    r"(?i)FATAL ERROR|ERROR: ILLEGAL SUMOUT|Possible hardware failure|Hardware failure detected|Maximum number of warnings exceeded|TORTURE TEST FAILED|Torture Test completed .* - [1-9][0-9]* errors",
                )?,
                pattern(
                    r"(?i)ERROR: SUM\(INPUTS\) != SUM\(OUTPUTS\)|ERROR: Shift counter corrupt|ERROR: Illegal double encountered|ERROR: FFT data has been zeroed|ERROR: Jacobi error check failed|Warning: ILLEGAL SUMOUT|Warning: SUMOUT MISMATCH",
                )?,
            ],
            setup: pattern(
                r"(?i)Error allocating memory|Out of memory|Unable to allocate memory|Cannot initialize FFT code",
            )?,
            progress: pattern(r"(?i)Self-test ([0-9]+K?)(?: \(thread [0-9]+ of [0-9]+\))? passed")?,
        })
    }
}

/// The mprime package at `pkg`; the program is `pkg/bin/mprime`.
#[derive(Clone, Debug)]
pub struct Mprime {
    pkg: PathBuf,
    patterns: Patterns,
}

impl Mprime {
    /// # Errors
    ///
    /// [`Error::Pattern`] if an output pattern does not compile.
    pub fn new(pkg: impl Into<PathBuf>) -> Result<Self, Error> {
        Ok(Self {
            pkg: pkg.into(),
            patterns: Patterns::new()?,
        })
    }

    fn binary(&self) -> PathBuf {
        self.pkg.join("bin/mprime")
    }

    fn check_with(
        &self,
        check: impl FnOnce(&'static str, &Path) -> Result<(), Error>,
    ) -> Result<Ready, Error> {
        let program = self.binary();
        check(NAME, &program)?;
        Ok(Ready {
            detail: program.display().to_string(),
        })
    }

    fn plan_with(
        &self,
        workload: &Workload,
        dir: &Path,
        cpus: &[CpuId],
        check: impl FnOnce(&'static str, &Path) -> Result<(), Error>,
    ) -> Result<Plan, Error> {
        let backend = self.kind();
        let variant = Variant::of(workload.base).ok_or(Error::Unsupported {
            backend,
            workload: workload.id(),
        })?;
        if cpus.is_empty() {
            return Err(Error::NoCpus {
                backend,
                workload: workload.id(),
            });
        }
        let program = self.binary();
        check(NAME, &program)?;
        Ok(Plan {
            program,
            args: launch_args(dir),
            inputs: inputs(variant, cpus.len()),
            watched: vec![RESULTS],
        })
    }
}

impl Backend for Mprime {
    fn kind(&self) -> togi_machine::Backend {
        togi_machine::Backend::Mprime
    }

    fn check(&self) -> Result<Ready, Error> {
        self.check_with(check_executable)
    }

    fn plan(&self, workload: &Workload, dir: &Path, cpus: &[CpuId]) -> Result<Plan, Error> {
        self.plan_with(workload, dir, cpus, check_executable)
    }

    fn configuration(&self, workload: &Workload) -> Result<Vec<u8>, Error> {
        let variant = Variant::of(workload.base).ok_or(Error::Unsupported {
            backend: self.kind(),
            workload: workload.id(),
        })?;
        Ok(configuration_bytes(
            &launch_args(Path::new("")),
            &inputs(variant, usize::from(workload.threads)),
        ))
    }

    fn classify(&self, line: &str) -> Line {
        let patterns = &self.patterns;
        if patterns
            .computation
            .iter()
            .any(|pattern| pattern.is_match(line))
        {
            Line::ComputationError
        } else if patterns.setup.is_match(line) {
            Line::SetupError
        } else if let Some(size) = patterns.progress.captures(line).and_then(|c| c.get(1)) {
            Line::Progress(format!("self-test {} passed", size.as_str()))
        } else {
            Line::Other
        }
    }
}

#[cfg(test)]
mod tests;
