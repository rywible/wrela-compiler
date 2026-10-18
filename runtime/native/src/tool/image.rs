//! `std::tool::image` (language.md §15.5): images decoded, resized and encoded on the CPU, beside
//! the tool. PNG, JPEG and WebP decode to RGBA8 (rows top to bottom); `resize` is Lanczos3, as
//! Pillow's `Image.Resampling.LANCZOS` computes its weights, over premultiplied alpha as Pillow's
//! RGBA resize is; PNG encodes losslessly, and WebP lossily through libwebp, with the quality and
//! method the request gives. The same inputs give the same bytes: no codec here uses threads or
//! SIMD paths that differ by machine, except libwebp's, which are bit-exact by its design.
//!
//! **Bytes are reserved before they're held.** The host reads each request's sizes first (an
//! image's header, a buffer's dimensions), then reserves the answer's blob and the work's own
//! buffers in the blob budget in one step, on the worker, before anything is decoded or
//! allocated: an image that doesn't fit fails then, unread. The output is in that blob; what the
//! work didn't use of the reservation is let go. The blob is the request's until the tool reads
//! the answer, so a failed request gives it back.
//!
//! **The work stops with its request.** It runs on a thread of its own, and the heavy worker that
//! waits for it looks for the request's deadline (from when the host took it) and the tool's end
//! every few milliseconds. Then it cancels the work, which a resize sees between rows and a codec
//! between its phases. If the work hasn't stopped within the grace after that (a codec's own call
//! doesn't stop), the run is the host's failure (status 71), as an HTTP client that won't return
//! is.

use super::services::{Answer, Body, Data, Services, Task, Work};
use super::toolchain::Out;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// A request's deadline unless it asks for another.
const DEADLINE: Duration = Duration::from_secs(60);

/// How often the worker looks at the work it waits for.
const POLL: Duration = Duration::from_millis(10);

/// The most pixels an image has each way (libwebp's own most is one fewer).
pub(crate) const MOST_SIDE: u32 = 16384;

/// What the work is cancelled with: past its deadline, or the tool ended.
const CANCELLED: &str = "the image's work was cancelled";

/// An image request, read.
enum Op {
    Decode(Data),
    Resize { rgba: Data, size: (u32, u32), to: (u32, u32) },
    Png { rgba: Data, size: (u32, u32) },
    Webp { rgba: Data, size: (u32, u32), quality: f32, method: u8 },
}

impl Op {
    fn name(&self) -> &'static str {
        match self {
            Op::Decode(_) => "decode",
            Op::Resize { .. } => "resize",
            Op::Png { .. } => "encode_png",
            Op::Webp { .. } => "encode_webp",
        }
    }
}

/// Whether `w`×`h` is a size an image may have.
fn side(w: u32, h: u32) -> Result<(), String> {
    if w == 0 || h == 0 || w > MOST_SIDE || h > MOST_SIDE {
        return Err(format!("an image of {w}×{h} isn't 1 to {MOST_SIDE} pixels each way"));
    }
    Ok(())
}

/// How request `name` of the image service, with `b`, is carried out.
pub(crate) fn route(name: &str, b: &mut Body) -> Work {
    let entered = Instant::now();
    let parsed = (|| {
        let op = match name {
            "decode" => Op::Decode(b.data()?),
            "resize" => {
                let rgba = b.data()?;
                let size = (b.u32()?, b.u32()?);
                let to = (b.u32()?, b.u32()?);
                side(to.0, to.1)?;
                Op::Resize { rgba, size, to }
            }
            "encode_png" => {
                let rgba = b.data()?;
                Op::Png { rgba, size: (b.u32()?, b.u32()?) }
            }
            "encode_webp" => {
                let rgba = b.data()?;
                let size = (b.u32()?, b.u32()?);
                let quality = f64::from_bits(b.u64()?);
                let method = b.u8()?;
                if !(0.0..=100.0).contains(&quality) {
                    return Err(format!("a WebP's quality is 0 to 100, not {quality}"));
                }
                if method > 6 {
                    return Err(format!("a WebP's method is 0 to 6, not {method}"));
                }
                if size.0 >= MOST_SIDE || size.1 >= MOST_SIDE {
                    return Err(format!("a WebP is at most {} pixels each way", MOST_SIDE - 1));
                }
                Op::Webp { rgba, size, quality: quality as f32, method }
            }
            _ => return Err(format!("the image service has no `{name}`")),
        };
        let deadline = super::http::deadline(f64::from_bits(b.u64()?), DEADLINE)?;
        b.end()?;
        Ok((op, deadline))
    })();
    match parsed {
        Ok((op, deadline)) => {
            let task: Task = Box::new(move |s, request| {
                super::services::answer(carry_out(s, request, op, entered + deadline, deadline))
            });
            Work::Heavy(task)
        }
        Err(why) => Work::Now(Answer::Bytes([&[1u8][..], why.as_bytes()].concat())),
    }
}

/// The bytes `data` names: its own, or its blob's, shared (while they're held, the blob can't be
/// appended to or released, as while a file is written from it).
fn bytes(s: &Services, data: Data) -> Result<Arc<Vec<u8>>, String> {
    match data {
        Data::Bytes(b) => Ok(Arc::new(b)),
        Data::Blob(id) => s.blobs().get(id),
    }
}

