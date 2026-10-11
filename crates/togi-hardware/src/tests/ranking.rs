use std::error::Error as _;
use std::fs;
use std::io;
use std::path::Path;

use tempfile::TempDir;
use togi_machine::{CoreId, CoreInfo, CoreRank, CpuId};

use crate::ranking::{RankingError, read};

fn core(id: i64, cpus: &[u16]) -> CoreInfo {
    CoreInfo {
        core: CoreId::new(id).unwrap(),
        cpus: cpus.iter().copied().map(CpuId).collect(),
        ..CoreInfo::default()
    }
}

fn put(root: &Path, policy: u16, text: &str) {
    let dir = root.join(format!("sys/devices/system/cpu/cpufreq/policy{policy}"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("amd_pstate_prefcore_ranking"), text).unwrap();
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
    let root = TempDir::new().unwrap();
    put(root.path(), 4, "125\n");
    put(root.path(), 8, "174\n");
    put(root.path(), 20, "1\n");
    let cores = [core(0, &[4, 20]), core(8, &[8, 24])];
    let ranks = read(root.path(), &cores).unwrap();
    assert_eq!(ranks, [rank(0, 125), rank(8, 174)]);
}

#[test]
fn a_missing_file_ends_the_read_and_keeps_the_ranks_before_it() {
    let root = TempDir::new().unwrap();
    put(root.path(), 4, "125\n");
    let cores = [core(0, &[4, 20]), core(8, &[8, 24])];
    let partial = read(root.path(), &cores).unwrap_err();
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
    let root = TempDir::new().unwrap();
    put(root.path(), 4, "not-a-rank\n");
    let partial = read(root.path(), &[core(9, &[4, 20])]).unwrap_err();
    assert!(partial.value.is_empty());
    let message = source_of(&partial).to_string();
    assert!(message.contains("cpu 4"), "{message}");
    assert!(message.contains("not-a-rank"), "{message}");
}

#[test]
fn a_value_beyond_i64_is_refused_instead_of_wrapping() {
    let root = TempDir::new().unwrap();
    put(root.path(), 4, "99999999999999999999\n");
    let partial = read(root.path(), &[core(0, &[4])]).unwrap_err();
    assert!(matches!(source_of(&partial), RankingError::Parse { .. }));
}

#[test]
fn a_negative_rank_is_read_as_is() {
    let root = TempDir::new().unwrap();
    put(root.path(), 4, "-3\n");
    assert_eq!(read(root.path(), &[core(0, &[4])]).unwrap(), [rank(0, -3)]);
}

#[test]
fn a_core_without_a_cpu_is_an_error() {
    let root = TempDir::new().unwrap();
    let partial = read(root.path(), &[core(3, &[])]).unwrap_err();
    assert!(matches!(source_of(&partial), RankingError::NoCpu { .. }));
}
