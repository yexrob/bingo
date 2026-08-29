//! One cut, shared by everything in this crate that has a budget: keep the
//! newest, drop the oldest.

/// The first element of the newest run that still fits `budget`. Everything
/// before it is what the cap leaves out; `xs.len()` means not even the last
/// element fits.
pub fn first_within<T>(xs: &[T], budget: u64, cost: impl Fn(&T) -> u64) -> usize {
    let mut used = 0u64;
    for (index, x) in xs.iter().enumerate().rev() {
        used = used.saturating_add(cost(x));
        if used > budget {
            return index + 1;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ones(n: usize) -> Vec<u64> {
        vec![1; n]
    }

    #[test]
    fn everything_fits_under_a_generous_budget() {
        assert_eq!(first_within(&ones(5), 10, |x| *x), 0);
    }

    #[test]
    fn the_budget_keeps_the_newest_and_drops_the_oldest() {
        assert_eq!(first_within(&ones(5), 3, |x| *x), 2);
    }

    #[test]
    fn a_single_element_over_budget_leaves_nothing() {
        assert_eq!(first_within(&[9u64], 3, |x| *x), 1);
    }

    #[test]
    fn an_empty_slice_starts_at_zero() {
        assert_eq!(first_within::<u64>(&[], 0, |x| *x), 0);
    }
}
