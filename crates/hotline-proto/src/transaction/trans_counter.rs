//! The `trans` numbers that one end of a connection gives out.

use std::num::NonZeroU32;

/// Gives out 1 up to `u32::MAX`, then 1 again, and never 0.
#[derive(Debug, PartialEq, Eq)]
pub struct TransCounter(NonZeroU32);

impl TransCounter {
    /// A counter whose first `trans` is 1.
    pub const fn new() -> Self {
        Self(NonZeroU32::MIN)
    }

    /// The next `trans`.
    pub const fn next_trans(&mut self) -> u32 {
        let trans = self.0.get();
        self.0 = match self.0.checked_add(1) {
            Some(next) => next,
            None => NonZeroU32::MIN,
        };
        trans
    }
}

impl Default for TransCounter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_trans_counts_up_from_1() {
        let mut counter = TransCounter::new();
        assert_eq!([counter.next_trans(), counter.next_trans()], [1, 2]);
    }

    #[test]
    fn next_trans_skips_0_after_u32_max() {
        let mut counter = TransCounter(NonZeroU32::MAX);
        assert_eq!([counter.next_trans(), counter.next_trans()], [u32::MAX, 1]);
    }
}
