//! Shuffle (`z`): the Jukebox, a playlist or a Mix in an order of its own,
//! as YouTube's shuffle button, each video once a round. No random crate:
//! splitmix64, seeded from the keys the standard library makes for its
//! hash maps.

use std::hash::{BuildHasher, RandomState};

/// Where shuffled orders come from. It's `Copy`: a copy gives the same
/// orders again, so what plays after a round can be shown before it's
/// picked.
#[derive(Clone, Copy, Debug)]
pub struct Rng(u64);

impl Default for Rng {
    /// Seeded anew each run.
    fn default() -> Self {
        Self(RandomState::new().hash_one(0u64))
    }
}

impl Rng {
    #[cfg(test)]
    pub fn seeded(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number from 0 to `n - 1` (0 for `n` 0).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next() % n as u64) as usize
    }

    /// `items` in an order of their own, each as likely as any other
    /// (Fisher–Yates).
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.below(i + 1));
        }
    }

    /// A new round of `items`: all of them, shuffled, and the one `last`
    /// says just played not first, so no video plays twice in a row.
    pub fn round<T>(&mut self, mut items: Vec<T>, last: impl Fn(&T) -> bool) -> Vec<T> {
        self.shuffle(&mut items);
        if items.len() > 1 && last(&items[0]) {
            let first = items.remove(0);
            items.push(first);
        }
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shuffle_keeps_every_item_once_and_moves_them() {
        let mut rng = Rng::seeded(7);
        let mut items: Vec<u32> = (0..50).collect();
        rng.shuffle(&mut items);
        assert_ne!(items, (0..50).collect::<Vec<_>>(), "moved");
        items.sort_unstable();
        assert_eq!(items, (0..50).collect::<Vec<_>>(), "each once");
        assert!((0..1000).all(|_| rng.below(3) < 3));
        assert_eq!(rng.below(0), 0);
    }

    #[test]
    fn a_copy_gives_the_same_order_again() {
        let mut rng = Rng::default();
        let mut peek = rng;
        let shown = peek.round((0..20).collect::<Vec<u32>>(), |_| false);
        assert_eq!(rng.round((0..20).collect(), |_| false), shown);
    }

    #[test]
    fn a_round_never_starts_with_what_just_played() {
        for seed in 0..200 {
            let round = Rng::seeded(seed).round(vec![1, 2, 3], |&n| n == 2);
            assert_ne!(round[0], 2, "seed {seed}");
            let mut sorted = round.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, [1, 2, 3]);
        }
        assert_eq!(Rng::seeded(1).round(vec![2], |&n| n == 2), [2], "alone");
    }
}
