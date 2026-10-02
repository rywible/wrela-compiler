//! Deterministic randomness and fast hashing, without dependencies.
//!
//! [`Rng`] is SplitMix64: tiny, fast, and statistically good enough for choosing derivations.
//! Every random program is generated from a seed derived with [`mix`] from the run's seed and
//! the program's index, so a failure reproduces from those two numbers alone, whatever the
//! thread count.

use std::hash::{BuildHasherDefault, Hasher};

/// A SplitMix64 generator.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number in `0..n`. `n` must be positive. (The modulo bias is negligible for the small
    /// `n` used here.)
    pub fn below(&mut self, n: usize) -> usize {
        assert!(n > 0, "Rng::below(0)");
        (self.next_u64() % n as u64) as usize
    }

    /// True with probability `p`.
    pub fn chance(&mut self, p: f64) -> bool {
        ((self.next_u64() >> 11) as f64) / ((1u64 << 53) as f64) < p
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// Derives an independent seed from two numbers (a run seed and a program index).
pub fn mix(a: u64, b: u64) -> u64 {
    let mut r = Rng::new(a ^ b.rotate_left(32) ^ 0x5851_f42d_4c95_7f2d);
    r.next_u64();
    r.next_u64() ^ b
}

/// The Fx hash (as used in rustc): much faster than SipHash for the small integer keys of the
/// Earley parser, and DoS resistance doesn't matter here.
#[derive(Clone, Copy, Default)]
pub struct FxHasher {
    hash: u64,
}

const FX_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FxHasher {
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(FX_SEED);
    }
}

impl Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut buf = [0u8; 8];
            buf[..chunk.len()].copy_from_slice(chunk);
            self.add(u64::from_le_bytes(buf));
        }
    }

    fn write_u8(&mut self, n: u8) {
        self.add(n.into());
    }

    fn write_u32(&mut self, n: u32) {
        self.add(n.into());
    }

    fn write_u64(&mut self, n: u64) {
        self.add(n);
    }

    fn write_usize(&mut self, n: usize) {
        self.add(n as u64);
    }

    fn finish(&self) -> u64 {
        self.hash
    }
}

pub type FxBuild = BuildHasherDefault<FxHasher>;
pub type FxHashMap<K, V> = std::collections::HashMap<K, V, FxBuild>;
pub type FxHashSet<K> = std::collections::HashSet<K, FxBuild>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix_is_deterministic_and_spread() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        let xs: Vec<u64> = (0..100).map(|_| a.next_u64()).collect();
        let ys: Vec<u64> = (0..100).map(|_| b.next_u64()).collect();
        assert_eq!(xs, ys);
        let mut counts = [0u32; 4];
        let mut r = Rng::new(1);
        for _ in 0..4000 {
            counts[r.below(4)] += 1;
        }
        assert!(counts.iter().all(|&c| (850..1150).contains(&c)), "{counts:?}");
    }

    #[test]
    fn mixed_seeds_differ() {
        assert_ne!(mix(1, 0), mix(1, 1));
        assert_ne!(mix(1, 0), mix(2, 0));
    }
}