/// `rgba`'s length checked against `w`×`h` pixels of RGBA8.
fn pixels(rgba: &[u8], (w, h): (u32, u32)) -> Result<(), String> {
    side(w, h)?;
    let want = u64::from(w) * u64::from(h) * 4;
    if rgba.len() as u64 != want {
        return Err(format!("{w}×{h} RGBA8 is {want} bytes, not {}", rgba.len()));
    }
    Ok(())
}

/// The most bytes a PNG of `w`×`h` RGBA8 takes: its rows, each with its filter's byte, stored
/// (deflate's worst, 5 bytes a block of 65535), and its chunks' framing.
fn png_most(w: u32, h: u32) -> u64 {
    let raw = u64::from(h) * (1 + u64::from(w) * 4);
    raw + 5 * (raw / 16383 + 1) + 4096
}

/// The most bytes a lossy WebP of `w`×`h` takes: no more than its pixels' RGBA, and the
/// container's and the codec's own bytes, which a tiny image's are more than its pixels.
fn webp_most(w: u32, h: u32) -> u64 {
    u64::from(w) * u64::from(h) * 4 + 64 * 1024
}

/// The most bytes a resize of `size` to `to` holds: its output, the horizontal pass's rows (four
/// `f32`s a pixel, unless the width stays), and both axes' weights.
fn resize_most(size: (u32, u32), to: (u32, u32)) -> u64 {
    let out = u64::from(to.0) * u64::from(to.1) * 4;
    let rows = if to.0 == size.0 { 0 } else { u64::from(to.0) * u64::from(size.1) * 16 };
    out + rows + weights_most(size.0, to.0) + weights_most(size.1, to.1)
}

/// The most bytes [`weights`] holds resampling `from` pixels to `to`: each output pixel's entry
/// (its first pixel and its `Vec`), its weights (at most `6 × from / to + 2` of them when it
/// shrinks, 8 when it grows, as `f32`s), and one pixel's weights as `f64`s while they're made.
fn weights_most(from: u32, to: u32) -> u64 {
    let (from, to) = (u64::from(from), u64::from(to));
    let taps = 6 * from.max(to) + 2 * to;
    let one = 6 * from.div_ceil(to) + 8;
    to * (std::mem::size_of::<(usize, Vec<f32>)>() as u64) + 4 * taps + 8 * one
}

/// The work the request does, on its own thread: what it gives (a size and bytes), or why not.
type Job = Box<dyn FnOnce(&AtomicBool) -> Result<(u32, u32, Vec<u8>), String> + Send>;

/// Carries out `op` for `request` by `due`: the answer's payload, or why it failed.
fn carry_out(
    s: &Arc<Services>,
    request: u32,
    op: Op,
    due: Instant,
    deadline: Duration,
) -> Result<Vec<u8>, String> {
    let what = op.name();
    if Instant::now() >= due {
        return Err(format!(
            "`{what}` didn't start within its deadline of {} s",
            deadline.as_secs_f64()
        ));
    }
    // The sizes first, then the reservation, then the work: nothing is decoded or allocated
    // that the budget doesn't hold.
    let (reserve, sized, job): (u64, bool, Job) = match op {
        Op::Decode(data) => {
            let input = bytes(s, data)?;
            let layout = header(&input)?;
            side(layout.width, layout.height)?;
            // The RGBA, and the most the decoder holds besides.
            let rgba = u64::from(layout.width) * u64::from(layout.height) * 4;
            (rgba + layout.working, true, Box::new(move |cancel| decode(&input, cancel)))
        }
        Op::Resize { rgba, size, to } => {
            let input = bytes(s, rgba)?;
            pixels(&input, size)?;
            let job: Job = Box::new(move |cancel| {
                let (out, _) = lanczos(&input, size, to, cancel)?;
                Ok((to.0, to.1, out))
            });
            (resize_most(size, to), true, job)
        }
        Op::Png { rgba, size } => {
            let input = bytes(s, rgba)?;
            pixels(&input, size)?;
            let most = png_most(size.0, size.1);
            let job: Job = Box::new(move |cancel| {
                Ok((size.0, size.1, encode_png(&input, size, most, cancel)?))
            });
            // The encoder's buffer, the blob it's shrunk to (`Blobs::fill`), and the
            // compressor's state and a row.
            (2 * most + 4 * u64::from(size.0) + (1 << 20), false, job)
        }
        Op::Webp { rgba, size, quality, method } => {
            let input = bytes(s, rgba)?;
            pixels(&input, size)?;
            let most = webp_most(size.0, size.1);
            let n = input.len() as u64;
            // libwebp's copy of the pixels and its YUV and alpha planes (at most the pixels'
            // bytes again), and the encoded bytes: libwebp's own, then the answer's.
            let job: Job = Box::new(move |cancel| {
                Ok((size.0, size.1, encode_webp(&input, size, quality, method, most, cancel)?))
            });
            (2 * n + 2 * most, false, job)
        }
    };
    let id = s.blobs().create_reserved(request, reserve).map_err(|why| {
        format!("`{what}` needs {reserve} bytes of the blob budget for its pixels: {why}")
    })?;
    let (w, h, out) = apart(s, request, what, due, deadline, job)?;
    s.blobs().fill(id, out)?;
    let mut answer = Out::default();
    if sized {
        answer.u32(w).u32(h);
    }
    answer.u8(1).u32(id);
    Ok(answer.0)
}

