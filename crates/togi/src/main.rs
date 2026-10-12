use std::io::{stderr, stdout};
use std::process::ExitCode;

fn main() -> ExitCode {
    togi_cli::run(
        std::env::args_os(),
        &mut stdout().lock(),
        &mut stderr().lock(),
    )
    .into()
}
