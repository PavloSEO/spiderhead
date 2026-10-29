//! MT19937 generator core and a seeded in-place shuffle.
//!
//! The Louvain partition draws randomness from a seeded MT19937 stream
//! and its shuffle; reproducing the partition requires the same generator
//! and the same `rk_interval` masked-rejection draws used by
//! of the standard in-place shuffle for lists.

/// MT19937 state as seeded by `mt19937_seed` / `init_genrand`.
pub struct Mt19937 {
    mt: [u32; 624],
    idx: usize,
}

impl Mt19937 {
    /// Seed with `init_genrand(seed)`, the standard integer-seed path.
    pub fn seed(seed: u32) -> Self {
        let mut mt = [0u32; 624];
        mt[0] = seed;
        for i in 1..624 {
            mt[i] = 1812433253u32
                .wrapping_mul(mt[i - 1] ^ (mt[i - 1] >> 30))
                .wrapping_add(i as u32);
        }
        Self { mt, idx: 624 }
    }

    fn twist(&mut self) {
        const N: usize = 624;
        const M: usize = 397;
        for i in 0..N {
            let y = (self.mt[i] & 0x80000000) | (self.mt[(i + 1) % N] & 0x7fffffff);
            self.mt[i] = self.mt[(i + M) % N] ^ (y >> 1) ^ if y & 1 != 0 { 0x9908b0df } else { 0 };
        }
        self.idx = 0;
    }

    /// `mt19937_next` — tempered 32-bit draw.
    pub fn next_u32(&mut self) -> u32 {
        if self.idx >= 624 {
            self.twist();
        }
        let mut y = self.mt[self.idx];
        self.idx += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c5680;
        y ^= (y << 15) & 0xefc60000;
        y ^= y >> 18;
        y
    }

    /// Uniform integer in `0..=max` by masked rejection: 32-bit draws for
    /// `max <= 0xffffffff`, 64-bit draws otherwise.
    pub fn interval(&mut self, max: u64) -> u64 {
        if max == 0 {
            return 0;
        }
        let mut mask = max;
        mask |= mask >> 1;
        mask |= mask >> 2;
        mask |= mask >> 4;
        mask |= mask >> 8;
        mask |= mask >> 16;
        mask |= mask >> 32;
        loop {
            let value = if max <= 0xffff_ffff {
                (self.next_u32() as u64) & mask
            } else {
                (((self.next_u32() as u64) << 32) | self.next_u32() as u64) & mask
            };
            if value <= max {
                return value;
            }
        }
    }
}

/// Shared `RandomState` object: holds the MT19937 core so successive
/// `randomize` calls consume the stream in order.
pub struct RandomState {
    mt: Mt19937,
}

impl RandomState {
    /// New generator for `seed`.
    pub fn new(seed: u32) -> Self {
        Self {
            mt: Mt19937::seed(seed),
        }
    }

    /// `random_state.shuffle(list)` — in-place Fisher–Yates with
    /// `j = rk_interval(i)` for `i` from `n-1` down to 1.
    pub fn shuffle_inplace<T>(&mut self, items: &mut [T]) {
        let n = items.len();
        for i in (1..n).rev() {
            let j = self.mt.interval(i as u64) as usize;
            items.swap(i, j);
        }
    }
}

/// Shuffle a copy of `items` with the shared RNG state; the input is left unchanged.
pub fn randomize<T: Clone>(items: &[T], state: &mut RandomState) -> Vec<T> {
    let mut out = items.to_vec();
    state.shuffle_inplace(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected order for seed 42, recorded from the reference generator.
    #[test]
    fn shuffle_matches_numpy_10() {
        let mut rs = RandomState::new(42);
        let mut items: Vec<i64> = (0..10).collect();
        rs.shuffle_inplace(&mut items);
        assert_eq!(items, [8, 1, 5, 0, 7, 2, 9, 4, 3, 6]);
    }

    #[test]
    fn shuffle_matches_numpy_strs() {
        let mut rs = RandomState::new(42);
        let mut items = vec!["a", "b", "c", "d", "e"];
        rs.shuffle_inplace(&mut items);
        assert_eq!(items, ["b", "e", "c", "a", "d"]);
    }

    #[test]
    fn shuffle_matches_numpy_20() {
        let mut rs = RandomState::new(42);
        let mut items: Vec<i64> = (0..20).collect();
        rs.shuffle_inplace(&mut items);
        assert_eq!(
            items,
            [0, 17, 15, 1, 8, 5, 11, 3, 18, 16, 13, 2, 9, 19, 4, 12, 7, 10, 14, 6]
        );
    }
}
