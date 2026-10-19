use std::time::Duration;

/// Terminal coordinates clip at the terminal's 16-bit coordinate limit.
pub(crate) fn saturating_cells(cells: usize) -> u16 {
    u16::try_from(cells).unwrap_or(u16::MAX)
}

/// Displayed history ordinals stop at the journal ordinal limit.
pub(crate) fn saturating_ordinal(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// Backwards wall-clock corrections contribute no displayed elapsed time.
pub(crate) fn nonnegative_elapsed(nanos: i128) -> Duration {
    let nanos = nanos.max(0).unsigned_abs();
    let seconds = nanos / 1_000_000_000;
    let Ok(seconds) = u64::try_from(seconds) else {
        return Duration::MAX;
    };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the nanosecond remainder is less than one billion"
    )]
    Duration::new(seconds, (nanos % 1_000_000_000) as u32)
}

/// An internal input wait clips at the operating system's signed second limit.
pub(crate) fn saturating_timeout_seconds(seconds: u64) -> i64 {
    i64::try_from(seconds).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{
        nonnegative_elapsed, saturating_cells, saturating_ordinal, saturating_timeout_seconds,
    };
    use std::time::Duration;

    #[test_case::test_case(0, 0; "zero")]
    #[test_case::test_case(65_535, u16::MAX; "coordinate boundary")]
    #[test_case::test_case(65_536, u16::MAX; "coordinate overflow")]
    #[test_case::test_case(usize::MAX, u16::MAX; "address space boundary")]
    fn cells_clip_without_wrapping(input: usize, expected: u16) {
        assert_eq!(saturating_cells(input), expected);
    }

    #[test]
    fn history_ordinals_saturate_at_the_counter_limit() {
        assert_eq!(saturating_ordinal(0), 0);
        assert_eq!(saturating_ordinal(u32::MAX as usize), u32::MAX);
        assert_eq!(saturating_ordinal(usize::MAX), u32::MAX);
    }

    #[test_case::test_case(-1, Duration::ZERO; "backwards correction")]
    #[test_case::test_case(0, Duration::ZERO; "equal timestamps")]
    #[test_case::test_case(1_000_000_001, Duration::new(1, 1); "nanosecond precision")]
    #[test_case::test_case(i128::from(u64::MAX) + 1, Duration::new(18_446_744_073, 709_551_616); "beyond u64 nanoseconds")]
    #[test_case::test_case(i128::MAX, Duration::MAX; "positive duration overflow")]
    #[test_case::test_case(i128::MIN, Duration::ZERO; "negative duration overflow")]
    fn elapsed_time_clamps_in_the_correct_direction(input: i128, expected: Duration) {
        assert_eq!(nonnegative_elapsed(input), expected);
    }

    #[test_case::test_case(0, 0; "zero")]
    #[test_case::test_case(i64::MAX as u64, i64::MAX; "signed limit")]
    #[test_case::test_case(i64::MAX as u64 + 1, i64::MAX; "beyond signed limit")]
    #[test_case::test_case(u64::MAX, i64::MAX; "unsigned limit")]
    fn input_timeout_saturates_instead_of_wrapping(input: u64, expected: i64) {
        assert_eq!(saturating_timeout_seconds(input), expected);
    }
}
