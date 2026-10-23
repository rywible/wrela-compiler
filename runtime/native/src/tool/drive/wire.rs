//! What the tool host and a driven build's process say to each other, and the steps a tool
//! asks for (language.md §15.5's `drive`), as both read them.
//!
//! **Frames.** Each message is a frame: its length (a `u32`, little-endian, of what follows),
//! its kind (a byte), then its bytes. The length comes first, so the reader knows a message's
//! size before it reads any of it: the tool host reserves a blob's bytes in its budget before a
//! build's process sends them, and refuses a frame of a kind larger than that kind may be.
//!
//! The tool host sends [`LOAD`] once, then [`STEPS`], [`POSTS`] and [`ANSWER`], each answered
//! by one [`DONE`]; and [`REPLY`], each the answer to an [`ASK`] of the process's, by its
//! number. While it carries out a command, the process asks ([`ASK`]) for a blob's bytes to be
//! reserved before it reads them back, and for a build's code (by its WASM's SHA-256, then if
//! need be its WASM), and sends each reserved blob's bytes ([`BLOB`]) before its [`DONE`].

use super::super::services::Body;
use super::super::toolchain::Out;
use std::io::{Read, Write};

/// The tool host's first command: the build to load, as [`Load`] encodes it.
pub(crate) const LOAD: u8 = 1;
/// Steps to run, as [`write_steps`] writes them.
pub(crate) const STEPS: u8 = 2;
/// The posts the build made that the tool hasn't been given.
pub(crate) const POSTS: u8 = 3;
/// An answer to a post: its number, then 0 and the bytes, or 1 and why it failed.
pub(crate) const ANSWER: u8 = 4;
/// The tool host's answer to an [`ASK`]: its number, then 0 and what was asked for, 1 and why
/// not, or (for code) 2: send the WASM.
pub(crate) const REPLY: u8 = 5;

/// The process asks: its number, then 0 and a blob's size (a `u64`) to reserve, 1 and the
/// SHA-256 of a build's WASM whose code it needs, or 2 and the WASM itself.
pub(crate) const ASK: u8 = 10;
/// A reserved blob's bytes: its handle (a `u32`), then the bytes.
pub(crate) const BLOB: u8 = 11;
/// A command's end: 0 and its answer (as std reads it), or 1 and why it failed.
pub(crate) const DONE: u8 = 12;

/// What an [`ASK`] asks for.
pub(crate) const ASK_RESERVE: u8 = 0;
pub(crate) const ASK_CODE: u8 = 1;
pub(crate) const ASK_WASM: u8 = 2;

/// A [`REPLY`]'s status.
pub(crate) const GIVEN: u8 = 0;
pub(crate) const REFUSED: u8 = 1;
pub(crate) const SEND_WASM: u8 = 2;

/// The most steps one request runs.
pub(crate) const MOST_STEPS: usize = 1024;

/// The most bytes of an answer's inline part: everything but its blobs.
pub(crate) const MOST_INLINE: usize = 512 << 10;

/// The most frames or ticks one step runs.
pub(crate) const MOST_FRAMES: u32 = 1 << 20;

/// The most bytes of a [`DONE`]: a sanity bound, well past what a command answers (a steps
/// answer is at most [`MOST_INLINE`]; a posts answer, its posts' bodies and URLs).
pub(crate) const MOST_FRAME: usize = 64 << 20;

/// Writes a frame of `kind` holding `parts`, one after another.
pub(crate) fn write_frame(w: &mut impl Write, kind: u8, parts: &[&[u8]]) -> std::io::Result<()> {
    let len: usize = 1 + parts.iter().map(|p| p.len()).sum::<usize>();
    let len = u32::try_from(len).map_err(|_| std::io::Error::other("a frame past 4 GiB"))?;
    let mut head = [0u8; 5];
    head[..4].copy_from_slice(&len.to_le_bytes());
    head[4] = kind;
    w.write_all(&head)?;
    for p in parts {
        w.write_all(p)?;
    }
    w.flush()
}