/// Runs `job` on a thread of its own, waiting for it until `due`: what it gave, or why it didn't.
/// Past `due`, or when the tool ends, the job is cancelled, and has the grace; if it hasn't
/// returned then, the run is the host's failure.
fn apart(
    s: &Services,
    request: u32,
    what: &str,
    due: Instant,
    deadline: Duration,
    job: Job,
) -> Result<(u32, u32, Vec<u8>), String> {
    let cancel = Arc::new(AtomicBool::new(false));
    let (sent, got) = std::sync::mpsc::channel();
    let flag = cancel.clone();
    let stall = s.image_stall;
    let signal = s.signal.clone();
    let spawned =
        std::thread::Builder::new().name(format!("wrela-image-{request}")).spawn(move || {
            if let Some(signal) = &signal {
                (signal.0)("image");
            }
            // A test's codec that doesn't see its cancel.
            std::thread::sleep(stall);
            let run = std::panic::AssertUnwindSafe(|| job(&flag));
            let result = std::panic::catch_unwind(run)
                .unwrap_or_else(|_| Err("the image's work failed (it panicked)".into()));
            let _ = sent.send(result);
        });
    let thread = spawned.map_err(|e| format!("can't start the image's thread: {e}"))?;
    let (why, grace) = loop {
        match got.recv_timeout(POLL) {
            Ok(result) => {
                let _ = thread.join();
                return result;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let _ = thread.join();
                return Err("the image's thread ended without an answer".into());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if s.closing.load(Ordering::Acquire) {
            // Half the grace: the pool waits for this worker the whole grace.
            break (format!("the tool ended during `{what}`"), s.grace / 2);
        }
        if Instant::now() >= due {
            let why = format!(
                "`{what}` ran past its deadline of {} s, and the host stopped it",
                deadline.as_secs_f64()
            );
            break (why, s.grace);
        }
    };
    cancel.store(true, Ordering::Release);
    if got.recv_timeout(grace).is_ok() {
        let _ = thread.join();
        return Err(why);
    }
    let _ = s.stuck.set(format!(
        "`{what}` for request {request} didn't stop within {grace:?} of being cancelled"
    ));
    // Left to the run's end, which counts it if it still runs.
    s.strays.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(thread);
    Err(why)
}

fn stopped(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) { Err(CANCELLED.into()) } else { Ok(()) }
}

/// An encoded image's format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Png,
    Jpeg,
    Webp,
}

/// An encoded image as its headers give it, before anything is decoded: its format, its size,
/// and the most bytes its decoder holds besides the RGBA it gives (audited from each crate's
/// allocations).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    format: Format,
    width: u32,
    height: u32,
    working: u64,
}

/// `bytes`' format, by its first bytes, its size, from its headers alone, and its decoder's most.
fn header(bytes: &[u8]) -> Result<Layout, String> {
    let n = bytes.len() as u64;
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        // Its first chunk, `IHDR`, read here: nothing of the rest is.
        let Some(h) = bytes.get(8..24).filter(|h| h[..8] == [0, 0, 0, 13, b'I', b'H', b'D', b'R'])
        else {
            return Err("the PNG doesn't start with its header (`IHDR`)".into());
        };
        let be = |at: usize| u32::from_be_bytes(h[at..at + 4].try_into().expect("4"));
        let (width, height) = (be(8), be(12));
        let (w, hh) = (u64::from(width), u64::from(height));
        // png 0.18: its output buffer (8-bit, at most four channels), its rows being unfiltered
        // (each at most 8 bytes a pixel and a filter byte: the previous row, the current one, and
        // the data stream it keeps a few of, with at least 128 KiB), what the decompressor keeps,
        // and its text and colour profile, at most `png_reader`'s 4 MiB.
        let row = 8 * w + 1;
        let working = 4 * w * hh + 16 * row + (512 << 10) + (4 << 20);
        return Ok(Layout { format: Format::Png, width, height, working });
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        let mut d = jpeg_decoder(bytes);
        d.decode_headers().map_err(|e| format!("the JPEG: {e:?}"))?;
        let info = d.info().ok_or("the JPEG has no size")?;
        let (width, height) = (u32::from(info.width), u32::from(info.height));
        let (w, h) = (u64::from(width) + 32, u64::from(height) + 32);
        // zune-jpeg 0.5: a progressive JPEG's coefficients, an `i16` for each of up to four
        // components' samples over its MCUs (each at most 32 pixels each way), its rows'
        // upsampling buffers, and its copies of the file's metadata (at most the file).
        let working = 8 * w * h + 256 * w + n + (1 << 20);
        return Ok(Layout { format: Format::Jpeg, width, height, working });
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        let webp = webp_layout(bytes)?;
        let (w, h) = (u64::from(webp.width), u64::from(webp.height));
        // libwebp 1.3: the RGBA it decodes into, then a lossy image's or a lossless one's own.
        let mut working = 4 * w * h;
        match webp.image {
            WebpImage::Lossless(len) => working += lossless_most(w, h, len),
            WebpImage::Lossy => {
                // `AllocateMemory` (frame_dec.c): for each 16 pixels across, its intra modes,
                // top samples, macroblock info and data, filter info and its rows' cache, all
                // under 5000 bytes; and the alpha plane, decoded at the image's size.
                working += 5000 * w.div_ceil(16) + (64 << 10);
                match webp.alpha {
                    WebpAlpha::None => {}
                    WebpAlpha::Raw => working += w * h,
                    WebpAlpha::Lossless(len) => working += w * h + lossless_most(w, h, len),
                }
            }
        }
        return Ok(Layout {
            format: Format::Webp,
            width: webp.width,
            height: webp.height,
            working,
        });
    }
    Err("the bytes aren't a PNG, a JPEG or a WebP".into())
}

