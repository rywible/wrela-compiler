//! AC2 and AC3: the ui package's text and widgets. Text is drawn from the fonts as distance
//! fields (ui/text.wrela, ui/draw.wrela); here it's compared with a reference rasterizer
//! (ab_glyph, which computes each pixel's exact coverage from the outlines) drawing the same
//! glyphs at the same places, and its drawing is timed.

use std::path::Path;
use wrela_wfont::{Font, Options};

const FONTS: [(&str, &str); 2] = [
    ("ui/fonts/Inter-Regular-Latin1.ttf", "ui/fonts/inter.wfont"),
    ("ui/fonts/JetBrainsMono-Regular-Latin1.ttf", "ui/fonts/mono.wfont"),
];

fn read(path: &str) -> Vec<u8> {
    std::fs::read(wrela_tests::repo_root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// The checked-in `.wfont` files are what tools/wfont makes from the fonts, each is at most
/// 200 KB, and a build holds each one's bytes once: what a font adds to a build.
#[test]
fn the_fonts_are_current_and_small() {
    let built = crate::built("../../ui/tests/text");
    let wasm = std::fs::read(built.join("game.wasm")).expect("game.wasm");
    for (ttf, wfont) in FONTS {
        let made = wrela_wfont::make(&read(ttf), &Options::default()).expect("makes");
        assert!(
            made == read(wfont),
            "{wfont} is stale: run `cargo run -p wrela-wfont -- {ttf} {wfont}`"
        );
        println!("{wfont}: {} bytes", made.len());
        assert!(made.len() <= 200 * 1024, "{wfont} is {} bytes, over 200 KB", made.len());
        let found = wasm.windows(made.len()).filter(|w| *w == made.as_slice()).count();
        assert_eq!(found, 1, "the build holds {wfont} {found} times");
    }
}

/// Every widget's frame test, each driven by a script of input (ui/tests/widgets/scripts).
#[test]
fn the_widgets_pass_their_frame_tests() {
    let out = wrela_driver::test(&wrela_tests::repo_root().join("ui/tests/widgets"), None);
    let failures: Vec<_> = out.results.iter().filter(|r| r.failure.is_some()).collect();
    assert!(out.passed(), "{:?} {failures:?}", out.diagnostics);
    assert!(out.results.len() >= 9, "{} tests", out.results.len());
}

/// The test strings' sizes and lines, as ui/tests/text lays them out.
const SIZES: [f32; 7] = [10.0, 12.0, 16.0, 24.0, 32.0, 48.0, 72.0];
const LATIN: &str = "The quick brown fox jumps over the lazy dog: 0123456789";
const MORE: &str = "Grüße, Æsir! ¿Qué? £5 © ÿ ñ ß Ø";

fn line_top(face: usize, i: usize) -> f32 {
    let mut y = 10.0 + face as f32 * 700.0;
    for k in 0..i {
        y += SIZES[k / 2] * 1.4 + 6.0;
    }
    y
}

/// Draws `text` with ab_glyph into `img` (RGB floats, `w` wide) at `size` pixels per em, its
/// glyphs at the pens `font` lays them out at, white over what's there.
fn reference_line(
    img: &mut [f32],
    w: usize,
    ttf: &[u8],
    font: &Font,
    text: &str,
    size: f32,
    top: f32,
) {
    use ab_glyph::{Font as _, FontRef, PxScale, point};
    let face = FontRef::try_from_slice(ttf).expect("a font");
    let upem = face.units_per_em().expect("units per em");
    // ab_glyph's scale is the height from descender to ascender, in pixels.
    let scale = PxScale::from(size * face.height_unscaled() / upem);
    let baseline = (top + font.ascender * size + 0.5).floor();
    let h = img.len() / (3 * w);
    for (c, (_, pen)) in text.chars().zip(font.layout(text, size, 10.0)) {
        let id = face.glyph_id(if face.glyph_id(c).0 == 0 { '-' } else { c });
        let glyph = id.with_scale_and_position(scale, point(pen, baseline));
        if let Some(o) = face.outline_glyph(glyph) {
            let b = o.px_bounds();
            o.draw(|x, y, cover| {
                let (px, py) = (b.min.x as i64 + i64::from(x), b.min.y as i64 + i64::from(y));
                if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                    let at = 3 * (py as usize * w + px as usize);
                    for ch in 0..3 {
                        img[at + ch] = img[at + ch] * (1.0 - cover) + cover;
                    }
                }
            });
        }
    }
}

/// The mean difference, in 255ths, between the GPU's frame and the reference over the line
/// at `top`, `size` pixels per em, across the screen.
fn line_mean(frame: &[u8], reference: &[f32], w: usize, top: f32, size: f32) -> f64 {
    let (y0, y1) = (top.floor() as usize, (top + size * 1.4).ceil() as usize);
    let (mut sum, mut n) = (0.0f64, 0usize);
    for y in y0..y1 {
        for x in 0..w {
            for ch in 0..3 {
                let got = f64::from(frame[4 * (y * w + x) + ch]);
                let want = (f64::from(reference[3 * (y * w + x) + ch]) * 255.0).round();
                sum += (got - want).abs();
                n += 1;
            }
        }
    }
    sum / n as f64
}

/// Test strings at 16, 24 and 48 px match the same font rendered by a reference rasterizer
/// within a mean of 4/255; the other sizes, 12 px among them, are measured and printed.
#[test]
#[ignore = "needs a GPU"]
fn text_matches_a_reference_rasterizer() {
    let built = crate::built("../../ui/tests/text");
    let (w, h) = (1280usize, 1400usize);
    let run = wrela_host::Host::load(&built)
        .expect("load")
        .run_frames(&[0.0], w as u32, h as u32)
        .expect("run");
    let bg = [0.1f32, 0.1, 0.12].map(|c| (c * 255.0).round() / 255.0);
    let mut reference = vec![0.0f32; 3 * w * h];
    for px in reference.chunks_exact_mut(3) {
        px.copy_from_slice(&bg);
    }
    let mut worst_checked: f64 = 0.0;
    for (f, (ttf, wfont)) in FONTS.iter().enumerate() {
        let ttf = read(ttf);
        let font = Font::parse(&read(wfont)).expect("a .wfont");
        for i in 0..14 {
            let (text, size) = (if i % 2 == 0 { LATIN } else { MORE }, SIZES[i / 2]);
            reference_line(&mut reference, w, &ttf, &font, text, size, line_top(f, i));
        }
        for i in 0..14 {
            let size = SIZES[i / 2];
            let mean = line_mean(&run.frame, &reference, w, line_top(f, i), size);
            let checked = [16.0, 24.0, 48.0].contains(&size);
            println!(
                "{} {size} px, line {}: mean {mean:.2}/255{}",
                Path::new(wfont).file_stem().and_then(|s| s.to_str()).unwrap_or("?"),
                i % 2,
                if checked { "" } else { " (measured)" }
            );
            if checked {
                worst_checked = worst_checked.max(mean);
                assert!(mean <= 4.0, "{wfont} at {size} px: a mean of {mean:.2}/255");
            }
        }
    }
    // Both pictures, for looking at.
    let dir = wrela_tests::repo_root().join("target/tmp/text");
    std::fs::create_dir_all(&dir).expect("dir");
    run.write_png(dir.join("gpu.png")).expect("png");
    let rgba: Vec<u8> = reference
        .chunks_exact(3)
        .flat_map(|p| {
            [p[0], p[1], p[2]].map(|c| (c * 255.0).round() as u8).into_iter().chain([255])
        })
        .collect();
    wrela_host::image::write_png(&dir.join("reference.png"), w as u32, h as u32, &rgba)
        .expect("png");
    println!("worst checked line: {worst_checked:.2}/255; pictures in {}", dir.display());
}

/// The median of each frame's screen-pass time, from timings of label `label`.
fn median_pass(timings: &[(usize, String, f64)], label: &str) -> f64 {
    let ns: Vec<f64> = timings.iter().filter(|t| t.1.contains(label)).map(|t| t.2).collect();
    assert!(!ns.is_empty(), "no `{label}` timings");
    wrela_tests::median(&ns)
}

/// A screen of source code, 10,000 glyphs and more, draws in at most 0.5 ms of GPU time at
/// 1080p: the median of 30 frames, in each host.
#[test]
#[ignore = "measure: needs Chrome, python3 and a GPU"]
fn a_screen_of_code_draws_within_half_a_millisecond() {
    const FRAMES: u32 = 30;
    let (dir, rel) = wrela_tests::page("ui/tests/code", "code-screen");
    let options = wrela_host::Options { timestamps: true, ..wrela_host::Options::default() };
    let mut host = wrela_host::Host::load_with(&dir, &options).expect("load");
    let times: Vec<f32> = (0..FRAMES).map(|i| wrela_host::frame_time(i, 60.0)).collect();
    let native = host.run_frames(&times, 1920, 1080).expect("run");
    let glyphs = match host.call_export("glyphs", &[]).expect("glyphs").as_slice() {
        [wrela_host::Value::I32(n)] => *n as u32,
        other => panic!("glyphs returned {other:?}"),
    };
    assert!(glyphs >= 10_000, "only {glyphs} glyphs");
    let native: Vec<(usize, String, f64)> =
        native.timings.iter().map(|t| (t.frame, t.label.clone(), t.nanos)).collect();
    drop(host); // the GPU lock, which Chrome's run takes next
    let run = wrela_tests::ChromeRun {
        timestamps: true,
        ..wrela_tests::ChromeRun::new(FRAMES, 1920, 1080, 60.0)
    };
    let chrome = wrela_tests::run_in_chrome_with(&rel, run).timings;
    let (n, c) = (median_pass(&native, "screen"), median_pass(&chrome, "screen"));
    println!(
        "{glyphs} glyphs at 1920x1080: native {:.3} ms, Chrome {:.3} ms (medians of {FRAMES})",
        n / 1e6,
        c / 1e6
    );
    // Chrome is the reference (vision.md). The native host waits for the GPU at every
    // submission when it times, so the GPU idles between frames and its clock drops: its
    // times are reported, not held to the budget.
    assert!(c <= 0.5e6, "{:.3} ms in Chrome, over 0.5 ms", c / 1e6);
}
