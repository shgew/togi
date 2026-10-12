pub(crate) fn float(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// The `p`-quantile of an ascending slice by linear interpolation between neighbouring ranks; 0 for no values.
pub(crate) fn percentile(sorted: &[f64], p: f64) -> f64 {
    let Some(last) = sorted.len().checked_sub(1) else {
        return 0.0;
    };
    let at = p * float(last);
    let low = at.floor();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "`at` lies in [0, len - 1], so its floor is a valid index"
    )]
    let low_index = low as usize;
    let high_index = (low_index + 1).min(last);
    sorted[low_index] + (sorted[high_index] - sorted[low_index]) * (at - low)
}

/// The share of trials a real fact answered; 0 for no trials.
pub(crate) fn share(real: u32, trials: u32) -> f64 {
    if trials == 0 {
        0.0
    } else {
        f64::from(real) / f64::from(trials)
    }
}