/// The most bytes libwebp 1.3's lossless decoder (vp8l_dec.c) holds for a `w`×`h` image coded in
/// `len` bytes: its pixels and cache rows (`AllocateInternalBuffers32b`, four bytes each); its
/// Huffman groups, each five tables of at most 2300 + 2704 four-byte codes (`kTableSize`) and
/// its group (under 1 KiB); the transforms' and the Huffman image's subsampled pixels (at most a
/// quarter each way, five of them), each with one group; the group mapping (65536 `int`s); and
/// its code lengths. Groups are at most 1000 and the image's pixels without a mapping, and with
/// one the distinct groups the Huffman image names: at most its pixels, 65536, and its literals'
/// bits (`8 × len`: distinct values each take at least a bit).
fn lossless_most(w: u64, h: u64, len: u64) -> u64 {
    const GROUP: u64 = 4 * (2300 + 2704) + 1024;
    let sub = w.div_ceil(4) * h.div_ceil(4);
    let groups = (w * h).min(1000) + sub.min(65536).min(8 * len);
    4 * (w * h + 17 * w) + GROUP * (groups + 5) + 5 * 4 * sub + 4 * 65536 + (1 << 20)
}

/// What a WebP's chunks say, read before it's decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Webp {
    width: u32,
    height: u32,
    image: WebpImage,
    alpha: WebpAlpha,
}

/// A WebP's image: lossy (`VP8 `), or lossless (`VP8L`) in this many bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WebpImage {
    Lossy,
    Lossless(u64),
}

/// A lossy WebP's alpha (`ALPH`): none, its bytes as they are, or lossless in this many bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WebpAlpha {
    None,
    Raw,
    Lossless(u64),
}

/// The size a `VP8 ` chunk's key frame or a `VP8L` chunk's header gives.
fn webp_frame(lossless: bool, payload: &[u8]) -> Result<(u32, u32), String> {
    if lossless {
        // A signature byte, then the width and height less 1 in 14 bits each, and a version of 0.
        let Some(&[0x2f, a, b, c, d]) = payload.get(..5).and_then(|p| <&[u8; 5]>::try_from(p).ok())
        else {
            return Err("the WebP's lossless image has no header".into());
        };
        let v = u32::from_le_bytes([a, b, c, d]);
        if v >> 29 != 0 {
            return Err("the WebP's lossless image has an unknown version".into());
        }
        return Ok(((v & 0x3fff) + 1, ((v >> 14) & 0x3fff) + 1));
    }
    // A key frame's tag (its first bit 0), its start code, then the width and height in 14 bits.
    match payload.get(..10) {
        Some(p) if p[0] & 1 == 0 && p[3..6] == [0x9d, 0x01, 0x2a] => {
            let le = |at: usize| u32::from(u16::from_le_bytes([p[at], p[at + 1]]) & 0x3fff);
            Ok((le(6), le(8)))
        }
        _ => Err("the WebP's lossy image isn't a key frame".into()),
    }
}

/// Reads a WebP's chunks: each within the file and the RIFF's length; a simple WebP's one image,
/// or an extended one's canvas and its one image of the canvas's size, and its alpha, if it's
/// lossy and has alpha (raw alpha holds a byte for each pixel). An animation is refused, as is
/// an image of another size than its canvas, so the decoder never works on a size the canvas
/// doesn't give.
fn webp_layout(bytes: &[u8]) -> Result<Webp, String> {
    let bad = |why: &str| Err(format!("the WebP {why}"));
    let riff = u32::from_le_bytes(bytes[4..8].try_into().expect("4")) as usize;
    let Some(end) = riff.checked_add(8).filter(|&e| e <= bytes.len() && e >= 12) else {
        return bad("is shorter than its RIFF header says");
    };
    let mut chunks = Vec::new();
    let mut at = 12;
    while at < end {
        let Some(head) = bytes.get(at..at + 8).filter(|_| at + 8 <= end) else {
            return bad("has a chunk cut short");
        };
        let len = u32::from_le_bytes(head[4..8].try_into().expect("4")) as usize;
        let start = at + 8;
        let Some(stop) = start.checked_add(len).filter(|&s| s <= end) else {
            return bad("has a chunk that reaches past its end");
        };
        chunks.push((<[u8; 4]>::try_from(&head[..4]).expect("4"), &bytes[start..stop]));
        at = stop + (len & 1);
    }
    let image = |name: &[u8; 4], payload: &[u8]| -> Result<(WebpImage, (u32, u32)), String> {
        let lossless = name == b"VP8L";
        let kind =
            if lossless { WebpImage::Lossless(payload.len() as u64) } else { WebpImage::Lossy };
        Ok((kind, webp_frame(lossless, payload)?))
    };
    let Some(&(first, payload)) = chunks.first() else { return bad("has no chunks") };
    match &first {
        b"VP8 " | b"VP8L" => {
            let (kind, (width, height)) = image(&first, payload)?;
            Ok(Webp { width, height, image: kind, alpha: WebpAlpha::None })
        }
        b"VP8X" => {
            let Some(x) = payload.get(..10) else { return bad("has a short VP8X header") };
            if x[0] & 0x02 != 0 {
                return bad("is animated: only a still image is decoded");
            }
            let le24 = |at: usize| u32::from_le_bytes([x[at], x[at + 1], x[at + 2], 0]) + 1;
            let canvas = (le24(4), le24(7));
            let mut found: Option<(WebpImage, (u32, u32))> = None;
            let mut alpha = WebpAlpha::None;
            for &(name, payload) in &chunks[1..] {
                match &name {
                    b"ANIM" | b"ANMF" => {
                        return bad("is animated: only a still image is decoded");
                    }
                    b"VP8 " | b"VP8L" if found.is_some() => return bad("has two images"),
                    b"VP8 " | b"VP8L" => found = Some(image(&name, payload)?),
                    b"ALPH" if found.is_some() => return bad("has its alpha after its image"),
                    b"ALPH" if alpha != WebpAlpha::None => return bad("has two alphas"),
                    b"ALPH" => {
                        let Some(&info) = payload.first() else { return bad("has an empty ALPH") };
                        alpha = match info & 0x03 {
                            0 => {
                                let pixels = u64::from(canvas.0) * u64::from(canvas.1);
                                if ((payload.len() - 1) as u64) < pixels {
                                    return bad("has fewer alpha bytes than pixels");
                                }
                                WebpAlpha::Raw
                            }
                            1 => WebpAlpha::Lossless(payload.len() as u64),
                            _ => return bad("has an unknown alpha compression"),
                        };
                    }
                    _ => {}
                }
            }
            let Some((kind, size)) = found else { return bad("has no image") };
            if size != canvas {
                return Err(format!(
                    "the WebP's image is {}×{}, not its canvas's {}×{}",
                    size.0, size.1, canvas.0, canvas.1
                ));
            }
            // A lossless image keeps its own alpha: an `ALPH` beside it is ignored.
            let alpha = if kind == WebpImage::Lossy { alpha } else { WebpAlpha::None };
            Ok(Webp { width: canvas.0, height: canvas.1, image: kind, alpha })
        }
        _ => bad("doesn't start with an image or a VP8X header"),
    }
}

