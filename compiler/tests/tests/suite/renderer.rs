//! std::gpu as a renderer (AC10): compiler/tests/renderer draws each capability into one
//! quarter of the screen. Fixed pixels hold what each must give, and headless Chrome gives the
//! same frame as the native host (a mean within 0.5/255) and the same state hash.
//! `cargo test -p wrela-tests --test suite renderer:: -- --ignored`.

use std::path::Path;
use wrela_host::{GpuTiming, Host, RunResult, Timing, image};
use wrela_tests::{page, run_in_chrome};

const SIZE: u32 = 128;
/// The largest mean absolute difference between the hosts' frames, in 8-bit steps.
const MEAN_LIMIT: f64 = 0.5;

fn native(dir: &Path) -> RunResult {
    native_at(dir, SIZE, SIZE)
}

/// Two frames of the build in `dir`, `w` × `h`, in the native host.
fn native_at(dir: &Path, w: u32, h: u32) -> RunResult {
    Host::load(dir).expect("load").run_frames(&[0.0, 1.0 / 60.0], w, h).expect("run")
}

/// Two frames of the build in `dir` (its page at `rel`), `w` × `h`, in Chrome and in the native
/// host, which give the same state hash: Chrome's last frame and the native host's.
fn both_hosts(dir: &Path, rel: &str, w: u32, h: u32) -> (Vec<u8>, Vec<u8>) {
    let browser = run_in_chrome(rel, 2, w, h, 60.0);
    let run = native_at(dir, w, h);
    assert_eq!(browser.hash, run.hash_hex(), "the hosts' state hashes differ");
    (browser.frame, run.frame)
}

/// Chrome's frame against the native host's: within a mean of `MEAN_LIMIT`.
fn frames_match(chrome: &[u8], native: &[u8]) {
    let diff = image::compare(chrome, native).expect("same size");
    eprintln!("Chrome against the native host: mean {:.4}/255, max {}/255", diff.mean, diff.max);
    assert!(diff.mean <= MEAN_LIMIT, "mean difference {:.4} over {MEAN_LIMIT}", diff.mean);
}

