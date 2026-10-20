//! `std::tool::image` (language.md §15.5) on the tool host: the fixtures in compiler/tests/images
//! (a PNG, a JPEG and a WebP of four 8×8 quadrants) decode to their pixels; a Lanczos3 resize
//! matches a reference computed here from the formula, within 1 per channel; an encode decodes
//! back (a PNG exactly, a WebP within a PSNR bound), to the same bytes each time; a 1920×1080
//! frame goes through blobs into a 960×540 cover, as the release tool (studio/release) makes one;
//! and an image too large for the blob budget is refused from its header, before it's decoded.

use crate::tool::{printer, reached, run, scratch, signals, tool_build};
use std::path::{Path, PathBuf};
use wrela_host::tool::{BlobBudget, Caps, End, Grants, Testing};

/// A tool that does what its first argument says, with `in` and `out`:
/// - `decode`: each fixture, as `out/<name>.rgba`;
/// - `resize`: `in/pattern.rgba` (37×23) to 16×11 and 50×40, as `out/down.rgba`, `out/up.rgba`;
/// - `round`: the pattern as a PNG and decoded back (`out/pattern.png`, `out/png.rgba`), and as a
///   WebP at quality 90, method 6, twice (`out/a.webp`, `out/b.webp`), decoded back
///   (`out/webp.rgba`);
/// - `frame`: `in/frame.rgba` (1920×1080) fit into 960×540 (`out/cover.rgba`), as a WebP
///   (`out/cover.webp`) decoded back (`out/cover-back.rgba`), and the frame as a PNG decoded
///   back (`out/frame-back.rgba`);
/// - `huge`: `in/huge.png`, read as bytes, decoded: it prints why it can't be;
/// - `stall`: starts a resize, and waits until it's interrupted.
const IMAGE_TOOL: &str = r#"
use std::io::{IoError, print}
use std::job::{Job, Step}
use std::tool::{RUNNING, answer, args}
use std::tool::blob
use std::tool::blob::{Blob, Data}
use std::tool::files
use std::tool::image
use std::tool::time

fn blob_of(polled: take Result<Vec<u8>, IoError>, what: str) -> Data {
    match blob::read_handle(take polled) {
        Ok(b) => Data::Blob(b),
        Err(e) => panic(f"{what}: {e.message()}"),
    }
}

fn image_of(polled: take Result<Vec<u8>, IoError>, what: str) -> image::Image {
    match image::read_image(take polled) {
        Ok(i) => i,
        Err(e) => panic(f"{what}: {e.message()}"),
    }
}

fn encoded_of(polled: take Result<Vec<u8>, IoError>, what: str) -> Data {
    match image::read_encoded(take polled) {
        Ok(d) => d,
        Err(e) => panic(f"{what}: {e.message()}"),
    }
}

fn done(polled: take Result<Vec<u8>, IoError>, what: str) {
    if let Err(e) = answer(take polled) {
        panic(f"{what}: {e.message()}")
    }
}

fn blob_in(d: Data) -> Blob {
    match d {
        Data::Blob(b) => b,
        Data::Bytes(_) => panic("pixels in a blob"),
    }
}

