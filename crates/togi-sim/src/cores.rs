use togi_machine::CoreId;

pub(crate) fn core_id(index: usize) -> Option<CoreId> {
    CoreId::new(i64::try_from(index).ok()?).ok()
}

/// The first `count` core IDs in ascending order.
pub(crate) fn ids(count: usize) -> impl Iterator<Item = CoreId> {
    (0..count).filter_map(core_id)
}