/// The package at `pkg` (a page `name`) in both hosts: the same state hash, and frames that
/// match.
fn hosts_agree_on(pkg: &str, name: &str) {
    let (dir, rel) = page(pkg, name);
    let (chrome, native) = both_hosts(&dir, &rel, SIZE, SIZE);
    frames_match(&chrome, &native);
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
#[ignore = "needs the runtime"]
fn the_browser_draws_the_native_hosts_frame() {
    hosts_agree_on("compiler/tests/renderer", "renderer-browser");
}

/// A kernel writes a texture's texels (`std::gpu::Texels`), each invocation its own, and a pass
/// draws them (compiler/tests/texels): texel (x, y) of 16 × 16 is red x / 15, green y / 15,
/// blue a half, 8 × 8 pixels a texel.
#[test]
#[ignore = "needs a GPU"]
fn a_kernel_writes_a_textures_texels() {
    let (dir, _) = page("compiler/tests/texels", "texels-native");
    let run = native(&dir);
    for (x, y) in [(0u32, 0u32), (3, 9), (15, 15), (7, 0)] {
        let got = pixel(&run.frame, x * 8 + 4, y * 8 + 4);
        let want = [(x * 255 + 7) / 15, (y * 255 + 7) / 15, 128, 255].map(|c| c as u8);
        near(got, want, &format!("texel ({x}, {y})"));
    }
}

/// Groups (§18): a kernel and a fragment shader each take one parameter of textures, a sampler,
/// a buffer's span and values (compiler/tests/groups). The left half, drawn from a group, is the
/// right half, drawn from the same inputs one by one, to the bit; the kernel's group filled the
/// texture they read.
#[test]
#[ignore = "needs a GPU"]
fn a_group_draws_what_its_fields_draw_one_by_one() {
    halves_alike("compiler/tests/groups", "groups-native");
}

/// The package at `pkg` (a page `name`) in the native host: the frame's left half is its right
/// half, to the bit, and shows the texture a kernel filled.
fn halves_alike(pkg: &str, name: &str) {
    let (dir, _) = page(pkg, name);
    let run = native(&dir);
    let half = SIZE / 2;
    let mut lit = 0;
    for y in 0..SIZE {
        for x in 0..half {
            let (l, r) = (pixel(&run.frame, x, y), pixel(&run.frame, x + half, y));
            assert_eq!(l, r, "pixel ({x}, {y}) against ({}, {y})", x + half);
            lit += usize::from(l[0] > 60 && l[1] > 30);
        }
    }
    assert!(lit > (half * SIZE / 4) as usize, "only {lit} pixels show the filled texture");
}

/// Bound entry points as values (§18): the right half is drawn by entry points bound first, as
/// values, and recorded by functions generic over `Kernel`, `VertexShader<V>` and
/// `FragmentShader<V>` (compiler/tests/bound), with a buffer given whole where the left half
/// gives a span of it. It's the left half, drawn by the same entry points bound where they're
/// recorded, to the bit; the kernels filled the textures they read.
#[test]
#[ignore = "needs a GPU"]
fn bound_entry_points_draw_what_named_ones_draw() {
    halves_alike("compiler/tests/bound", "bound-native");
}

/// A kernel writes a 3D texture's texels (`std::gpu::Texels3d`), and a pass loads and samples
/// them (compiler/tests/volume): texel (x, y, z) of 4 × 4 × 4 is red x / 3, green y / 3, blue
/// z / 3. The top half loads texel ((x / 8) % 4, (y / 8) % 4, x / 32); the bottom half samples
/// at (1/8, 1/8, x / 128), so its blue is blended between layers, and its green is the depth.
#[test]
#[ignore = "needs a GPU"]
fn a_kernel_writes_a_3d_textures_texels() {
    let (dir, _) = page("compiler/tests/volume", "volume-native");
    let run = native(&dir);
    let c = |k: u32| ((k * 255 + 1) / 3) as u8;
    for (x, y) in [(4u32, 4u32), (12, 20), (60, 28), (100, 4), (124, 60)] {
        let want = [c((x / 8) % 4), c((y / 8) % 4), c(x / 32), 255];
        near(pixel(&run.frame, x, y), want, &format!("pixel ({x}, {y})"));
    }
    // Layer k's centre is at w = (k + 1/2) / 4, so blue is (4w - 1/2) / 3, within [0, 1].
    for x in [4u32, 40, 64, 100, 124] {
        let w = (x as f32 + 0.5) / 128.0;
        let blue = ((4.0 * w - 0.5) / 3.0).clamp(0.0, 1.0);
        let want = [0, 255, (blue * 255.0).round() as u8, 255];
        near(pixel(&run.frame, x, 100), want, &format!("sampled at pixel ({x}, 100)"));
    }
}

#[test]
#[ignore = "needs the runtime"]
fn the_browser_writes_3d_texels_as_the_native_host_does() {
    hosts_agree_on("compiler/tests/volume", "volume-browser");
}

/// A vertex shader and a fragment shader with parameters of the same name read their own
/// buffers (compiler/tests/shader_params): the vertex shader's, 1, scales its triangle to cover
/// the screen, and the fragment shader's, 0.25, is its red. (Each read the fragment shader's
/// once: the triangle, a quarter the size, left the right of the screen empty.)
#[test]
#[ignore = "needs a GPU"]
fn each_shader_reads_its_own_parameters() {
    let (dir, _) = page("compiler/tests/shader_params", "shader-params-native");
    let run = native(&dir);
    for (x, y) in [(4, 4), (64, 64), (124, 64), (124, 124)] {
        near(pixel(&run.frame, x, y), [64, 0, 0, 255], &format!("pixel ({x}, {y})"));
    }
}

#[test]
#[ignore = "needs the runtime"]
fn the_browser_writes_texels_as_the_native_host_does() {
    hosts_agree_on("compiler/tests/texels", "texels-browser");
}

/// Texture formats (§18, compiler/tests/formats): a kernel writes r32uint, r32float and rgba8
/// textures, passes draw into r16float, rg16float and r32uint ones (the last a fragment
/// shader's `u32`), and each texel holds what was written. The screen's six bands are each a
/// texture's texels checked against what they should be: green throughout.
#[test]
#[ignore = "needs a GPU"]
fn each_format_holds_what_was_written() {
    let (dir, _) = page("compiler/tests/formats", "formats-native");
    let run = native(&dir);
    bands_green(&run.frame, 6);
}

/// Each of a test's `n` bands, top to bottom, green at every pixel.
fn bands_green(frame: &[u8], n: u32) {
    for band in 0..n {
        let red = (0..SIZE)
            .flat_map(|x| (0..SIZE).map(move |y| (x, y)))
            .filter(|&(_, y)| y * n / SIZE == band)
            .filter(|&(x, y)| pixel(frame, x, y) != [0, 255, 0, 255])
            .count();
        assert_eq!(red, 0, "band {band}: {red} pixels don't hold what was written");
    }
}

#[test]
#[ignore = "needs the runtime"]
fn the_browser_holds_each_format_as_the_native_host_does() {
    let (dir, rel) = page("compiler/tests/formats", "formats-browser");
    let (chrome, _) = both_hosts(&dir, &rel, SIZE, SIZE);
    bands_green(&chrome, 6);
}

/// Dispatch over a domain (§18, compiler/tests/domains): kernels with no bounds check of their
/// own, over 100, over (13, 7) and over a texture, each write exactly what they cover: the
/// screen's three bands are each a dispatch's results checked, green throughout.
#[test]
#[ignore = "needs a GPU"]
fn a_dispatch_over_a_domain_covers_it_exactly() {
    let (dir, _) = page("compiler/tests/domains", "domains-native");
    let run = native(&dir);
    bands_green(&run.frame, 3);
}

#[test]
#[ignore = "needs the runtime"]
fn the_browser_covers_a_domain_as_the_native_host_does() {
    let (dir, rel) = page("compiler/tests/domains", "domains-browser");
    let (chrome, _) = both_hosts(&dir, &rel, SIZE, SIZE);
    bands_green(&chrome, 3);
}

/// Chrome's test mode times each pass on the GPU, as the native host does: the same passes, in
/// the same frames, with the same labels.
#[test]
#[ignore = "needs the runtime"]
fn both_hosts_time_each_pass() {
    let (dir, rel) = page("compiler/tests/renderer", "renderer-timings");
    let run = wrela_tests::ChromeRun {
        timing: Timing::Span,
        ..wrela_tests::ChromeRun::new(2, SIZE, SIZE, 60.0)
    };
    let browser = wrela_tests::run_in_chrome_with(&rel, run).timings;
    let options = wrela_host::Options { timing: Timing::Span, ..wrela_host::Options::default() };
    let mut host = Host::load_with(&dir, &options).expect("load");
    let native = host.run_frames(&[0.0, 1.0 / 60.0], SIZE, SIZE).expect("run").timings;
    let passes = |ts: &[GpuTiming]| -> Vec<(usize, String)> {
        ts.iter().map(|t| (t.frame, t.label.clone())).collect()
    };
    assert!(!native.is_empty(), "the native host timed no passes");
    assert_eq!(passes(&browser), passes(&native), "the passes each host timed");
    assert!(browser.iter().all(|t| t.nanos.is_finite() && t.nanos >= 0.0), "{browser:?}");
    assert!(browser.iter().any(|t| t.nanos > 0.0), "every pass took no time: {browser:?}");
}

/// Indexed indirect draws, each pipeline with the cull mode and depth bias its draw states
/// (compiler/tests/indexed): the left half green, the right red, in the native host and in
/// Chrome, which agree on the frame and the state hash. Its three draws are three pipelines.
#[test]
#[ignore = "needs the runtime"]
fn indexed_draws_cull_and_bias_in_both_hosts() {
    let (dir, rel) = page("compiler/tests/indexed", "indexed-hosts");
    let states: Vec<String> = wrela_tests::manifest(&dir)
        .pipelines
        .iter()
        .map(|p| match &p.stage {
            wrela_abi::manifest::Stage::Render { cull, depth_bias, .. } => {
                format!("{cull:?} {}", depth_bias.constant)
            }
            _ => "compute".into(),
        })
        .collect();
    assert_eq!(states, ["None 0", "Back -1000", "Back 0"]);
    let (chrome, native) = both_hosts(&dir, &rel, SIZE, SIZE);
    for (x, want) in [(8, [0, 255, 0, 255]), (SIZE - 8, [255, 0, 0, 255])] {
        near(pixel(&native, x, SIZE / 2), want, "the native host");
        near(pixel(&chrome, x, SIZE / 2), want, "Chrome");
    }
    frames_match(&chrome, &native);
}

/// A draw's depth test and depth writes (compiler/tests/depth-state, §10.2 of #28): eight cells,
/// each a comparison with writes on or off, and two whose fragments give their own depth
/// (`WithDepth`), nearer and farther than their triangles'. A cell's left half shows whether its quad passed
/// its test over the grey at depth 0.5, and its right half whether a white probe passed after
/// it, so whether the quad wrote its depth. The native host draws what each case should, and
/// Chrome draws the same frame with the same state hash.
#[test]
#[ignore = "needs the runtime"]
fn each_depth_test_and_write_draws_the_same_in_both_hosts() {
    use wrela_abi::manifest::{Compare, DepthState, Stage};
    let (dir, rel) = page("compiler/tests/depth-state", "depth-state");
    let states: Vec<DepthState> = wrela_tests::manifest(&dir)
        .pipelines
        .iter()
        .filter_map(|p| match &p.stage {
            Stage::Render { depth, .. } => Some(*depth),
            _ => None,
        })
        .collect();
    for (compare, write) in [
        (Compare::Less, true),
        (Compare::LessEqual, true),
        (Compare::Equal, false),
        (Compare::Greater, true),
        (Compare::Always, false),
        (Compare::Always, true),
    ] {
        let want = DepthState { compare, write };
        assert!(states.contains(&want), "no pipeline has {want:?}: {states:?}");
    }
    // As wide as the others (`pixel`), and less tall.
    let (chrome, native) = both_hosts(&dir, &rel, SIZE, SIZE * 3 / 4);
    let (grey, white) = ([128, 128, 128, 255], [255, 255, 255, 255]);
    let orange = [255, 128, 0, 255];
    let cases: [(&str, [u8; 4], [u8; 4]); 10] = [
        ("less, nearer, writing", [255, 0, 0, 255], [255, 0, 0, 255]),
        ("less-equal, as near", [0, 255, 0, 255], white),
        ("equal, as near, no writes", [0, 0, 255, 255], white),
        ("greater, farther, writing", [255, 255, 0, 255], white),
        ("always, far, no writes", [255, 0, 255, 255], [255, 0, 255, 255]),
        ("always, far, writing", [0, 255, 255, 255], white),
        ("less, farther", grey, white),
        ("equal, farther", grey, white),
        ("drawn far, its fragments nearer", orange, orange),
        ("drawn near, its fragments farther", grey, white),
    ];
    for (k, (what, left, right)) in cases.iter().enumerate() {
        let (x0, y0) = (32 * (k as u32 % 4), 32 * (k as u32 / 4));
        for (host, frame) in [("the native host", &native), ("Chrome", &chrome)] {
            near(pixel(frame, x0 + 8, y0 + 16), *left, &format!("{what}: the quad, {host}"));
            near(pixel(frame, x0 + 24, y0 + 16), *right, &format!("{what}: the probe, {host}"));
        }
    }
    frames_match(&chrome, &native);
}

/// `std::derive::filtered` antialiases a checkered ground plane at a sample a pixel: against the
/// mean of 256 samples a pixel, its error is at most a quarter of a point sample's (which breaks
/// into moiré past a few metres), on the GPU; and Chrome draws what the native host draws.
#[test]
#[ignore = "needs a GPU and Chrome"]
fn a_filtered_checker_fades_where_a_point_sample_aliases() {
    let (dir, rel) = page("compiler/tests/filtered-aa", "filtered-aa");
    let run = Host::load(&dir).expect("load").run_frames(&[0.0], 384, 96).expect("run");
    let at = |x: usize, y: usize| f64::from(run.frame[4 * (y * 384 + x)]);
    let (mut point, mut filtered) = (0.0, 0.0);
    for y in 0..96 {
        for x in 0..128 {
            let reference = at(x + 256, y);
            point += (at(x, y) - reference).abs();
            filtered += (at(x + 128, y) - reference).abs();
        }
    }
    let n = 128.0 * 96.0;
    eprintln!("against 256 samples: point {:.2}/255, filtered {:.2}/255", point / n, filtered / n);
    assert!(4.0 * filtered <= point, "filtered {filtered}, point {point}");
    let browser = run_in_chrome(&rel, 1, 384, 96, 60.0);
    frames_match(&browser.frame, &run.frame);
}

/// `std::derive::filtered` gives each builtin its footprint's average on the GPU, as on the CPU:
/// the fixture's 16 pixels, a case each (std's `filtered_builtins_are_their_footprints_average`),
/// are all white, natively and in Chrome.
#[test]
#[ignore = "needs a GPU and Chrome"]
fn filtered_builtins_are_their_footprints_average_on_the_gpu() {
    let (dir, rel) = page("compiler/tests/filtered-cases", "filtered-cases");
    let native = Host::load(&dir).expect("load").run_frames(&[0.0], 16, 1).expect("run").frame;
    let browser = run_in_chrome(&rel, 1, 16, 1, 60.0).frame;
    for (host, frame) in [("native", &native), ("Chrome", &browser)] {
        let failed: Vec<usize> = (0..16).filter(|&k| frame[4 * k + 1] != 255).collect();
        assert!(failed.is_empty(), "{host}: cases {failed:?} are not their average");
    }
}
