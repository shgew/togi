use std::fmt;

use togi_journal::Seq;
use togi_machine::{CcdId, CoreId, CoreSet};

pub(crate) fn ids(cores: CoreSet) -> String {
    let names: Vec<String> = cores.iter().map(|core| core.get().to_string()).collect();
    format!("[{}]", names.join(" "))
}

fn count(n: i64) -> String {
    format!("{n} count{}", if n == 1 { "" } else { "s" })
}

/// How the failing request of the moved core was known.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Basis {
    NoTelemetry {
        offset: i64,
    },
    Uncovered {
        offset: i64,
        sources: Vec<Seq>,
    },
    NoPass {
        request: f64,
    },
    /// A carried failure: passes recorded after it would set its target, so it takes none.
    Carried {
        request: f64,
    },
    Met {
        request: f64,
        target: f64,
    },
    Toward {
        request: f64,
        target: f64,
    },
}

/// What else the charge says about which core moves.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Note {
    NamedZeroNotTop {
        core: CoreId,
        ccd: CcdId,
    },
    NamedOnEmptyCcd {
        named: CoreId,
        named_ccd: CcdId,
        loaded_ccd: CcdId,
    },
    SteppedDown {
        ccd: CcdId,
        skipped: Vec<CoreSet>,
        group: CoreSet,
    },
    Ordinal {
        n: u32,
        after: u32,
    },
    RerunPassed {
        failure: Seq,
        end: Seq,
    },
    Located(String),
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NamedZeroNotTop { core, ccd } => write!(
                f,
                "named core {:02} failed at CO 0 without being a top requester; back off CCD {}'s top group instead",
                core.get(),
                ccd.0
            ),
            Self::NamedOnEmptyCcd {
                named,
                named_ccd,
                loaded_ccd,
            } => write!(
                f,
                "named core {:02} failed at CO 0 on CCD {}, which had no loaded core; count the failure against loaded CCD {}'s top group",
                named.get(),
                named_ccd.0,
                loaded_ccd.0
            ),
            Self::SteppedDown {
                ccd,
                skipped,
                group,
            } => {
                let skipped: Vec<String> = skipped.iter().map(|group| ids(*group)).collect();
                write!(
                    f,
                    "CCD {} groups [{}] are already at CO 0; step down request order to group {}",
                    ccd.0,
                    skipped.join(" "),
                    ids(*group)
                )
            }
            Self::Ordinal { n, after } => write!(
                f,
                "the unloaded cores have not been checked by a located hunt yet; voltage-targeted backoff {n} for this load since its last passing trial; a later failure can trigger a located hunt after {after} backoffs"
            ),
            Self::Located(clause) => f.write_str(clause),
            Self::RerunPassed { failure, end } => write!(
                f,
                "the rerun of failure #{failure} with every core at CO 0 passed (#{end}), so it counts as unattributed against the cores off CO 0"
            ),
        }
    }
}

/// Why a voltage-targeted backoff moves a core as it does: the structured facts the journal's
/// reason line renders once.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct R7Reason {
    pub(crate) failure: Seq,
    pub(crate) core: CoreId,
    pub(crate) basis: Basis,
    pub(crate) counts: i64,
    pub(crate) notes: Vec<Note>,
    pub(crate) rail: Option<f64>,
}

impl fmt::Display for R7Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "voltage-targeted R7 backoff after failure #{}: core {:02} ",
            self.failure,
            self.core.get()
        )?;
        let counts = count(self.counts);
        match &self.basis {
            Basis::NoTelemetry { offset } => write!(
                f,
                "order came from offsets at CO {offset}; no request telemetry, {counts}"
            )?,
            Basis::Uncovered { offset, sources } => {
                let sources: Vec<String> = sources.iter().map(ToString::to_string).collect();
                write!(
                    f,
                    "order came from offsets at CO {offset}; request measurements [{}] do not cover it, {counts}",
                    sources.join(" ")
                )?;
            }
            Basis::NoPass { request } => {
                write!(f, "request {request:.3} V; no qualifying pass, {counts}")?;
            }
            Basis::Carried { request } => write!(
                f,
                "request {request:.3} V; a carried failure takes no voltage target, {counts}"
            )?,
            Basis::Met { request, target } => write!(
                f,
                "request {request:.3} V already met the passed target {target:.3} V; {counts}"
            )?,
            Basis::Toward { request, target } => {
                write!(f, "request {request:.3} V to {target:.3} V, {counts}")?;
            }
        }
        for note in &self.notes {
            write!(f, "; {note}")?;
        }
        if let Some(rail) = self.rail {
            write!(f, "; {counts} to rise above {rail:.3} V")?;
        }
        Ok(())
    }
}
