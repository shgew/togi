use std::process::ExitCode;

fn main() -> ExitCode {
    togi_cli::main_with(&togi_cli::system::Real)
}