/// A frame's kind and the length of its bytes, read: `None` at the end of the stream.
pub(crate) fn read_head(r: &mut impl Read) -> std::io::Result<Option<(u8, usize)>> {
    let mut head = [0u8; 5];
    let mut got = 0;
    while got < head.len() {
        match r.read(&mut head[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let len = u32::from_le_bytes(head[..4].try_into().expect("four bytes")) as usize;
    if len == 0 {
        return Err(std::io::Error::other("a frame with no kind"));
    }
    Ok(Some((head[4], len - 1)))
}

/// `len` bytes, read whole.
pub(crate) fn read_bytes(r: &mut impl Read, len: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = vec![0; len];
    r.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// How a build is loaded: its root path, with a GPU or on the CPU alone, its screen's size,
/// its frames' rate, how many threads run its parallel jobs (0: one per core), how its GPU
/// work is timed, how long each helper holds a job's result back (microseconds: a test's slow
/// jobs), and whether it records the commands it submits (`Commands`).
#[derive(Clone, Debug)]
#[cfg_attr(not(feature = "gpu"), allow(dead_code))]
pub(crate) struct Load {
    pub path: String,
    pub gpu: bool,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub workers: u32,
    pub timing: crate::Timing,
    pub hold: u32,
    pub record: bool,
}

impl Load {
    /// A load as std's `drive::load` writes it, up to its storage.
    pub(crate) fn parse(b: &mut Body) -> Result<Load, String> {
        let path = b.str()?;
        let gpu = b.u8()? != 0;
        let (width, height) = (b.u32()?, b.u32()?);
        let fps = f64::from_bits(b.u64()?);
        let workers = b.u32()?;
        let timing = match b.u8()? {
            0 => crate::Timing::Off,
            1 => crate::Timing::Span,
            2 => crate::Timing::Serial,
            k => return Err(format!("{k} isn't a timing mode")),
        };
        let hold = b.u32()?;
        let record = b.u8()? != 0;
        if !(fps.is_finite() && fps > 0.0) {
            return Err(format!("frames' rate is more than 0, not {fps}"));
        }
        if width == 0 || height == 0 || width > 16384 || height > 16384 {
            return Err(format!("a screen of {width}×{height} isn't 1 to 16384 each way"));
        }
        if workers > wrela_abi::memory::MAX_WORKERS + 1 {
            return Err(format!("at most {} workers", wrela_abi::memory::MAX_WORKERS + 1));
        }
        Ok(Load { path, gpu, width, height, fps, workers, timing, hold, record })
    }

    /// As [`Load::parse`] reads it.
    pub(crate) fn write(&self, out: &mut Out) {
        out.str(&self.path).u8(u8::from(self.gpu)).u32(self.width).u32(self.height);
        out.f64(self.fps).u32(self.workers);
        out.u8(match self.timing {
            crate::Timing::Off => 0,
            crate::Timing::Span => 1,
            crate::Timing::Serial => 2,
        });
        out.u32(self.hold).u8(u8::from(self.record));
    }
}

/// A step of a `steps` request, as std's `drive::Step` is: a script as its JSON (empty: none).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    Init,
    Frames(u32, String),
    FrameAt(f32),
    Ticks(u32, String),
    Call(String, Vec<crate::Value>),
    Audio(u32, bool),
    Capture,
    Hash,
    Phases,
    Buffers,
    ReadBuffer(u32),
    Commands,
    Running,
    Hold(String),
    Release(String),
}

impl Step {
    /// Its name, as a request's failure names it.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Step::Init => "init",
            Step::Frames(..) => "frames",
            Step::FrameAt(..) => "frame_at",
            Step::Ticks(..) => "ticks",
            Step::Call(..) => "call",
            Step::Audio(..) => "audio",
            Step::Capture => "capture",
            Step::Hash => "hash",
            Step::Phases => "phases",
            Step::Buffers => "buffers",
            Step::ReadBuffer(..) => "read_buffer",
            Step::Commands => "commands",
            Step::Running => "running",
            Step::Hold(..) => "hold",
            Step::Release(..) => "release",
        }
    }

    /// Whether it runs the build's code: after a trap, such a step is skipped.
    pub(crate) fn runs_code(&self) -> bool {
        matches!(
            self,
            Step::Init
                | Step::Frames(..)
                | Step::FrameAt(..)
                | Step::Ticks(..)
                | Step::Call(..)
                | Step::Audio(..)
        )
    }
}

/// A script as `std::input::Script::as_json` writes it, or none for empty text.
pub(crate) fn script(text: &str) -> Result<Vec<wrela_abi::input::Scripted>, String> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    crate::parse_script(text).map_err(|e| format!("the script: {e}"))
}

/// Steps as std's `drive::write_steps` writes them: a count, then each step's kind and what it
/// carries. Each is checked as it's read: at most [`MOST_STEPS`], each script read, each time
/// finite, each call with at most 64 arguments.
pub(crate) fn parse_steps(b: &mut Body) -> Result<Vec<Step>, String> {
    let n = b.u32()? as usize;
    if n > MOST_STEPS {
        return Err(format!("{n} steps are more than a request runs ({MOST_STEPS})"));
    }
    let mut steps = Vec::with_capacity(n);
    for i in 0..n {
        let step = (|| {
            Ok::<_, String>(match b.u8()? {
                0 => Step::Init,
                k @ (1 | 3) => {
                    let n = b.u32()?;
                    if n > MOST_FRAMES {
                        return Err(format!("at most {MOST_FRAMES} at once"));
                    }
                    let text = b.str()?;
                    script(&text)?;
                    if k == 1 { Step::Frames(n, text) } else { Step::Ticks(n, text) }
                }
                2 => {
                    let time = f32::from_bits(b.u32()?);
                    if !time.is_finite() {
                        return Err(format!("a frame's time is finite, not {time}"));
                    }
                    Step::FrameAt(time)
                }
                4 => {
                    let export = b.str()?;
                    let n = b.u32()?;
                    if n > 64 {
                        return Err("an export takes at most 64 arguments".into());
                    }
                    let mut args = Vec::new();
                    for _ in 0..n {
                        let (kind, bits) = (b.u8()?, b.u64()?);
                        args.push(match kind {
                            0 => crate::Value::I32(bits as u32 as i32),
                            1 => crate::Value::I64(bits as i64),
                            2 => crate::Value::F32(f32::from_bits(bits as u32)),
                            3 => crate::Value::F64(f64::from_bits(bits)),
                            k => return Err(format!("{k} isn't a scalar's kind")),
                        });
                    }
                    Step::Call(export, args)
                }
                5 => Step::Audio(b.u32()?, b.u8()? != 0),
                6 => Step::Capture,
                7 => Step::Hash,
                8 => Step::Phases,
                9 => Step::Buffers,
                10 => Step::ReadBuffer(b.u32()?),
                11 => Step::Commands,
                12 => Step::Running,
                k @ (13 | 14) => {
                    let path = b.str()?;
                    if let Some(why) = wrela_abi::check::path_problem(&path) {
                        return Err(format!("a held path {why}"));
                    }
                    if k == 13 { Step::Hold(path) } else { Step::Release(path) }
                }
                k => return Err(format!("{k} isn't a step")),
            })
        })()
        .map_err(|why| format!("step {i}: {why}"))?;
        steps.push(step);
    }
    Ok(steps)
}

