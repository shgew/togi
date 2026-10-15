use std::error::Error as _;
use std::io;

use togi_machine::{CoreId, CoreInfo, CoreRank, CpuId};

use super::support::Tree;
use crate::ranking::RankingError;

fn core(id: i64, cpus: &[u16]) -> CoreInfo {
    CoreInfo {
        core: CoreId::new(id).unwrap(),
        cpus: cpus.iter().copied().map(CpuId).collect(),
        ..CoreInfo::default()
    }
}

fn put(root: &Tree, policy: u16, text: &str) {
    root.put(
        &format!("sys/devices/system/cpu/cpufreq/policy{policy}/amd_pstate_prefcore_ranking"),
        text,
    );
}

fn rank(core: i64, value: i64) -> CoreRank {
    CoreRank {
        core: CoreId::new(core).unwrap(),
        value,
    }
}

fn source_of(partial: &togi_machine::Partial<Vec<CoreRank>>) -> &RankingError {
    let togi_machine::Error::Other(error) = &partial.error else {
        panic!("not a ranking error: {partial:?}");
    };
    error.downcast_ref().unwrap()
}

#[test]
fn reads_each_core_through_its_first_logical_cpu() {
    let root = Tree::empty();
    put(&root, 4, "125\n");
    put(&root, 8, "174\n");
    put(&root, 20, "1\n");
    let cores = [core(0, &[4, 20]), core(8, &[8, 24])];
    let ranks = root.ranking(&cores).unwrap();
    assert_eq!(ranks, [rank(0, 125), rank(8, 174)]);
}

#[test]
fn a_missing_file_ends_the_read_and_keeps_the_ranks_before_it() {
    let root = Tree::empty();
    put(&root, 4, "125\n");
    let cores = [core(0, &[4, 20]), core(8, &[8, 24])];
    let partial = root.ranking(&cores).unwrap_err();
    assert_eq!(partial.value, [rank(0, 125)]);
    let error = source_of(&partial);
    assert!(
        matches!(error, RankingError::Read { cpu: CpuId(8), .. }),
        "{error:?}"
    );
    assert_eq!(
        error
            .source()
            .unwrap()
            .downcast_ref::<io::Error>()
            .unwrap()
            .kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn text_that_is_not_an_integer_is_an_error_naming_the_cpu_and_text() {
    let root = Tree::empty();
    put(&root, 4, "not-a-rank\n");
    let partial = root.ranking(&[core(9, &[4, 20])]).unwrap_err();
    assert!(partial.value.is_empty());
    let message = source_of(&partial).to_string();
    assert!(message.contains("cpu 4"), "{message}");
    assert!(message.contains("not-a-rank"), "{message}");
}

#[test]
fn a_value_beyond_i64_is_refused_instead_of_wrapping() {
    let root = Tree::empty();
    put(&root, 4, "99999999999999999999\n");
    let partial = root.ranking(&[core(0, &[4])]).unwrap_err();
    assert!(matches!(source_of(&partial), RankingError::Parse { .. }));
}

#[test]
fn a_negative_rank_is_read_as_is() {
    let root = Tree::empty();
    put(&root, 4, "-3\n");
    assert_eq!(root.ranking(&[core(0, &[4])]).unwrap(), [rank(0, -3)]);
}

#[test]
fn a_core_without_a_cpu_is_an_error() {
    let root = Tree::empty();
    let partial = root.ranking(&[core(3, &[])]).unwrap_err();
    assert!(matches!(source_of(&partial), RankingError::NoCpu { .. }));
}
