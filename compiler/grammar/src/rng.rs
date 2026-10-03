//! Deterministic randomness and fast hashing, without dependencies.
//!
//! [`Rng`] is SplitMix64: tiny, fast, and statistically good enough for choosing derivations.
//! Every random program is generated from a seed derived with [`mix`] from the run's seed and
//! the program's index, so a failure reproduces from those two numbers alone, whatever the
//! thread count. [`par_seeded`] runs such a series over many threads.

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

    /// A number in `[0, 1)`, uniformly.
    pub fn unit(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64) / ((1u64 << 53) as f64)
    }

    /// True with probability `p`.
    pub fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
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

/// The machine's thread count (1 if it is unknown): the default for [`par_seeded`] runs.
pub fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// Calls `f(state, i, rng)` for each `i` in `0..n`, with `rng` seeded by `mix(seed, i)`, on
/// `threads` threads (at least one), each with a `stack`-byte stack. Thread `t` takes `t`,
/// `t + threads`, … with its own state from `init`. Returns the states in thread order, so what
/// the caller merges from them doesn't depend on timing. A panic in `f` panics here, with the
/// same payload.
pub fn par_seeded<S: Send>(
    n: u64,
    seed: u64,
    threads: usize,
    stack: usize,
    init: impl Fn() -> S + Sync,
    f: impl Fn(&mut S, u64, Rng) + Sync,
) -> Vec<S> {
    let threads = threads.max(1) as u64;
    let (init, f) = (&init, &f);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|t| {
                std::thread::Builder::new()
                    .stack_size(stack)
                    .spawn_scoped(scope, move || {
                        let mut state = init();
                        let mut i = t;
                        while i < n {
                            f(&mut state, i, Rng::new(mix(seed, i)));
                            i += threads;
                        }
                        state
                    })
                    .expect("spawning a worker thread")
            })
            .collect();
        workers
            .into_iter()
            .map(|w| w.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    })
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

    #[test]
    fn seeded_runs_dont_depend_on_the_thread_count() {
        let run = |threads| {
            let states = par_seeded(
                10,
                3,
                threads,
                1 << 20,
                Vec::new,
                |v: &mut Vec<(u64, u64)>, i, mut rng| v.push((i, rng.next_u64())),
            );
            let mut all: Vec<_> = states.into_iter().flatten().collect();
            all.sort_unstable();
            all
        };
        let one = run(1);
        assert_eq!(one.len(), 10);
        assert_eq!(one, run(4));
        assert_eq!(one[2], (2, Rng::new(mix(3, 2)).next_u64()));
    }
}
