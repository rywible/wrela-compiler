//! std::gpu as a renderer (AC10): compiler/tests/renderer draws each capability into one
//! quarter of the screen. Fixed pixels hold what each must give, and headless Chrome gives the
//! same frame as the native host (a mean within 0.5/255) and the same state hash.
//! `cargo test -p wrela-tests --test suite renderer:: -- --ignored`.

use wrela_host::{Host, RunResult, image};
use wrela_tests::{page, run_in_chrome};

const SIZE: u32 = 128;
/// The largest mean absolute difference between the hosts' frames, in 8-bit steps.
const MEAN_LIMIT: f64 = 0.5;

fn native(dir: &std::path::Path) -> RunResult {
    Host::load(dir).expect("load").run_frames(&[0.0, 1.0 / 60.0], SIZE, SIZE).expect("run")
}

fn pixel(frame: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = (4 * (y * SIZE + x)) as usize;
    frame[i..i + 4].try_into().expect("4 bytes")
}

fn near(got: [u8; 4], want: [u8; 4], what: &str) {
    let ok = got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 2);
    assert!(ok, "{what}: got {got:?}, want {want:?}");
}

#[test]
#[ignore = "needs a GPU"]
fn each_capability_draws_what_it_should() {
    let (dir, _) = page("compiler/tests/renderer", "renderer-native");
    let run = native(&dir);
    let px = |x, y| pixel(&run.frame, x, y);
    // Top left: the checkerboard, nearest: 16 pixels a texel, red where the row and column add
    // up to an even number.
    near(px(8, 8), [255, 0, 0, 255], "nearest texel (0, 0)");
    near(px(24, 8), [255, 255, 255, 255], "nearest texel (1, 0)");
    near(px(24, 24), [255, 0, 0, 255], "nearest texel (1, 1)");
    // Top right: blended. Pixel (8, 8)'s centre is 1/32 of a texel right of and below texel
    // (0, 0)'s, so 2 × 1/32 × 31/32 of it is white; pixel (16, 8)'s is 17/32 of the way to
    // texel (1, 0).
    near(px(64 + 8, 8), [255, 15, 15, 255], "linear near a texel's centre");
    near(px(64 + 16, 8), [255, 135, 135, 255], "linear between texels");
    // Bottom left: the offscreen pass. The near blue quad covers its left half; the far green
    // one the rest; the red one, behind both, is hidden.
    near(px(8, 64 + 32), [0, 0, 255, 255], "the near quad");
    near(px(56, 64 + 32), [0, 255, 0, 255], "the far quad");
    // Bottom right: the shadow map's left half is nearer the light than depth 0.5. Green is half
    // the light plus a quarter of the depth at texel (0, 0), 0.25.
    near(px(64 + 8, 64 + 32), [0, 16, 51, 255], "in shadow");
    near(px(64 + 56, 64 + 32), [255, 143, 51, 255], "lit");
}

#[test]
#[ignore = "needs Chrome, python3 and a GPU"]
fn the_browser_draws_the_native_hosts_frame() {
    let (dir, rel) = page("compiler/tests/renderer", "renderer-browser");
    // The browser first: this thread holds no GPU lock while Chrome runs.
    let browser = run_in_chrome(&rel, 2, SIZE, SIZE, 60.0);
    let run = native(&dir);
    assert_eq!(browser.hash, run.hash_hex(), "the hosts' state hashes differ");
    let diff = image::compare(&browser.frame, &run.frame).expect("same size");
    eprintln!("Chrome against the native host: mean {:.4}/255, max {}/255", diff.mean, diff.max);
    assert!(diff.mean <= MEAN_LIMIT, "mean difference {:.4} over {MEAN_LIMIT}", diff.mean);
}

/// Chrome's test mode times each pass on the GPU, as the native host does: the same passes, in
/// the same frames, with the same labels.
#[test]
#[ignore = "needs Chrome, python3 and a GPU"]
fn both_hosts_time_each_pass() {
    let (dir, rel) = page("compiler/tests/renderer", "renderer-timings");
    let run = wrela_tests::ChromeRun {
        timestamps: true,
        ..wrela_tests::ChromeRun::new(2, SIZE, SIZE, 60.0)
    };
    let browser = wrela_tests::run_in_chrome_with(&rel, run).timings;
    let options = wrela_host::Options { timestamps: true, ..wrela_host::Options::default() };
    let mut host = Host::load_with(&dir, &options).expect("load");
    let native = host.run_frames(&[0.0, 1.0 / 60.0], SIZE, SIZE).expect("run").timings;
    let want: Vec<(usize, String)> = native.iter().map(|t| (t.frame, t.label.clone())).collect();
    let got: Vec<(usize, String)> = browser.iter().map(|(f, l, _)| (*f, l.clone())).collect();
    assert!(!want.is_empty(), "the native host timed no passes");
    assert_eq!(got, want, "the passes each host timed");
    assert!(browser.iter().all(|(_, _, n)| n.is_finite() && *n >= 0.0), "{browser:?}");
    assert!(browser.iter().any(|(_, _, n)| *n > 0.0), "every pass took no time: {browser:?}");
}