fn png_reader(bytes: &[u8]) -> Result<png::Reader<std::io::Cursor<&[u8]>>, String> {
    // Its text and colour profile in at most 4 MiB: the header is read before any reservation.
    let limits = png::Limits { bytes: 4 << 20 };
    let mut decoder = png::Decoder::new_with_limits(std::io::Cursor::new(bytes), limits);
    // Indexed, grey and 16-bit images come out as 8-bit grey, grey and alpha, RGB or RGBA.
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    decoder.read_info().map_err(|e| format!("the PNG: {e}"))
}

fn jpeg_decoder(bytes: &[u8]) -> zune_jpeg::JpegDecoder<std::io::Cursor<&[u8]>> {
    let options = zune_core::options::DecoderOptions::default()
        .jpeg_set_out_colorspace(zune_core::colorspace::ColorSpace::RGBA)
        .set_max_width(MOST_SIDE as usize)
        .set_max_height(MOST_SIDE as usize);
    zune_jpeg::JpegDecoder::new_with_options(std::io::Cursor::new(bytes), options)
}

/// `channels` bytes a pixel, as RGBA8: grey, grey and alpha, RGB or RGBA, made opaque where they
/// have no alpha.
fn to_rgba(pixels: &[u8], channels: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixels.len() / channels * 4);
    for p in pixels.chunks_exact(channels) {
        match channels {
            1 => out.extend_from_slice(&[p[0], p[0], p[0], 255]),
            2 => out.extend_from_slice(&[p[0], p[0], p[0], p[1]]),
            3 => out.extend_from_slice(&[p[0], p[1], p[2], 255]),
            _ => out.extend_from_slice(p),
        }
    }
    out
}

/// Decodes `bytes` (a PNG, its first frame if it has several, a JPEG or a still WebP) as RGBA8.
fn decode(bytes: &[u8], cancel: &AtomicBool) -> Result<(u32, u32, Vec<u8>), String> {
    let Layout { format, width: w, height: h, .. } = header(bytes)?;
    stopped(cancel)?;
    let rgba = match format {
        Format::Png => {
            let mut reader = png_reader(bytes)?;
            let size = reader.output_buffer_size().ok_or("the PNG is too large")?;
            let mut out = vec![0; size];
            let frame = reader.next_frame(&mut out).map_err(|e| format!("the PNG: {e}"))?;
            out.truncate(frame.buffer_size());
            stopped(cancel)?;
            let channels = match frame.color_type {
                png::ColorType::Grayscale => 1,
                png::ColorType::GrayscaleAlpha => 2,
                png::ColorType::Rgb => 3,
                png::ColorType::Rgba => 4,
                other => return Err(format!("the PNG's pixels are {other:?}")),
            };
            if channels == 4 { out } else { to_rgba(&out, channels) }
        }
        Format::Jpeg => {
            let mut d = jpeg_decoder(bytes);
            d.decode().map_err(|e| format!("the JPEG: {e:?}"))?
        }
        Format::Webp => {
            let image =
                webp::Decoder::new(bytes).decode().ok_or("libwebp couldn't decode the WebP")?;
            stopped(cancel)?;
            to_rgba(&image, if image.is_alpha() { 4 } else { 3 })
        }
    };
    if rgba.len() as u64 != u64::from(w) * u64::from(h) * 4 {
        return Err(format!("the image decoded to {} bytes, not {w}×{h} RGBA8", rgba.len()));
    }
    Ok((w, h, rgba))
}

