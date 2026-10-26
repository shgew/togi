//! The content identity every simulated and fake trial runner reports: no executable exists, so the
//! identity is derived from fixed labels. A workload always maps to the same [`ContentId`] and
//! different workloads and backends map to different ones.

use sha2::{Digest as _, Sha256};
use togi_machine::{ContentId, Digest, WorkloadId};

fn label_digest(label: &str) -> Digest {
    Digest::from_bytes(Sha256::digest(label.as_bytes()).into())
}

/// The content identity of `workload`: the executable digest names its backend, the configuration
/// digest names the workload that runs where `workload` is named ([`WorkloadId::runs`]).
#[must_use]
pub fn content_id(workload: WorkloadId) -> ContentId {
    let backend = workload.backend();
    ContentId {
        backend,
        executable: label_digest(&format!("togi-sim executable {backend}")),
        configuration: label_digest(&format!(
            "togi-sim configuration {}",
            workload.runs().as_str()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use togi_machine::{ALL_REGIMES, Backend, workloads};

    fn all() -> Vec<WorkloadId> {
        ALL_REGIMES
            .into_iter()
            .flat_map(|regime| {
                (0..workloads(regime).len()).filter_map(move |index| WorkloadId::new(regime, index))
            })
            .collect()
    }

    #[test]
    fn a_workload_always_has_the_same_identity() {
        for workload in all() {
            assert_eq!(content_id(workload), content_id(workload), "{workload}");
        }
    }

    #[test]
    fn workloads_of_one_regime_differ_in_configuration() {
        for regime in ALL_REGIMES {
            let ids: Vec<_> = (0..workloads(regime).len())
                .filter_map(|index| WorkloadId::new(regime, index))
                .map(|workload| content_id(workload).configuration)
                .collect();
            let distinct: std::collections::BTreeSet<_> = ids.iter().collect();
            assert_eq!(distinct.len(), ids.len(), "{regime:?}");
        }
    }

    #[test]
    fn backends_differ_in_executable_and_share_it_within_a_backend() {
        let mprime = WorkloadId::by_id("mprime-sse-4k-21k").map(content_id);
        let ycruncher = WorkloadId::by_id("ycruncher-bkt-sftv4").map(content_id);
        let (Some(mprime), Some(ycruncher)) = (mprime, ycruncher) else {
            panic!("the catalog lacks the R1 workloads");
        };
        assert_eq!(mprime.backend, Backend::Mprime);
        assert_eq!(ycruncher.backend, Backend::Ycruncher);
        assert_ne!(mprime.executable, ycruncher.executable);
        assert_ne!(mprime.configuration, ycruncher.configuration);
    }

    #[test]
    fn a_class_has_the_identity_of_the_workload_it_runs() {
        for workload in all() {
            assert_eq!(
                content_id(workload),
                content_id(workload.runs()),
                "{workload}"
            );
        }
    }
}