@job
fn main(given: take Vec<String>) -> u32 {
    let what = given[0].clone()
    if what.as_str() == "decode" {
        let names = [String::from("quadrants.png"), String::from("quadrants.jpg"), String::from("quadrants.webp")]
        for i in 0..3 {
            let name = names[i].clone()
            var r = files::read_blob(f"in/{name}".as_str())
            var got = r.poll()
            while got.is_none() {
                yield
                got = r.poll()
            }
            let Some(p) = take got else { return 90 }
            let file = blob_of(take p, "read")
            var d = image::decode(file)
            var dg = d.poll()
            while dg.is_none() {
                yield
                dg = d.poll()
            }
            let Some(dp) = take dg else { return 90 }
            let decoded = image_of(take dp, "decode")
            var w = files::write_blob(f"out/{name}.rgba".as_str(), blob_in(decoded.rgba))
            var wg = w.poll()
            while wg.is_none() {
                yield
                wg = w.poll()
            }
            let Some(wp) = take wg else { return 90 }
            done(take wp, "write")
            print(f"{name}: {decoded.width}x{decoded.height}")
        }
        return 0
    }
    if what.as_str() == "stall" {
        // A resize that's still running when the tool ends.
        let small = image::Image { width: 4, height: 4, rgba: Data::Bytes(Vec::filled(64, u8(9))) }
        let _r = image::resize(small, image::Size { width: 2, height: 2 })
        var t = time::after(600.0)
        while t.poll().is_none() { yield }
        return 0
    }
    if what.as_str() == "huge" {
        var r = files::read("in/huge.png")
        var got = r.poll()
        while got.is_none() {
            yield
            got = r.poll()
        }
        let Some(p) = take got else { return 90 }
        let Ok(bytes) = answer(take p) else { return 91 }
        var d = image::decode(Data::Bytes(take bytes))
        var dg = d.poll()
        while dg.is_none() {
            yield
            dg = d.poll()
        }
        let Some(dp) = take dg else { return 90 }
        match image::read_image(take dp) {
            Ok(_) => print("decoded"),
            Err(e) => print(f"refused: {e.message()}"),
        }
        return 0
    }
    let source = if what.as_str() == "frame" { "in/frame.rgba" } else { "in/pattern.rgba" }
    let w0 = if what.as_str() == "frame" { u32(1920) } else { u32(37) }
    let h0 = if what.as_str() == "frame" { u32(1080) } else { u32(23) }
    var r = files::read_blob(source)
    var got = r.poll()
    while got.is_none() {
        yield
        got = r.poll()
    }
    let Some(p) = take got else { return 90 }
    let source = image::Image { width: w0, height: h0, rgba: blob_of(take p, "read") }
    // Each output: what to write, and where.
    var outs: Vec<Data> = Vec::new()
    var paths: Vec<String> = Vec::new()
    if what.as_str() == "resize" {
        var a = image::resize(source.clone(), image::Size { width: 16, height: 11 })
        var b = image::resize(source.clone(), image::Size { width: 50, height: 40 })
        var ag = a.poll()
        var bg = b.poll()
        while ag.is_none() || bg.is_none() {
            yield
            if ag.is_none() {
                ag = a.poll()
            }
            if bg.is_none() {
                bg = b.poll()
            }
        }
        let Some(ap) = take ag else { return 90 }
        let Some(bp) = take bg else { return 90 }
        outs.push(image_of(take ap, "down").rgba)
        paths.push(String::from("out/down.rgba"))
        outs.push(image_of(take bp, "up").rgba)
        paths.push(String::from("out/up.rgba"))
    }
    if what.as_str() == "round" {
        var e = image::encode_png(source.clone())
        var eg = e.poll()
        while eg.is_none() {
            yield
            eg = e.poll()
        }
        let Some(ep) = take eg else { return 90 }
        let png = encoded_of(take ep, "encode_png")
        var d = image::decode(png.clone())
        var dg = d.poll()
        while dg.is_none() {
            yield
            dg = d.poll()
        }
        let Some(dp) = take dg else { return 90 }
        outs.push(take png)
        paths.push(String::from("out/pattern.png"))
        outs.push(image_of(take dp, "decode png").rgba)
        paths.push(String::from("out/png.rgba"))
        var wa = image::encode_webp(source.clone(), 90.0, 6)
        var wb = image::encode_webp(source.clone(), 90.0, 6)
        var ga = wa.poll()
        var gb = wb.poll()
        while ga.is_none() || gb.is_none() {
            yield
            if ga.is_none() {
                ga = wa.poll()
            }
            if gb.is_none() {
                gb = wb.poll()
            }
        }
        let Some(pa) = take ga else { return 90 }
        let Some(pb) = take gb else { return 90 }
        let a = encoded_of(take pa, "encode_webp")
        let b = encoded_of(take pb, "encode_webp")
        var dw = image::decode(a.clone())
        var dwg = dw.poll()
        while dwg.is_none() {
            yield
            dwg = dw.poll()
        }
        let Some(dwp) = take dwg else { return 90 }
        outs.push(take a)
        paths.push(String::from("out/a.webp"))
        outs.push(take b)
        paths.push(String::from("out/b.webp"))
        outs.push(image_of(take dwp, "decode webp").rgba)
        paths.push(String::from("out/webp.rgba"))
    }
    if what.as_str() == "frame" {
        let size = image::fit(source.size(), image::Size { width: 960, height: 540 })
        var c = image::resize(source.clone(), size)
        var cg = c.poll()
        while cg.is_none() {
            yield
            cg = c.poll()
        }
        let Some(cp) = take cg else { return 90 }
        let cover = image_of(take cp, "resize")
        var e = image::encode_webp(cover.clone(), 90.0, 6)
        var f = image::encode_png(source.clone())
        var eg = e.poll()
        var fg = f.poll()
        while eg.is_none() || fg.is_none() {
            yield
            if eg.is_none() {
                eg = e.poll()
            }
            if fg.is_none() {
                fg = f.poll()
            }
        }
        let Some(ep) = take eg else { return 90 }
        let Some(fp) = take fg else { return 90 }
        let webp = encoded_of(take ep, "encode_webp")
        let png = encoded_of(take fp, "encode_png")
        var dw = image::decode(webp.clone())
        var dp = image::decode(png)
        var dwg = dw.poll()
        var dpg = dp.poll()
        while dwg.is_none() || dpg.is_none() {
            yield
            if dwg.is_none() {
                dwg = dw.poll()
            }
            if dpg.is_none() {
                dpg = dp.poll()
            }
        }
        let Some(dwp) = take dwg else { return 90 }
        let Some(dpp) = take dpg else { return 90 }
        let back = image_of(take dwp, "decode webp")
        let frame = image_of(take dpp, "decode png")
        print(f"cover {cover.width}x{cover.height}, back {back.width}x{back.height}, frame {frame.width}x{frame.height}")
        outs.push(take cover.rgba)
        paths.push(String::from("out/cover.rgba"))
        outs.push(take webp)
        paths.push(String::from("out/cover.webp"))
        outs.push(take back.rgba)
        paths.push(String::from("out/cover-back.rgba"))
        outs.push(take frame.rgba)
        paths.push(String::from("out/frame-back.rgba"))
    }
    for i in 0..outs.len() {
        var w = files::write_blob(paths[i].as_str(), blob_in(outs[i].clone()))
        var wg = w.poll()
        while wg.is_none() {
            yield
            wg = w.poll()
        }
        let Some(wp) = take wg else { return 90 }
        done(take wp, "write")
    }
    0
}