/// Lanczos3, as Pillow's: `sinc(x) sinc(x / 3)` for `|x| < 3`.
pub(crate) fn lanczos3(x: f64) -> f64 {
    fn sinc(x: f64) -> f64 {
        if x == 0.0 {
            return 1.0;
        }
        let x = x * std::f64::consts::PI;
        x.sin() / x
    }
    if (-3.0..3.0).contains(&x) { sinc(x) * sinc(x / 3.0) } else { 0.0 }
}

/// Each output pixel's first input pixel and weights, resampling `from` pixels to `to`, as
/// Pillow's `precompute_coeffs` computes them: a pixel's centre is at `(x + 0.5) × scale`, the
/// filter is widened by the scale when it shrinks, and the weights sum to 1.
pub(crate) fn weights(from: u32, to: u32) -> Vec<(usize, Vec<f32>)> {
    let scale = f64::from(from) / f64::from(to);
    let filterscale = scale.max(1.0);
    let support = 3.0 * filterscale;
    (0..to)
        .map(|x| {
            let center = (f64::from(x) + 0.5) * scale;
            // C's `(int)`: towards zero.
            let lo = ((center - support + 0.5) as i64).max(0) as usize;
            let hi = ((center + support + 0.5) as i64).min(i64::from(from)) as usize;
            let mut w: Vec<f64> =
                (lo..hi).map(|i| lanczos3((i as f64 - center + 0.5) / filterscale)).collect();
            let sum: f64 = w.iter().sum();
            if sum != 0.0 {
                for k in &mut w {
                    *k /= sum;
                }
            }
            (lo, w.into_iter().map(|k| k as f32).collect())
        })
        .collect()
}

/// `rgba` (`w`×`h`) resized to `to` with Lanczos3: horizontally, then vertically, each pass
/// skipped where the size stays, over premultiplied alpha, rounded to 8 bits once at the end. A
/// resize that keeps the width reads its vertical pass from `rgba` itself, with no rows between.
/// The output, and the bytes the work held besides (its rows and weights), as [`resize_most`]
/// reserves them.
fn lanczos(
    rgba: &[u8],
    (w, h): (u32, u32),
    to: (u32, u32),
    cancel: &AtomicBool,
) -> Result<(Vec<u8>, u64), String> {
    let (w, h) = (w as usize, h as usize);
    let (tw, th) = (to.0 as usize, to.1 as usize);
    // Premultiplied, as Pillow's RGBA resize is (through "RGBa").
    let premultiplied = |p: &[u8]| -> [f32; 4] {
        let a = f32::from(p[3]) / 255.0;
        [f32::from(p[0]) * a, f32::from(p[1]) * a, f32::from(p[2]) * a, f32::from(p[3])]
    };
    let held = |ws: &[(usize, Vec<f32>)]| -> u64 {
        let entry = std::mem::size_of::<(usize, Vec<f32>)>() as u64;
        ws.iter().map(|(_, k)| entry + 4 * k.capacity() as u64).sum()
    };
    let mut scratch = 0;
    let mut rows: Vec<[f32; 4]> = Vec::new();
    if tw != w {
        rows.reserve_exact(tw * h);
        let across = weights(w as u32, to.0);
        scratch += held(&across);
        for y in 0..h {
            stopped(cancel)?;
            let row = &rgba[y * w * 4..(y + 1) * w * 4];
            for (lo, ks) in &across {
                let mut sum = [0.0f32; 4];
                for (k, wt) in ks.iter().enumerate() {
                    let p = premultiplied(&row[(lo + k) * 4..(lo + k) * 4 + 4]);
                    for c in 0..4 {
                        sum[c] += p[c] * wt;
                    }
                }
                rows.push(sum);
            }
        }
        scratch += 16 * rows.capacity() as u64;
    }
    let at = |x: usize, y: usize| -> [f32; 4] {
        if tw == w {
            premultiplied(&rgba[(y * w + x) * 4..(y * w + x) * 4 + 4])
        } else {
            rows[y * tw + x]
        }
    };
    let down = (th != h).then(|| weights(h as u32, to.1));
    scratch += down.as_deref().map_or(0, held);
    let mut out = Vec::with_capacity(tw * th * 4);
    for y in 0..th {
        stopped(cancel)?;
        for x in 0..tw {
            let p = match &down {
                None => at(x, y),
                Some(down) => {
                    let (lo, ks) = &down[y];
                    let mut sum = [0.0f32; 4];
                    for (k, wt) in ks.iter().enumerate() {
                        let q = at(x, lo + k);
                        for c in 0..4 {
                            sum[c] += q[c] * wt;
                        }
                    }
                    sum
                }
            };
            let byte = |v: f32| v.round().clamp(0.0, 255.0) as u8;
            let a = p[3];
            if a <= 0.0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            let un = 255.0 / a;
            out.extend_from_slice(&[byte(p[0] * un), byte(p[1] * un), byte(p[2] * un), byte(a)]);
        }
    }
    Ok((out, scratch))
}

/// A `Vec` written to no further than its capacity: past it, writing fails, so an encoder can't
/// take more than was reserved.
struct Capped(Vec<u8>);

