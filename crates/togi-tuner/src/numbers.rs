/// Journal ordinals fit every supported platform's address space.
pub(crate) fn index(value: u32) -> usize {
    match usize::try_from(value) {
        Ok(value) => value,
        Err(_) => unreachable!("togi requires at least a 32-bit address space"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(0; "zero index")]
    #[test_case(u32::MAX; "largest wire index")]
    fn wire_indices_are_exact(value: u32) {
        assert_eq!(u32::try_from(index(value)).unwrap(), value);
    }
}