/// Writes `steps` as [`parse_steps`] reads them.
pub(crate) fn write_steps(out: &mut Out, steps: &[Step]) {
    out.u32(steps.len() as u32);
    for s in steps {
        match s {
            Step::Init => out.u8(0),
            Step::Frames(n, text) => out.u8(1).u32(*n).str(text),
            Step::FrameAt(t) => out.u8(2).u32(t.to_bits()),
            Step::Ticks(n, text) => out.u8(3).u32(*n).str(text),
            Step::Call(export, args) => {
                out.u8(4).str(export).u32(args.len() as u32);
                for v in args {
                    let (kind, bits) = scalar(*v);
                    out.u8(kind).u64(bits);
                }
                &mut *out
            }
            Step::Audio(quanta, timed) => out.u8(5).u32(*quanta).u8(u8::from(*timed)),
            Step::Capture => out.u8(6),
            Step::Hash => out.u8(7),
            Step::Phases => out.u8(8),
            Step::Buffers => out.u8(9),
            Step::ReadBuffer(h) => out.u8(10).u32(*h),
            Step::Commands => out.u8(11),
            Step::Running => out.u8(12),
            Step::Hold(path) => out.u8(13).str(path),
            Step::Release(path) => out.u8(14).str(path),
        };
    }
}

/// A scalar's kind and bits, as std's `drive::Scalar` crosses.
pub(crate) fn scalar(v: crate::Value) -> (u8, u64) {
    match v {
        crate::Value::I32(x) => (0, x as u32 as u64),
        crate::Value::I64(x) => (1, x as u64),
        crate::Value::F32(x) => (2, u64::from(x.to_bits())),
        crate::Value::F64(x) => (3, x.to_bits()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_read_back_as_written_and_too_many_are_refused() {
        let steps = vec![
            Step::Init,
            Step::Frames(3, String::new()),
            Step::FrameAt(0.5),
            Step::Ticks(2, String::new()),
            Step::Call("add".into(), vec![crate::Value::I32(-2), crate::Value::F64(0.25)]),
            Step::Audio(4, true),
            Step::Capture,
            Step::Hash,
            Step::Phases,
            Step::Buffers,
            Step::ReadBuffer(7),
            Step::Commands,
            Step::Running,
            Step::Hold("tiles/far.bin".into()),
            Step::Release("tiles/far.bin".into()),
        ];
        let mut out = Out::default();
        write_steps(&mut out, &steps);
        let mut b = Body::new(&out.0);
        assert_eq!(parse_steps(&mut b).expect("steps"), steps);
        assert!(b.end().is_ok());
        let mut out = Out::default();
        write_steps(&mut out, &vec![Step::Hash; MOST_STEPS + 1]);
        let why = parse_steps(&mut Body::new(&out.0)).expect_err("too many");
        assert!(why.contains("1025 steps are more than a request runs (1024)"), "{why}");
        let mut out = Out::default();
        write_steps(&mut out, &[Step::Hash, Step::FrameAt(f32::NAN)]);
        let why = parse_steps(&mut Body::new(&out.0)).expect_err("a time that isn't finite");
        assert!(why.starts_with("step 1: "), "{why}");
        let mut out = Out::default();
        write_steps(&mut out, &[Step::Hold("../outside".into())]);
        let why = parse_steps(&mut Body::new(&out.0)).expect_err("a path out of the build");
        assert!(why.starts_with("step 0: a held path"), "{why}");
    }

    #[test]
    fn a_frame_says_its_size_before_its_bytes() {
        let mut out = Vec::new();
        write_frame(&mut out, DONE, &[b"ab", b"c"]).expect("written");
        assert_eq!(out[..5], [4, 0, 0, 0, DONE]);
        let mut r = &out[..];
        assert_eq!(read_head(&mut r).expect("a head"), Some((DONE, 3)));
        assert_eq!(read_bytes(&mut r, 3).expect("bytes"), b"abc");
        assert_eq!(read_head(&mut r).expect("the end"), None);
    }
}