pub fn tool_init() -> Job<main> {
    main.start(args())
}

pub fn tool(job: mut Job<main>) -> u32 {
    match job.resume() {
        Step::Yielded => RUNNING,
        Step::Done(status) => status,
    }
}
"#;

/// The fixtures' directory.
fn images() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("images")
}

/// Runs the image tool for `what`, reading `input` and writing a new `out`, with `budget`: what
/// it printed, and `out`.
fn image_tool(what: &str, input: &Path, budget: Option<BlobBudget>) -> (Vec<String>, PathBuf) {
    let build = tool_build(&format!("image-tool-{what}"), IMAGE_TOOL);
    let out = scratch(&format!("image-{what}-out"));
    let grants = Grants::default().root("in", input, false).root("out", &out, true);
    let (lines, mut options) = printer(|_| {});
    options.caps = Caps::open(&grants, |_| None).expect("granted");
    options.args = vec![what.to_string()];
    if let Some(b) = budget {
        options.blob_budget = b;
    }
    let ended = run(&build, options);
    let lines = lines.lock().expect("the lines").clone();
    assert_eq!(ended.end, End::Exit(0), "{lines:?}");
    (lines, out)
}

/// The fixtures' four quadrants, as made: top left, top right, bottom left, bottom right.
const QUADRANTS: [[u8; 4]; 4] =
    [[220, 30, 30, 255], [30, 200, 60, 255], [40, 60, 210, 255], [240, 240, 240, 128]];

