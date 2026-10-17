use std::io::Write as _;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use togi_tool_fit::{Ensemble, Mode, Options, run};

/// Fit simulated machines to the real trial facts and check the fit forward on later sessions.
#[derive(Debug, Parser)]
#[command(name = "togi-tool-fit")]
struct Cli {
    /// The gzip extract of decisive trial facts.
    #[arg(long, default_value = "tools/bench/facts/target.jsonl.gz")]
    facts: PathBuf,
    /// The directory machine files are written to.
    #[arg(long, default_value = "tools/bench/machines")]
    out: PathBuf,
    /// The bootstrap seed: refit N resamples with the stream seeded SEED+N.
    #[arg(long, default_value_t = 263)]
    seed: u64,
    /// The number of whole-trial bootstrap refits.
    #[arg(long, default_value_t = 8)]
    bootstrap: usize,
    /// The threads to run on; every CPU when absent. The results do not depend on it.
    #[arg(long)]
    jobs: Option<NonZeroUsize>,
    /// Run only the forward-chained check: fit no ensemble and write no machine files.
    #[arg(long)]
    forward_only: bool,
    /// With --forward-only, leave the newest N sessions out of the check.
    #[arg(long, default_value_t = 0, requires = "forward_only")]
    seal: usize,
    /// With --forward-only, also score these committed machine files, as one ensemble, on the same held-out
    /// sessions beside the fit: each trial's probability is the mean over all of them. They are not fitted.
    #[arg(long, num_args = 1.., value_name = "MACHINE", requires = "forward_only")]
    reference_machine: Vec<PathBuf>,
    /// Fit all decisive facts to target-shared-voltage.json; in-sample only, not forward-validated.
    #[arg(
        long,
        conflicts_with_all = ["forward_only", "seal", "seed", "bootstrap"]
    )]
    shared_voltage_in_sample: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mode = if cli.shared_voltage_in_sample {
        Mode::SharedVoltage
    } else if cli.forward_only {
        Mode::Forward { seal: cli.seal }
    } else {
        Mode::Ensemble(Ensemble {
            seed: cli.seed,
            bootstrap: cli.bootstrap,
        })
    };
    let options = Options {
        facts: cli.facts,
        out: cli.out,
        jobs: cli.jobs,
        reference: cli.reference_machine,
        mode,
    };
    let mut stdout = std::io::stdout();
    match run(&options, &mut stdout) {
        Ok(elapsed) => {
            let printed = match elapsed.forward {
                Some(forward) => writeln!(stdout, "Forward-chained elapsed: {forward:.3?}"),
                None => Ok(()),
            }
            .and_then(|()| writeln!(stdout, "Fit elapsed: {:.3?}", elapsed.total));
            match printed {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    let _ = writeln!(
                        std::io::stderr(),
                        "togi-tool-fit: write the timing: {error}"
                    );
                    ExitCode::FAILURE
                }
            }
        }
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "togi-tool-fit: {error}");
            ExitCode::FAILURE
        }
    }
}
