//! Kernels that share (AC4): compiler/tests/workgroup's kernels use workgroup memory with one
//! barrier (spike 01's `place_vertices` pattern), atomics, an append and an atomic map. The
//! native host's results are what each must give, and headless Chrome gives the same (same
//! state hash, which covers a buffer write of every summary).
//! `cargo test -p wrela-tests --test suite workgroup:: -- --ignored`.

use std::path::Path;
use wrela_host::{Host, Value, frame_time};
use wrela_tests::{page, run_in_chrome};

const FRAMES: u32 = 31;

fn native(dir: &Path) -> (String, Host) {
    let mut host = Host::load(dir).expect("load");
    let times: Vec<f32> = (0..FRAMES).map(|i| frame_time(i, 60.0)).collect();
    let run = host.run_frames(&times, 16, 16).expect("run");
    (run.hash_hex(), host)
}

fn get(host: &mut Host, name: &str) -> u32 {
    match host.call_export(name, &[]).expect(name).as_slice() {
        [Value::I32(v)] => *v as u32,
        other => panic!("{name} returned {other:?}"),
    }
}

#[test]
#[ignore = "needs a GPU"]
fn shared_memory_atomics_appends_and_maps_give_what_they_should() {
    let (dir, _) = page("compiler/tests/workgroup", "workgroup-native");
    let (_, mut host) = native(&dir);
    // Each of 2 × 64 cells sums its 8 corners, corner (x, y, z) of block b being
    // x + 10y + 100z + 1000b.
    let mut cells = 0.0f64;
    for b in 0..2 {
        for (x, y, z) in (0..64).map(|i| (i % 4, (i / 4) % 4, i / 16)) {
            for j in 0..8 {
                let (cx, cy, cz) = (x + (j & 1), y + ((j >> 1) & 1), z + ((j >> 2) & 1));
                cells += f64::from(cx + 10 * cy + 100 * cz + 1000 * b);
            }
        }
    }
    assert_eq!(get(&mut host, "cells_sum"), cells as u32, "the shared corners");
    let values: Vec<u32> = (0..1000u32).map(|i| i * 7919 % 1009).collect();
    assert_eq!(get(&mut host, "bins_sum"), 1000, "every value in one bin");
    let evens: Vec<u32> = values.iter().copied().filter(|v| v % 2 == 0).collect();
    assert_eq!(get(&mut host, "evens_count"), evens.len() as u32, "the appended count");
    assert_eq!(get(&mut host, "evens_sum"), evens.iter().sum::<u32>(), "the appended values");
    assert_eq!(get(&mut host, "map_sum"), 1000, "every value counted under its key");
    assert_eq!(get(&mut host, "map_keys"), 7, "the keys 0 to 6");
}

#[test]
#[ignore = "needs Chrome, python3 and a GPU"]
fn the_browser_gives_the_same_results() {
    let (dir, rel) = page("compiler/tests/workgroup", "workgroup-browser");
    let browser = run_in_chrome(&rel, FRAMES, 16, 16, 60.0);
    let (hash, _) = native(&dir);
    assert_eq!(browser.hash, hash, "the hosts' results differ (their state hashes do)");
}
