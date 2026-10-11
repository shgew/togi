use crate::id::ScopeName;
use crate::os::Os;
use crate::timing::Timing;

/// Scope names from `systemctl list-units` output: first token of each line, exact grammar only.
pub(crate) fn parse_listing(output: &str) -> Vec<ScopeName> {
    let _ = output;
    unimplemented!()
}

/// Clears leftovers of an earlier run under one 15 s deadline; success proves nothing runs.
pub(crate) async fn sweep<O: Os>(
    os: &O,
    timing: Timing,
) -> Result<String, crate::error::Error> {
    let _ = (os, timing);
    unimplemented!()
}

/// The preflight check: a throwaway scope pinned to CPU 0, 30 s.
pub(crate) async fn probe<O: Os>(os: &O, timing: Timing) -> Result<String, crate::error::Error> {
    let _ = (os, timing);
    unimplemented!()
}
