use std::num::{NonZeroU16, NonZeroU32};

/// Journal ordinals fit every supported platform's address space.
pub(crate) fn index(value: u32) -> usize {
    match usize::try_from(value) {
        Ok(value) => value,
        Err(_) => unreachable!("togi requires at least a 32-bit address space"),
    }
}

/// Evidence queries retain exact witnesses, but report counts capped at the wire limit.
pub(crate) fn saturating_evidence_count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Hunt progress counters stop at the largest representable journal counter.
pub(crate) fn saturating_hunt_count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Repeated checking classes accumulate requirements up to the journal count limit.
pub(crate) fn saturating_checking_count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// The evidence requirement carries a nonzero, 16-bit count even for larger accumulated quotas.
pub(crate) fn saturating_checking_requirement(value: u32) -> NonZeroU16 {
    NonZeroU16::new(u16::try_from(value).unwrap_or(u16::MAX)).unwrap_or(NonZeroU16::MIN)
}

pub(crate) fn checking_ordinal(value: usize) -> NonZeroU32 {
    NonZeroU32::new(saturating_checking_count(value).saturating_add(1)).unwrap_or(NonZeroU32::MAX)
}

/// Forecast decision counts are reported in a 16-bit uncertainty marker.
pub(crate) fn saturating_forecast_count(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(0, 0; "zero")]
    #[test_case(1, 1; "one")]
    #[test_case(u32::MAX as usize, u32::MAX; "wire limit")]
    #[cfg_attr(target_pointer_width = "64", test_case(u32::MAX as usize + 1, u32::MAX; "overflow"))]
    fn wire_counts_saturate(value: usize, expected: u32) {
        assert_eq!(saturating_evidence_count(value), expected);
        assert_eq!(saturating_hunt_count(value), expected);
        assert_eq!(saturating_checking_count(value), expected);
    }

    #[test_case(0, 1; "zero requirement stays nonzero")]
    #[test_case(1, 1; "one requirement")]
    #[test_case(65_535, 65_535; "requirement limit")]
    #[test_case(65_536, 65_535; "requirement overflow")]
    #[test_case(u32::MAX, 65_535; "largest accumulated requirement")]
    fn checking_requirements_saturate(value: u32, expected: u16) {
        assert_eq!(saturating_checking_requirement(value).get(), expected);
    }

    #[test_case(0, 1; "first ordinal")]
    #[test_case(u32::MAX as usize - 1, u32::MAX; "last ordinal")]
    #[test_case(u32::MAX as usize, u32::MAX; "ordinal increment overflow")]
    #[test_case(usize::MAX, u32::MAX; "largest index")]
    fn checking_ordinals_saturate(value: usize, expected: u32) {
        assert_eq!(checking_ordinal(value).get(), expected);
    }

    #[test_case(0, 0; "zero decisions")]
    #[test_case(65_535, 65_535; "decision limit")]
    #[test_case(65_536, 65_535; "decision overflow")]
    #[test_case(usize::MAX, 65_535; "largest decision count")]
    fn forecast_counts_saturate(value: usize, expected: u16) {
        assert_eq!(saturating_forecast_count(value), expected);
    }

    #[test_case(0; "zero index")]
    #[test_case(u32::MAX; "largest wire index")]
    fn wire_indices_are_exact(value: u32) {
        assert_eq!(u32::try_from(index(value)).unwrap(), value);
    }
}
