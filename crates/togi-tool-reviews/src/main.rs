use std::io::Write as _;
use std::process::ExitCode;

use togi_tool_reviews::{System, Tools, dispatch};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let mut system = System::from_process();
    let gh_token = std::env::var_os("GH_TOKEN")
        .map(|token| token.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = std::env::var_os("TMPDIR")
        .map(|dir| dir.to_string_lossy().into_owned())
        .filter(|dir| !dir.is_empty())
        .unwrap_or_else(|| "/tmp".to_owned());
    let (mut stdin, mut stdout, mut stderr) =
        (std::io::stdin(), std::io::stdout(), std::io::stderr());
    let mut t = Tools {
        programs: &mut system,
        gh_token,
        tmp,
        stdin: &mut stdin,
        stdout: &mut stdout,
        stderr: &mut stderr,
    };
    match dispatch(&mut t, &args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "reviews: {error}");
            ExitCode::FAILURE
        }
    }
}