#[test]
fn png_jpeg_and_webp_fixtures_decode_to_their_pixels() {
    let (lines, out) = image_tool("decode", &images(), None);
    assert_eq!(
        lines,
        ["quadrants.png: 16x16", "quadrants.jpg: 16x16", "quadrants.webp: 16x16"],
        "each decoded"
    );
    for (name, tolerance) in [("quadrants.png", 0), ("quadrants.jpg", 6), ("quadrants.webp", 10)] {
        let rgba = std::fs::read(out.join(format!("{name}.rgba"))).expect("pixels");
        assert_eq!(rgba.len(), 16 * 16 * 4, "{name}");
        for y in 0..16 {
            for x in 0..16 {
                let mut want = QUADRANTS[(y / 8) * 2 + x / 8];
                if name.ends_with(".jpg") {
                    // A JPEG has no alpha: it was made from the opaque pixels.
                    want[3] = 255;
                }
                let at = (y * 16 + x) * 4;
                // A lossy codec's colours within its tolerance, away from the quadrants' edges,
                // where its chroma is subsampled and smoothed.
                let inner = (2..6).contains(&(x % 8)) && (2..6).contains(&(y % 8));
                for c in 0..4 {
                    let got = i32::from(rgba[at + c]);
                    // Alpha is lossless in every one of them, edges too.
                    let tol = if c == 3 { 0 } else { tolerance };
                    if c != 3 && tolerance > 0 && !inner {
                        continue;
                    }
                    assert!(
                        (got - i32::from(want[c])).abs() <= tol,
                        "{name} ({x}, {y}) channel {c}: {got}, not {} within {tol}",
                        want[c]
                    );
                }
            }
        }
    }
}

/// A pattern of `w`×`h` RGBA8: smooth ramps, a sharp edge, and alpha that varies, some of it 0.
fn pattern(w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let edge = if x * 3 > w * 2 { 200 } else { 10 };
            out.extend_from_slice(&[
                ((x * 255) / (w - 1)) as u8,
                ((y * 255) / (h - 1)) as u8,
                edge,
                (((x + y) * 37) % 256) as u8,
            ]);
        }
    }
    out
}

/// Lanczos3, from its formula: `sinc(x) sinc(x / 3)` within 3 of 0.
fn lanczos3(x: f64) -> f64 {
    let sinc = |x: f64| {
        if x == 0.0 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) }
    };
    if x.abs() < 3.0 { sinc(x) * sinc(x / 3.0) } else { 0.0 }
}

/// The weight of each source pixel along an axis for output pixel `o`, resampling `from` to
/// `to`: the kernel at each source pixel's centre's distance from `o`'s, in source pixels,
/// scaled by the shrink (if it shrinks), normalized to sum to 1 over the pixels within its
/// support. Pillow's reach, too: the pixels from `centre - support` to `centre + support`,
/// rounded to the nearest.
fn axis(from: usize, to: usize, o: usize) -> Vec<(usize, f64)> {
    let scale = from as f64 / to as f64;
    let widen = scale.max(1.0);
    let centre = (o as f64 + 0.5) * scale;
    let support = 3.0 * widen;
    let lo = ((centre - support + 0.5).trunc() as i64).max(0) as usize;
    let hi = ((centre + support + 0.5).trunc() as i64).min(from as i64) as usize;
    let raw: Vec<(usize, f64)> =
        (lo..hi).map(|i| (i, lanczos3((i as f64 + 0.5 - centre) / widen))).collect();
    let sum: f64 = raw.iter().map(|(_, w)| w).sum();
    raw.into_iter().map(|(i, w)| (i, w / sum)).collect()
}

