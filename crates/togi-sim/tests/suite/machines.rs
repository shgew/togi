// The committed benchmark machines are shared inputs: they must load, unchanged, and validate.

use togi_sim::Model;
use togi_sim::file;

use crate::common::{committed, machines_dir};

#[test]
fn every_committed_machine_loads_and_validates() {
    let paths = committed();
    assert!(paths.len() >= 18, "{} machines", paths.len());
    for path in paths {
        let (machine, files) = file::load_with_files(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        assert_eq!(files[0], path);
        let model =
            Model::new(&machine, 1).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        assert_eq!(model.cores(), 16, "{}", path.display());
        assert_eq!(model.limits().len(), 16);
    }
}

#[test]
fn the_overlays_name_their_parents_and_the_fitted_machines_their_facts() {
    let (_, files) =
        file::load_with_files(machines_dir().join("target-r7-request-gap.json")).expect("loads");
    assert_eq!(files.len(), 2);
    assert!(files[1].ends_with("target-shared-voltage.json"));
    let fit = file::load(machines_dir().join("target-fit-0.json")).expect("loads");
    assert_eq!(fit.facts.as_deref(), Some("../facts/target.jsonl.gz"));
    let model = Model::new(&fit, 1).expect("valid");
    assert!(!model.bios_context().bios_version.is_empty());
    assert_eq!(model.facts(), Some("../facts/target.jsonl.gz"));
}
