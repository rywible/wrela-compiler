//! CPU loops (plan stage L): compiler/tests/loops's kernels give the same checksums in a build
//! with SIMD (whose loops may run four at a time, language.md §11.2) as in one without, bit for
//! bit. Each step of stage L that takes more loops four at a time keeps this.

use wrela_host::{CpuHost, Value};
use wrela_tests::{one_u32, page, scalar_page};

const KERNELS: [&str; 5] = ["k_runs", "k_vecs", "k_branchy", "k_particles", "k_index_sum"];

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
