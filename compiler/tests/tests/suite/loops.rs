//! CPU loops (plan stage L): compiler/tests/loops's kernels give the same checksums in a build
//! with SIMD (whose loops may run four at a time, language.md §11.2) as in one without, bit for
//! bit. Each step of stage L that takes more loops four at a time keeps this.

use wrela_host::{CpuHost, Value};
use wrela_tests::{one_u32, page, scalar_page};

const KERNELS: [&str; 6] =
    ["k_runs", "k_vecs", "k_branchy", "k_particles", "k_columns", "k_index_sum"];

#[test]
fn the_kernels_agree() {
    let (simd, _) = page("compiler/tests/loops", "loops-simd");
    let (scalar, _) = scalar_page("compiler/tests/loops", "loops-scalar");
    let mut a = CpuHost::load(&simd).expect("load");
    let mut b = CpuHost::load(&scalar).expect("load");
    for k in KERNELS {
        let args = [Value::I32(2)];
        let (x, y) = (one_u32(&mut a, k, &args), one_u32(&mut b, k, &args));
        assert_eq!(x, y, "{k}: {x:#x} with SIMD, {y:#x} without");
    }
}

/// `wrela build --report layout` (plan stage L.4, `wrela_ir::walked`): the particle loop reads
/// two of a `Particle`'s eight `f32`s and skips the other 24 bytes; the loop that pushes them
/// takes each whole.
#[test]
fn the_layout_report_finds_the_particle_loop() {
    let out = wrela_driver::build(&wrela_tests::repo_root().join("compiler/tests/loops"));
    let rows: Vec<_> = out.layout.iter().filter(|(_, w)| w.ty == "Particle").collect();
    let kernel = rows
        .iter()
        .find(|(_, w)| w.function.starts_with("k_particles"))
        .unwrap_or_else(|| panic!("no row for k_particles: {rows:?}"));
    let w = &kernel.1;
    assert_eq!((w.size, w.read, w.written, w.skipped(), w.whole), (32, 8, 0, 24, false), "{w:?}");
    assert!(kernel.0.starts_with("main.wrela:"), "{}", kernel.0);
    let pushed = rows.iter().find(|(_, w)| w.function.starts_with("init")).expect("init's row");
    assert!(pushed.1.whole, "{:?}", pushed.1);
    // The most skipped first.
    assert!(out.layout.windows(2).all(|p| p[0].1.skipped() >= p[1].1.skipped()));
}