/// `rgba` (`w`×`h`) resized to `tw`×`th` by the 2-D sum, over premultiplied alpha.
fn reference(rgba: &[u8], w: usize, h: usize, tw: usize, th: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(tw * th * 4);
    for oy in 0..th {
        let ys = axis(h, th, oy);
        for ox in 0..tw {
            let xs = axis(w, tw, ox);
            let mut sum = [0.0f64; 4];
            for &(y, wy) in &ys {
                for &(x, wx) in &xs {
                    let p = &rgba[(y * w + x) * 4..(y * w + x) * 4 + 4];
                    let a = f64::from(p[3]);
                    for c in 0..3 {
                        sum[c] += wx * wy * f64::from(p[c]) * a / 255.0;
                    }
                    sum[3] += wx * wy * a;
                }
            }
            let byte = |v: f64| v.round().clamp(0.0, 255.0) as u8;
            if sum[3] <= 0.0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                let un = 255.0 / sum[3];
                out.extend_from_slice(&[
                    byte(sum[0] * un),
                    byte(sum[1] * un),
                    byte(sum[2] * un),
                    byte(sum[3]),
                ]);
            }
        }
    }
    out
}

#[test]
fn a_lanczos_resize_matches_the_formula_within_one() {
    let input = scratch("image-resize-in");
    let source = pattern(37, 23);
    std::fs::write(input.join("pattern.rgba"), &source).expect("the pattern");
    let (_, out) = image_tool("resize", &input, None);
    for (name, (tw, th)) in [("down", (16, 11)), ("up", (50, 40))] {
        let got = std::fs::read(out.join(format!("{name}.rgba"))).expect("resized");
        let want = reference(&source, 37, 23, tw, th);
        assert_eq!(got.len(), want.len(), "{name}");
        let mut worst = 0;
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            let d = (i32::from(*g) - i32::from(*w)).abs();
            // A colour whose alpha rounds to 0 or 1 is all rounding: alpha itself is checked.
            let alpha = want[i / 4 * 4 + 3];
            if i % 4 != 3 && alpha < 2 {
                continue;
            }
            worst = worst.max(d);
        }
        assert!(worst <= 1, "{name}: a channel differs by {worst} from the formula's");
    }
}

/// The peak signal-to-noise ratio of `got` against `want`, in dB, over the colour channels.
fn psnr(got: &[u8], want: &[u8]) -> f64 {
    assert_eq!(got.len(), want.len());
    let (mut sum, mut n) = (0.0, 0.0);
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        if i % 4 == 3 {
            continue;
        }
        let d = f64::from(*g) - f64::from(*w);
        sum += d * d;
        n += 1.0;
    }
    let mse = sum / n;
    if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0 * 255.0 / mse).log10() }
}

#[test]
fn an_encode_decodes_back_a_png_exactly_and_a_webp_within_its_psnr() {
    let input = scratch("image-round-in");
    // Opaque, so the colours under no alpha count.
    let mut source = pattern(37, 23);
    for p in source.chunks_exact_mut(4) {
        p[3] = 255;
    }
    std::fs::write(input.join("pattern.rgba"), &source).expect("the pattern");
    let (_, out) = image_tool("round", &input, None);
    let png = std::fs::read(out.join("pattern.png")).expect("the PNG");
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    let (w, h, decoded) =
        wrela_host::image::png_of(&png, Path::new("pattern.png")).expect("a PNG another decodes");
    assert_eq!((w, h), (37, 23));
    assert_eq!(decoded, source, "the PNG's pixels are the pattern's");
    assert_eq!(std::fs::read(out.join("png.rgba")).expect("back"), source, "and decode back");
    let (a, b) = (
        std::fs::read(out.join("a.webp")).expect("a"),
        std::fs::read(out.join("b.webp")).expect("b"),
    );
    assert!(a.starts_with(b"RIFF") && &a[8..12] == b"WEBP");
    assert_eq!(a, b, "the same inputs give the same bytes");
    assert!(!a.windows(4).any(|w| w == b"ALPH"), "an opaque WebP has no alpha");
    let back = std::fs::read(out.join("webp.rgba")).expect("back");
    // A small pattern with a sharp edge in its blue, whose chroma WebP halves each way: 28.8 dB
    // when this was written.
    let db = psnr(&back, &source);
    assert!(db >= 25.0, "a WebP at quality 90 decodes back at {db:.1} dB");
}

