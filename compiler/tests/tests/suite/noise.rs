//! Bandlimited noise (AC5, D-077): compiler/tests/noise's fbm, filtered for each pixel's
//! footprint, is within a mean of 2/255 of a 16×-supersampled reference; the same noise
//! sampled once, unfiltered, isn't.

use crate::built;
use wrela_host::Host;
use wrela_tests::f32s;

const LIMIT: f64 = 2.0 / 255.0;

fn mean_diff(a: &[f32], b: &[f32]) -> f64 {
    let sum: f64 = a.iter().zip(b).map(|(x, y)| f64::from((x - y).abs())).sum();
    sum / a.len() as f64
}

#[test]
#[ignore = "needs a GPU"]
fn filtered_noise_matches_the_supersampled_reference() {
    let dir = built("noise");
    let mut host = Host::load(&dir).expect("load");
    let made = host.buffers();
    let [reference, filtered, plain] = made[..] else { panic!("three buffers: {made:?}") };
    let reference = f32s(&host.read_buffer(reference).expect("reference"));
    let filtered = f32s(&host.read_buffer(filtered).expect("filtered"));
    let plain = f32s(&host.read_buffer(plain).expect("plain"));
    let (f, p) = (mean_diff(&filtered, &reference), mean_diff(&plain, &reference));
    println!(
        "mean difference from the reference: filtered {:.2}/255, plain {:.2}/255",
        f * 255.0,
        p * 255.0
    );
    assert!(f <= LIMIT, "the filtered noise is {:.2}/255 from the reference", f * 255.0);
    assert!(p > LIMIT, "the plain noise is only {:.2}/255 from the reference", p * 255.0);
}