impl std::io::Write for Capped {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.0.len() + buf.len() > self.0.capacity() {
            return Err(std::io::Error::other("more bytes than were reserved"));
        }
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `rgba` (`w`×`h`) as a PNG of RGBA8, in at most `most` bytes.
fn encode_png(
    rgba: &[u8],
    (w, h): (u32, u32),
    most: u64,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let fail = |e: png::EncodingError| format!("the PNG: {e}");
    let mut out = Capped(Vec::with_capacity(most as usize));
    {
        let mut encoder = png::Encoder::new(&mut out, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(fail)?;
        let mut stream = writer.stream_writer().map_err(fail)?;
        // A band of rows at a time, so a cancel is seen between them.
        let band = (w as usize) * 4 * 64;
        for chunk in rgba.chunks(band) {
            stopped(cancel)?;
            std::io::Write::write_all(&mut stream, chunk).map_err(|e| format!("the PNG: {e}"))?;
        }
        stream.finish().map_err(fail)?;
    }
    Ok(out.0)
}

/// `rgba` (`w`×`h`) as a lossy WebP at `quality` (0 to 100) and `method` (0, fastest, to 6,
/// smallest), in at most `most` bytes: libwebp's defaults otherwise, its alpha at quality 100, on
/// one thread.
fn encode_webp(
    rgba: &[u8],
    (w, h): (u32, u32),
    quality: f32,
    method: u8,
    most: u64,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let mut config =
        webp::WebPConfig::new().map_err(|()| "libwebp's configuration didn't start".to_string())?;
    config.lossless = 0;
    config.quality = quality;
    config.method = i32::from(method);
    config.alpha_quality = 100;
    config.thread_level = 0;
    stopped(cancel)?;
    let encoded = webp::Encoder::from_rgba(rgba, w, h)
        .encode_advanced(&config)
        .map_err(|e| format!("libwebp: {e:?}"))?;
    stopped(cancel)?;
    if encoded.len() as u64 > most {
        return Err(format!("the WebP is {} bytes, more than the {most} reserved", encoded.len()));
    }
    Ok(encoded.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_sum_to_one_and_reach_as_far_as_pillows() {
        // Halving: each output pixel reaches 6 input pixels each way, its centre between two.
        let half = weights(8, 4);
        assert_eq!(half.len(), 4);
        for (lo, ks) in &half {
            let sum: f32 = ks.iter().sum();
            assert!((sum - 1.0).abs() < 1e-6, "{sum}");
            assert!(lo + ks.len() <= 8);
        }
        assert_eq!(half[0].0, 0, "clamped at the first pixel");
        // The same size: the identity.
        for (x, (lo, ks)) in weights(5, 5).iter().enumerate() {
            let at = ks.iter().position(|&k| (k - 1.0).abs() < 1e-6).expect("one weight of 1");
            assert_eq!(lo + at, x);
        }
        assert_eq!(lanczos3(3.0), 0.0);
        assert_eq!(lanczos3(0.0), 1.0);
    }

    #[test]
    fn a_header_gives_the_format_and_size_and_a_cancel_stops_a_decode() {
        let mut png = Vec::new();
        {
            let mut e = png::Encoder::new(&mut png, 3, 2);
            e.set_color(png::ColorType::Rgb);
            e.write_header().expect("a header").write_image_data(&[7; 18]).expect("pixels");
        }
        let layout = header(&png).expect("a header");
        assert_eq!((layout.format, layout.width, layout.height), (Format::Png, 3, 2));
        assert!(header(b"GIF89a").unwrap_err().contains("aren't"));
        let (w, h, rgba) = decode(&png, &AtomicBool::new(false)).expect("decoded");
        assert_eq!((w, h), (3, 2));
        assert_eq!(&rgba[..4], &[7, 7, 7, 255]);
        assert!(decode(&png, &AtomicBool::new(true)).unwrap_err().contains("cancelled"));
    }

    /// A lossy WebP of `w`×`h` from libwebp, as a simple file (`VP8 ` alone) if it's opaque.
    fn webp_of(rgba: &[u8], w: u32, h: u32, quality: f32, method: u8) -> Vec<u8> {
        let cancel = AtomicBool::new(false);
        encode_webp(rgba, (w, h), quality, method, webp_most(w, h), &cancel).expect("encoded")
    }

    /// The payload of the chunk `name` in the WebP `file`.
    fn chunk(file: &[u8], name: &[u8; 4]) -> Vec<u8> {
        let mut at = 12;
        while at + 8 <= file.len() {
            let len = u32::from_le_bytes(file[at + 4..at + 8].try_into().expect("4")) as usize;
            if &file[at..at + 4] == name {
                return file[at + 8..at + 8 + len].to_vec();
            }
            at += 8 + len + (len & 1);
        }
        panic!("no {name:?} chunk")
    }

    /// A RIFF file of `chunks`.
    fn riff(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut body = b"WEBP".to_vec();
        for (name, payload) in chunks {
            body.extend_from_slice(*name);
            body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            body.extend_from_slice(payload);
            if payload.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// A VP8X header for a `w`×`h` canvas with `flags`.
    fn vp8x(flags: u8, w: u32, h: u32) -> Vec<u8> {
        let mut x = vec![flags, 0, 0, 0];
        x.extend_from_slice(&(w - 1).to_le_bytes()[..3]);
        x.extend_from_slice(&(h - 1).to_le_bytes()[..3]);
        x
    }

    #[test]
    fn a_webp_whose_image_isnt_its_canvas_is_refused_before_anything_is_reserved() {
        // A real 16×16 lossy frame, its header saying 16383×16383, under a 1×1 canvas: a decoder
        // that trusts the frame would allocate its planes from it.
        let encoded = webp_of(&[90; 16 * 16 * 4], 16, 16, 75.0, 0);
        let real = chunk(&encoded, b"VP8 ");
        let mut frame = real.clone();
        frame[6..8].copy_from_slice(&16383u16.to_le_bytes());
        frame[8..10].copy_from_slice(&16383u16.to_le_bytes());
        let bomb = riff(&[(b"VP8X", vp8x(0, 1, 1)), (b"VP8 ", frame.clone())]);
        let refused = header(&bomb).expect_err("refused");
        assert!(refused.contains("not its canvas's 1×1"), "{refused}");
        // Through the service, under a tiny budget: refused from its chunks, nothing reserved.
        let s = Arc::new(super::super::services::for_test(super::super::BlobBudget {
            bytes: 1 << 10,
            handles: 4,
        }));
        let due = Instant::now() + Duration::from_secs(10);
        let op = Op::Decode(Data::Bytes(bomb));
        let why = carry_out(&s, 1, op, due, Duration::from_secs(10)).expect_err("refused");
        assert!(why.contains("canvas"), "{why}");
        assert_eq!(s.blobs().counted(), (0, 0));
        // The same frame alone is what its header says, and its decoder's work is counted.
        let alone = riff(&[(b"VP8 ", frame)]);
        let layout = header(&alone).expect("a simple WebP");
        assert_eq!((layout.width, layout.height), (16383, 16383));
        assert!(layout.working >= 4 * 16383 * 16383);
        // Each chunk within the file, a still image, and raw alpha for every pixel.
        let real = &real[..];
        let mut cut = riff(&[(b"VP8X", vp8x(0, 16, 16)), (b"VP8 ", real.to_vec())]);
        cut.truncate(cut.len() - 4);
        assert!(header(&cut).unwrap_err().contains("shorter"));
        let animated = riff(&[(b"VP8X", vp8x(0x02, 16, 16)), (b"VP8 ", real.to_vec())]);
        assert!(header(&animated).unwrap_err().contains("animated"));
        let anmf = riff(&[(b"VP8X", vp8x(0, 16, 16)), (b"ANMF", vec![0; 16])]);
        assert!(header(&anmf).unwrap_err().contains("animated"));
        let short =
            riff(&[(b"VP8X", vp8x(0x10, 16, 16)), (b"ALPH", vec![0; 9]), (b"VP8 ", real.to_vec())]);
        assert!(header(&short).unwrap_err().contains("alpha bytes"));
        let two =
            riff(&[(b"VP8X", vp8x(0, 16, 16)), (b"VP8 ", real.to_vec()), (b"VP8 ", real.to_vec())]);
        assert!(header(&two).unwrap_err().contains("two images"));
        let fine = riff(&[(b"VP8X", vp8x(0, 16, 16)), (b"VP8 ", real.to_vec())]);
        let (w, h, rgba) = decode(&fine, &AtomicBool::new(false)).expect("decoded");
        assert_eq!((w, h, rgba[3]), (16, 16, 255));
    }

    #[test]
    fn a_resize_reserves_all_it_holds_a_height_only_one_too() {
        let cancel = AtomicBool::new(false);
        for (size, to) in [
            ((37, 23), (37, 11)),
            ((37, 23), (37, 60)),
            ((37, 23), (16, 23)),
            ((37, 23), (16, 11)),
            ((5, 400), (300, 7)),
            ((400, 5), (7, 300)),
            ((1, 1), (64, 64)),
        ] {
            let rgba = vec![200; (size.0 * size.1 * 4) as usize];
            let (out, held) = lanczos(&rgba, size, to, &cancel).expect("resized");
            let used = out.capacity() as u64 + held;
            let reserved = resize_most(size, to);
            assert!(used <= reserved, "{size:?} to {to:?}: {used} held, {reserved} reserved");
        }
    }

    #[test]
    fn an_encode_keeps_only_its_bytes_in_the_budget() {
        let s = Arc::new(super::super::services::for_test(super::super::BlobBudget {
            bytes: 64 << 20,
            handles: 64,
        }));
        let due = Instant::now() + Duration::from_secs(30);
        // A flat image compresses to far less than the worst case reserved for it.
        for request in 1..=8 {
            let op = Op::Png { rgba: Data::Bytes(vec![9; 64 * 64 * 4]), size: (64, 64) };
            carry_out(&s, request, op, due, Duration::from_secs(30)).expect("encoded");
            let (charged, held) = s.blobs().counted();
            assert_eq!(held, charged, "after {request} encodes, what's held is what's charged");
        }
        assert!(s.blobs().counted().0 < 8 * 4096, "each blob is its PNG's few bytes");
    }

    #[test]
    fn tiny_webps_round_trip_opaque_and_transparent() {
        let cancel = AtomicBool::new(false);
        for (w, h) in [(1, 1), (2, 2)] {
            for alpha in [255u8, 0, 128] {
                let rgba: Vec<u8> =
                    (0..w * h).flat_map(|i| [200, 40, (i * 60) as u8, alpha]).collect();
                for (quality, method) in [(0.0, 0), (75.0, 4), (100.0, 6)] {
                    let webp = webp_of(&rgba, w, h, quality, method);
                    let (bw, bh, back) = decode(&webp, &cancel).expect("decoded back");
                    assert_eq!((bw, bh), (w, h));
                    for p in back.chunks_exact(4) {
                        assert_eq!(p[3], alpha, "{w}×{h} q{quality} m{method}: alpha is lossless");
                    }
                }
            }
        }
    }
}
