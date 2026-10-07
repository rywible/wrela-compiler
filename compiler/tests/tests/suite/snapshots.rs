//! Snapshots (AC6, language.md §6.11), on compiler/tests/snapshots: keyframes are clones taken
//! with `clone_into`, checksums are the derived `StateHash`, and rollback restores a keyframe
//! and replays the inputs since.

use std::time::Instant;
use wrela_host::{CpuBuild, CpuHost, Value};
use wrela_tests::page;

fn call(host: &mut CpuHost, name: &str, args: &[Value]) -> Vec<Value> {
    host.call_export(name, args).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn u64_of(v: &[Value]) -> u64 {
    match v {
        [Value::I64(x)] => *x as u64,
        other => panic!("returned {other:?}"),
    }
}

fn u32_of(v: &[Value]) -> u32 {
    match v {
        [Value::I32(x)] => *x as u32,
        other => panic!("returned {other:?}"),
    }
}

/// xorshift64*: the test's inputs and choices, the same every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const SLOTS: usize = 8;
const EVERY: u32 = 25;

/// Over 10,000 ticks, with random restores of a keyframe and replays of the inputs since,
/// every replayed tick gives the hash it gave the first time. Each keyframe has the state's
/// hash, and taking or restoring one allocates nothing (the keyframes have grown to size).
#[test]
fn replayed_ticks_give_the_same_hashes() {
    let (dir, _) = page("compiler/tests/snapshots", "snapshots-replay");
    let mut host = CpuBuild::load(&dir).expect("load").start_with(1).expect("start");
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    // The count works: a clone allocates its 3 `Vec`s and 4 strings' buffers.
    assert_eq!(u32_of(&call(&mut host, "clone_allocations", &[])), 7);
    let initial = u64_of(&call(&mut host, "hash", &[]));
    // `inputs[t]` is tick t's input; `hashes[t]` the hash after it.
    let mut inputs: Vec<u32> = Vec::new();
    let mut hashes: Vec<u64> = Vec::new();
    let hash_at = |hashes: &[u64], t: u32| if t == 0 { initial } else { hashes[t as usize - 1] };
    let mut slots: [Option<u32>; SLOTS] = [None; SLOTS];
    let (mut ticks, mut replayed, mut restores) = (0u32, 0u32, 0u32);
    while ticks < 10_000 {
        let t = u32_of(&call(&mut host, "tick_count", &[]));
        if t.is_multiple_of(EVERY) {
            let slot = (t / EVERY) as usize % SLOTS;
            call(&mut host, "keyframe", &[Value::I32(slot as i32)]);
            assert_eq!(u32_of(&call(&mut host, "allocated", &[])), 0, "a keyframe at tick {t}");
            let kept = u64_of(&call(&mut host, "keyframe_hash", &[Value::I32(slot as i32)]));
            assert_eq!(kept, hash_at(&hashes, t), "the keyframe of tick {t}");
            slots[slot] = Some(t);
        }
        if t as usize == inputs.len() {
            let input = rng.below(1000) as u32;
            inputs.push(input);
            call(&mut host, "tick", &[Value::I32(input as i32)]);
            hashes.push(u64_of(&call(&mut host, "hash", &[])));
        } else {
            call(&mut host, "tick", &[Value::I32(inputs[t as usize] as i32)]);
            let h = u64_of(&call(&mut host, "hash", &[]));
            assert_eq!(h, hashes[t as usize], "tick {t}, replayed");
            replayed += 1;
        }
        ticks += 1;
        if rng.below(40) == 0 {
            let kept: Vec<usize> = (0..SLOTS).filter(|&s| slots[s].is_some()).collect();
            let slot = kept[rng.below(kept.len() as u64) as usize];
            call(&mut host, "restore", &[Value::I32(slot as i32)]);
            assert_eq!(u32_of(&call(&mut host, "allocated", &[])), 0, "a restore");
            let k = slots[slot].expect("kept");
            assert_eq!(
                u64_of(&call(&mut host, "hash", &[])),
                hash_at(&hashes, k),
                "restored to {k}"
            );
            restores += 1;
        }
    }
    assert!(restores > 100 && replayed > 1000, "{restores} restores, {replayed} ticks replayed");
}

/// Measured, not gated (§6.11 estimates 0.3 ms for a copy and about the same for a hash): a
/// copy with `clone_into`, a `clone`, and a whole-state hash, of 10,000 entities of 256 bytes,
/// in wasmtime. `cargo test --release -p wrela-tests --test suite snapshot_costs -- --ignored
/// --nocapture` prints them.
#[test]
#[ignore = "long: a measurement; run it with --release"]
fn snapshot_costs() {
    let (dir, _) = page("compiler/tests/snapshots", "snapshots-costs");
    let mut host = CpuBuild::load(&dir).expect("load").start_with(1).expect("start");
    const N: i32 = 20;
    let mut time = |name: &str| {
        let args = [Value::I32(N)];
        call(&mut host, name, &args); // warm-up, not counted
        let mut runs: Vec<f64> = (0..9)
            .map(|_| {
                let t = Instant::now();
                call(&mut host, name, &args);
                t.elapsed().as_secs_f64() * 1e3 / f64::from(N)
            })
            .collect();
        runs.sort_by(f64::total_cmp);
        runs[4]
    };
    let copy = time("copy_big");
    let clone = time("clone_big");
    let hash = time("hash_big");
    println!("10,000 entities of 256 bytes (2.56 MB), median of 9 runs of {N}:");
    println!("  clone_into  {copy:.3} ms");
    println!("  clone       {clone:.3} ms");
    println!("  state_hash  {hash:.3} ms");
    assert!(matches!(call(&mut host, "spare_matches", &[])[..], [Value::I32(1)]));
}