#[test]
fn a_1920x1080_frame_goes_through_blobs_into_a_cover() {
    let input = scratch("image-frame-in");
    let frame = pattern(1920, 1080);
    let mut opaque = frame.clone();
    for p in opaque.chunks_exact_mut(4) {
        p[3] = 255;
    }
    std::fs::write(input.join("frame.rgba"), &opaque).expect("the frame");
    let (lines, out) = image_tool("frame", &input, None);
    assert_eq!(lines, ["cover 960x540, back 960x540, frame 1920x1080"]);
    let back = std::fs::read(out.join("frame-back.rgba")).expect("the frame back");
    assert!(back == opaque, "8 MiB through a PNG and back, exactly");
    let cover = std::fs::read(out.join("cover.rgba")).expect("the cover");
    assert_eq!(cover.len(), 960 * 540 * 4);
    let webp = std::fs::read(out.join("cover.webp")).expect("its WebP");
    assert!(webp.len() < cover.len() / 4, "the WebP is {} bytes", webp.len());
    let db = psnr(&std::fs::read(out.join("cover-back.rgba")).expect("decoded"), &cover);
    assert!(db >= 30.0, "the cover's WebP decodes back at {db:.1} dB");
}

#[test]
fn an_image_past_the_blob_budget_is_refused_from_its_header() {
    // A PNG's signature and header for 8192×8192 RGBA, and no pixels: decoding it would fail
    // otherwise, so the refusal is the budget's, before anything is decoded.
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = b"IHDR".to_vec();
    ihdr.extend_from_slice(&8192u32.to_be_bytes());
    ihdr.extend_from_slice(&8192u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    png.extend_from_slice(&13u32.to_be_bytes());
    png.extend_from_slice(&ihdr);
    png.extend_from_slice(&crc32(&ihdr).to_be_bytes());
    png.extend_from_slice(&0u32.to_be_bytes());
    png.extend_from_slice(b"IEND");
    png.extend_from_slice(&crc32(b"IEND").to_be_bytes());
    let input = scratch("image-huge-in");
    std::fs::write(input.join("huge.png"), &png).expect("the header");
    let (lines, _) = image_tool("huge", &input, Some(BlobBudget { bytes: 64 << 20, handles: 16 }));
    assert_eq!(lines.len(), 1);
    // Its RGBA (256 MiB) and the decoder's buffers besides: past 64 MiB.
    assert!(lines[0].starts_with("refused: `decode` needs "), "{lines:?}");
    assert!(lines[0].contains("budget of 67108864"), "{lines:?}");
}

/// PNG's CRC-32 of `bytes`.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[test]
fn a_codec_that_ignores_its_cancel_ends_the_run_as_the_hosts_failure() {
    // The resize's work holds 30 s first, seeing no cancel; the tool is interrupted once the
    // work has started, and the work has a grace of 0.2 s.
    let build = tool_build("image-tool-stall", IMAGE_TOOL);
    let input = scratch("image-stall-in");
    let grants = Grants::default().root("in", &input, false).root("out", &input, true);
    let (lines, mut options) = printer(|_| {});
    options.caps = Caps::open(&grants, |_| None).expect("granted");
    options.args = vec!["stall".into()];
    let (signal, told) = signals();
    let grace = std::time::Duration::from_millis(200);
    options.testing = Testing {
        grace,
        image_stall: std::time::Duration::from_secs(30),
        signal: Some(signal),
        ..Testing::default()
    };
    let interrupt = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    options.interrupt = Some(interrupt.clone());
    let running = std::thread::spawn(move || wrela_host::tool::run(&build, options));
    reached(&told, "image");
    interrupt.store(true, std::sync::atomic::Ordering::Release);
    let interrupted = std::time::Instant::now();
    let ended = running.join().expect("the run");
    // The worker gives up within half the grace, and the run waits the grace for its thread.
    let took = interrupted.elapsed();
    assert!(took < 2 * grace + std::time::Duration::from_secs(1), "{took:?}");
    let End::Host(why) = &ended.end else { panic!("{:?} {lines:?}", ended.end) };
    assert!(why.contains("still running") && why.contains("didn't stop within"), "{why}");
    assert_eq!(ended.end.status(), 71);
    assert_eq!(ended.workers_left, 1, "the codec's thread is counted");
}
