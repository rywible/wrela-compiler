//! # The command stream, version 7
//!
//! A program records GPU work as commands in its own memory and hands them to the host in
//! batches through `wrela.submit(ptr, len)` (D-099). The host decodes each batch in bulk into
//! WebGPU (browser) or wgpu (native) calls. Everything is little-endian; every field is a 32-bit
//! word, so every command is 4-byte aligned.
//!
//! ## A batch
//!
//! | Offset | Field | |
//! |---|---|---|
//! | 0 | magic | the bytes `WRCS` |
//! | 4 | version | [`VERSION`]; a host rejects any other |
//! | 8 | body length | bytes of commands that follow |
//! | 12 | commands | each: `opcode`, `payload length`, then the payload |
//!
//! ## Commands
//!
//! | Opcode | Command | Payload (words) |
//! |---|---|---|
//! | 1 | `CreateBuffer` | `handle`, `size` (bytes, a positive multiple of 4). The buffer starts zeroed. |
//! | 2 | `WriteBuffer` | `handle`, `offset`, `length` (bytes, multiples of 4), then `length` bytes of data |
//! | 3 | `Dispatch` | `pipeline`, `groups x`, `groups y`, `groups z`, the bindings, the uniforms |
//! | 4 | `BeginScreenPass` | the clear colour: `r`, `g`, `b`, `a` as f32. A pass on the screen, with no depth. |
//! | 5 | `Draw` | `pipeline`, `vertices`, `instances`, the bindings, the uniforms |
//! | 6 | `Present` | none. Ends the screen pass and shows it. |
//! | 7 | `DestroyBuffer` | `handle`. Later commands can't use the buffer; work already recorded still can. |
//! | 8 | `CopyBuffer` | `source`, `source offset`, `destination`, `destination offset`, `size` (bytes, multiples of 4) |
//! | 9 | `CreateTexture` | `handle`, `width`, `height`, `format` ([`TextureFormat`]). It starts zeroed. |
//! | 10 | `WriteTexture` | `handle`, `x`, `y`, `width`, `height`, `length`, then `length` bytes: rows of `width` texels, top to bottom, no padding |
//! | 11 | `DestroyTexture` | `handle` |
//! | 12 | `CreateSampler` | `handle`, `filter` (0 nearest, 1 linear), `address` (0 clamp, 1 repeat), `compare` ([`Compare`]; 0 for a sampler that doesn't compare) |
//! | 13 | `DestroySampler` | `handle` |
//! | 14 | `BeginPass` | `colour` (a texture, [`SCREEN`] or [`NONE`]), `load` (0 clear, 1 keep), the clear colour `r`, `g`, `b`, `a` as f32, `depth` (a depth texture or [`NONE`]), `depth load`, `clear depth` as f32 |
//! | 15 | `EndPass` | none. Ends a pass that isn't on the screen. |
//! | 16 | `DispatchIndirect` | `pipeline`, `arguments buffer`, `offset` (three `u32` group counts there), the bindings, the uniforms |
//! | 17 | `DrawIndirect` | `pipeline`, `arguments buffer`, `offset` (vertex count, instance count, first vertex, first instance there), the bindings, the uniforms |
//! | 18 | `ReadBuffer` | `request`, `handle`, `offset`, `size`: the buffer's bytes, a request |
//! | 19 | `StorageRead` | `request`, `path length`, then the path's UTF-8, padded to 4 bytes: the bytes stored at the path, a request |
//! | 20 | `StorageWrite` | `request`, `path length`, `data length`, then the path's UTF-8 and the data, each padded to 4 bytes: stores the bytes at the path, a request |
//! | 21 | `Fetch` | `request`, `url length`, then the URL's UTF-8, padded to 4 bytes: the bytes at the URL, relative to the build, a request |
//! | 22 | `Log` | `length`, then a line of UTF-8, padded to 4 bytes: shown on the host's console (`std::io::print`) |
//! | 23 | `Post` | `request`, `url length`, `body length`, then the URL's UTF-8 and the body, each padded to 4 bytes: sends the body to the host that serves the build, at the URL relative to the build; the answer is the host's reply, a request |
//! | 24 | `DrawIndexedIndirect` | `pipeline`, `index buffer`, `index offset`, `index size` (bytes of `u32` indices there, each a multiple of 4), `arguments buffer`, `offset` (index count, instance count, first index, base vertex, first instance there), the bindings, the uniforms |
//!
//! **Requests** (language.md §6.15) are answered on a later call: the program polls each with
//! the imports `wrela.request_status(request) -> i32` (-1 while it's pending, -2 if it failed,
//! else the answer's length in bytes) and `wrela.request_take(request, ptr)` (copies the answer
//! to `ptr` and forgets the request). The program chooses request numbers, each live once.
//! Storage is "bytes at a path": a path is relative, `/`-separated, with no `.` or `..` parts.
//!
//! **Bindings** are a count, then three words each, in the order the pipeline's manifest entry
//! lists its bindings: a buffer's `handle`, `offset` and `size` in bytes; a texture's or a
//! sampler's `handle`, `0`, `0`. **Uniforms** are a byte count, then the uniform block's bytes
//! (exactly `uniform.size` bytes, or none when the pipeline has no uniform block).
//!
//! - **Handles** are chosen by the program: small integers, each naming one live resource (a
//!   buffer, a texture or a sampler) at a time. A resource lives until the program destroys it
//!   (a compiled wrela program does when the value that owns it is dropped: language.md §6.13);
//!   work recorded before that still uses it.
//! - **Pipelines** index [`crate::Manifest::pipelines`]. A dispatch names a compute pipeline and
//!   a draw a render pipeline.
//! - **Order:** commands take effect in the order they're recorded. A host that batches GPU
//!   work must flush recorded dispatches before applying a later `WriteBuffer`.
//! - **Passes:** `BeginScreenPass` or `BeginPass`, then any number of `Draw`s,
//!   `DrawIndirect`s and `DrawIndexedIndirect`s, then `Present` (a pass on the screen) or `EndPass` (any other). Draws
//!   happen only inside a pass; everything else only outside one. A pass closes in the same
//!   call that opened it (a browser's canvas texture lives only until the frame's task ends):
//!   [`Sequencer::end_frame`].
//!
//! Malformed input is an error in every host, never undefined behaviour: [`decode`] and
//! [`Sequencer`] say what's wrong.

use std::fmt;

/// The stream format's version. Bumped by any change a host could notice.
pub const VERSION: u32 = 7;
pub const MAGIC: [u8; 4] = *b"WRCS";
/// The batch header: magic, version, body length.
pub const HEADER_LEN: usize = 12;
/// A command's header: opcode, payload length.
pub const COMMAND_HEADER_LEN: usize = 8;
/// A pass's attachment that isn't there.
pub const NONE: u32 = 0xFFFF_FFFF;
/// A pass's colour attachment that is the screen.
pub const SCREEN: u32 = 0xFFFF_FFFE;
/// How both hosts carry out `WriteBuffer` without splitting a submission: its bytes go into an
/// upload ring, [`UPLOAD_START`] bytes at first, and a copy from there is recorded in order. A
/// write over [`UPLOAD_MAX`] bytes goes through the queue, between submissions.
pub const UPLOAD_START: u32 = 256 * 1024;
pub const UPLOAD_MAX: u32 = 16 * 1024 * 1024;

/// Command opcodes. Values never change meaning; retired ones stay reserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Opcode {
    CreateBuffer = 1,
    WriteBuffer = 2,
    Dispatch = 3,
    BeginScreenPass = 4,
    Draw = 5,
    Present = 6,
    DestroyBuffer = 7,
    CopyBuffer = 8,
    CreateTexture = 9,
    WriteTexture = 10,
    DestroyTexture = 11,
    CreateSampler = 12,
    DestroySampler = 13,
    BeginPass = 14,
    EndPass = 15,
    DispatchIndirect = 16,
    DrawIndirect = 17,
    ReadBuffer = 18,
    StorageRead = 19,
    StorageWrite = 20,
    Fetch = 21,
    Log = 22,
    Post = 23,
    DrawIndexedIndirect = 24,
}

impl Opcode {
    pub const ALL: [Opcode; 24] = [
        Opcode::CreateBuffer,
        Opcode::WriteBuffer,
        Opcode::Dispatch,
        Opcode::BeginScreenPass,
        Opcode::Draw,
        Opcode::Present,
        Opcode::DestroyBuffer,
        Opcode::CopyBuffer,
        Opcode::CreateTexture,
        Opcode::WriteTexture,
        Opcode::DestroyTexture,
        Opcode::CreateSampler,
        Opcode::DestroySampler,
        Opcode::BeginPass,
        Opcode::EndPass,
        Opcode::DispatchIndirect,
        Opcode::DrawIndirect,
        Opcode::ReadBuffer,
        Opcode::StorageRead,
        Opcode::StorageWrite,
        Opcode::Fetch,
        Opcode::Log,
        Opcode::Post,
        Opcode::DrawIndexedIndirect,
    ];

    pub fn from_u32(v: u32) -> Option<Opcode> {
        Opcode::ALL.iter().copied().find(|o| *o as u32 == v)
    }

    pub fn name(self) -> &'static str {
        match self {
            Opcode::CreateBuffer => "CreateBuffer",
            Opcode::WriteBuffer => "WriteBuffer",
            Opcode::Dispatch => "Dispatch",
            Opcode::BeginScreenPass => "BeginScreenPass",
            Opcode::Draw => "Draw",
            Opcode::Present => "Present",
            Opcode::DestroyBuffer => "DestroyBuffer",
            Opcode::CopyBuffer => "CopyBuffer",
            Opcode::CreateTexture => "CreateTexture",
            Opcode::WriteTexture => "WriteTexture",
            Opcode::DestroyTexture => "DestroyTexture",
            Opcode::CreateSampler => "CreateSampler",
            Opcode::DestroySampler => "DestroySampler",
            Opcode::BeginPass => "BeginPass",
            Opcode::EndPass => "EndPass",
            Opcode::DispatchIndirect => "DispatchIndirect",
            Opcode::DrawIndirect => "DrawIndirect",
            Opcode::ReadBuffer => "ReadBuffer",
            Opcode::StorageRead => "StorageRead",
            Opcode::StorageWrite => "StorageWrite",
            Opcode::Fetch => "Fetch",
            Opcode::Log => "Log",
            Opcode::Post => "Post",
            Opcode::DrawIndexedIndirect => "DrawIndexedIndirect",
        }
    }
}

/// A texture's format: how its texels are stored and sampled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TextureFormat {
    /// Four 8-bit channels, read as floats in [0, 1]: the screen's format.
    Rgba8 = 0,
    /// Four 16-bit floats.
    Rgba16Float = 1,
    /// A 32-bit float depth, for depth testing and comparison sampling.
    Depth32Float = 2,
}

impl TextureFormat {
    pub const ALL: [TextureFormat; 3] =
        [TextureFormat::Rgba8, TextureFormat::Rgba16Float, TextureFormat::Depth32Float];

    pub fn from_u32(v: u32) -> Option<TextureFormat> {
        TextureFormat::ALL.iter().copied().find(|f| *f as u32 == v)
    }

    /// WebGPU's name for it.
    pub fn name(self) -> &'static str {
        match self {
            TextureFormat::Rgba8 => "rgba8unorm",
            TextureFormat::Rgba16Float => "rgba16float",
            TextureFormat::Depth32Float => "depth32float",
        }
    }

    /// The bytes of one texel.
    pub fn bytes_per_texel(self) -> u32 {
        match self {
            TextureFormat::Rgba8 | TextureFormat::Depth32Float => 4,
            TextureFormat::Rgba16Float => 8,
        }
    }

    pub fn is_depth(self) -> bool {
        self == TextureFormat::Depth32Float
    }
}

/// How two depths compare: a comparison sampler's test (what passes, the reference against the
/// texel), and a render pipeline's depth test (the fragment's depth against the target's). (0 in
/// the stream is a sampler that doesn't compare.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Compare {
    #[default]
    Less = 1,
    LessEqual = 2,
    Greater = 3,
    GreaterEqual = 4,
    Equal = 5,
    NotEqual = 6,
    Always = 7,
    Never = 8,
}

impl Compare {
    pub const ALL: [Compare; 8] = [
        Compare::Less,
        Compare::LessEqual,
        Compare::Greater,
        Compare::GreaterEqual,
        Compare::Equal,
        Compare::NotEqual,
        Compare::Always,
        Compare::Never,
    ];

    pub fn from_u32(v: u32) -> Option<Compare> {
        Compare::ALL.into_iter().find(|c| *c as u32 == v)
    }

    /// WebGPU's name for it.
    pub fn name(self) -> &'static str {
        match self {
            Compare::Less => "less",
            Compare::LessEqual => "less-equal",
            Compare::Greater => "greater",
            Compare::GreaterEqual => "greater-equal",
            Compare::Equal => "equal",
            Compare::NotEqual => "not-equal",
            Compare::Always => "always",
            Compare::Never => "never",
        }
    }
}

/// One binding of a dispatch or draw: a buffer's range, or a texture or sampler (whose offset
/// and size are 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Binding {
    pub handle: u32,
    pub offset: u32,
    pub size: u32,
}

impl Binding {
    /// A whole resource, or a texture or sampler.
    pub fn of(handle: u32) -> Binding {
        Binding { handle, offset: 0, size: 0 }
    }

    pub fn range(handle: u32, offset: u32, size: u32) -> Binding {
        Binding { handle, offset, size }
    }
}

/// A pass's attachments (`BeginPass`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pass {
    /// A texture, [`SCREEN`] or [`NONE`].
    pub color: u32,
    /// Keep what's there rather than clear it.
    pub keep_color: bool,
    pub clear: [f32; 4],
    /// A depth texture, or [`NONE`].
    pub depth: u32,
    pub keep_depth: bool,
    pub clear_depth: f32,
}

/// A decoded command. Byte payloads borrow from the batch.
#[derive(Clone, Debug, PartialEq)]
pub enum Command<'a> {
    CreateBuffer {
        handle: u32,
        size: u32,
    },
    WriteBuffer {
        handle: u32,
        offset: u32,
        data: &'a [u8],
    },
    Dispatch {
        pipeline: u32,
        groups: [u32; 3],
        bindings: Vec<Binding>,
        uniforms: &'a [u8],
    },
    BeginScreenPass {
        clear: [f32; 4],
    },
    Draw {
        pipeline: u32,
        vertices: u32,
        instances: u32,
        bindings: Vec<Binding>,
        uniforms: &'a [u8],
    },
    Present,
    DestroyBuffer {
        handle: u32,
    },
    CopyBuffer {
        source: u32,
        source_offset: u32,
        destination: u32,
        destination_offset: u32,
        size: u32,
    },
    CreateTexture {
        handle: u32,
        width: u32,
        height: u32,
        format: TextureFormat,
    },
    WriteTexture {
        handle: u32,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        data: &'a [u8],
    },
    DestroyTexture {
        handle: u32,
    },
    CreateSampler {
        handle: u32,
        linear: bool,
        repeat: bool,
        compare: Option<Compare>,
    },
    DestroySampler {
        handle: u32,
    },
    BeginPass(Pass),
    EndPass,
    DispatchIndirect {
        pipeline: u32,
        arguments: u32,
        offset: u32,
        bindings: Vec<Binding>,
        uniforms: &'a [u8],
    },
    DrawIndirect {
        pipeline: u32,
        arguments: u32,
        offset: u32,
        bindings: Vec<Binding>,
        uniforms: &'a [u8],
    },
    /// Indexed: `u32` indices, `index_size` bytes at `index_offset` in buffer `indices`.
    DrawIndexedIndirect {
        pipeline: u32,
        indices: u32,
        index_offset: u32,
        index_size: u32,
        arguments: u32,
        offset: u32,
        bindings: Vec<Binding>,
        uniforms: &'a [u8],
    },
    ReadBuffer {
        request: u32,
        handle: u32,
        offset: u32,
        size: u32,
    },
    StorageRead {
        request: u32,
        path: &'a str,
    },
    StorageWrite {
        request: u32,
        path: &'a str,
        data: &'a [u8],
    },
    Fetch {
        request: u32,
        url: &'a str,
    },
    Log {
        text: &'a str,
    },
    Post {
        request: u32,
        url: &'a str,
        body: &'a [u8],
    },
}

impl Command<'_> {
    pub fn opcode(&self) -> Opcode {
        match self {
            Command::CreateBuffer { .. } => Opcode::CreateBuffer,
            Command::WriteBuffer { .. } => Opcode::WriteBuffer,
            Command::Dispatch { .. } => Opcode::Dispatch,
            Command::BeginScreenPass { .. } => Opcode::BeginScreenPass,
            Command::Draw { .. } => Opcode::Draw,
            Command::Present => Opcode::Present,
            Command::DestroyBuffer { .. } => Opcode::DestroyBuffer,
            Command::CopyBuffer { .. } => Opcode::CopyBuffer,
            Command::CreateTexture { .. } => Opcode::CreateTexture,
            Command::WriteTexture { .. } => Opcode::WriteTexture,
            Command::DestroyTexture { .. } => Opcode::DestroyTexture,
            Command::CreateSampler { .. } => Opcode::CreateSampler,
            Command::DestroySampler { .. } => Opcode::DestroySampler,
            Command::BeginPass(_) => Opcode::BeginPass,
            Command::EndPass => Opcode::EndPass,
            Command::DispatchIndirect { .. } => Opcode::DispatchIndirect,
            Command::DrawIndirect { .. } => Opcode::DrawIndirect,
            Command::DrawIndexedIndirect { .. } => Opcode::DrawIndexedIndirect,
            Command::ReadBuffer { .. } => Opcode::ReadBuffer,
            Command::StorageRead { .. } => Opcode::StorageRead,
            Command::StorageWrite { .. } => Opcode::StorageWrite,
            Command::Fetch { .. } => Opcode::Fetch,
            Command::Log { .. } => Opcode::Log,
            Command::Post { .. } => Opcode::Post,
        }
    }
}

/// What's wrong with a batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamError {
    TooShort {
        needed: usize,
        got: usize,
    },
    BadMagic([u8; 4]),
    WrongVersion {
        expected: u32,
        got: u32,
    },
    BodyLength {
        declared: usize,
        actual: usize,
    },
    UnknownOpcode {
        opcode: u32,
        at: usize,
    },
    BadPayload {
        opcode: Opcode,
        at: usize,
        why: &'static str,
    },
    Sequence {
        opcode: Opcode,
        why: &'static str,
    },
    /// A call returned with a pass still open.
    UnclosedPass,
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamError::TooShort { needed, got } => {
                write!(f, "batch too short: needs {needed} bytes, has {got}")
            }
            StreamError::BadMagic(m) => {
                write!(f, "not a command batch: magic is {m:?}, not `WRCS`")
            }
            StreamError::WrongVersion { expected, got } => {
                write!(f, "command stream version {got}, but this host reads version {expected}")
            }
            StreamError::BodyLength { declared, actual } => {
                write!(f, "batch declares {declared} bytes of commands but has {actual}")
            }
            StreamError::UnknownOpcode { opcode, at } => {
                write!(f, "unknown opcode {opcode} at byte {at}")
            }
            StreamError::BadPayload { opcode, at, why } => {
                write!(f, "malformed {} at byte {at}: {why}", opcode.name())
            }
            StreamError::Sequence { opcode, why } => {
                write!(f, "{} out of sequence: {why}", opcode.name())
            }
            StreamError::UnclosedPass => {
                write!(f, "a call returned with a pass still open (no Present or EndPass)")
            }
        }
    }
}

impl std::error::Error for StreamError {}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Reads the bindings and uniforms that end a dispatch's or draw's payload `p` (words from
/// `from`).
fn bindings_and_uniforms(p: &[u8], from: usize) -> Result<(Vec<Binding>, &[u8]), &'static str> {
    let words = p.len() / 4;
    let w = |i: usize| word(p, i * 4);
    if words < from + 2 {
        return Err("payload too short");
    }
    let n = w(from) as usize;
    if words < from + 1 + 3 * n + 1 {
        return Err("binding list runs past the payload");
    }
    let bindings = (0..n)
        .map(|k| {
            let at = from + 1 + 3 * k;
            Binding { handle: w(at), offset: w(at + 1), size: w(at + 2) }
        })
        .collect();
    let ulen = w(from + 1 + 3 * n) as usize;
    let ustart = (from + 2 + 3 * n) * 4;
    if ulen != p.len() - ustart {
        return Err("uniform length doesn't match the payload");
    }
    Ok((bindings, &p[ustart..]))
}

/// Decodes one batch: its header, then every command. It doesn't check pass sequencing
/// (that spans batches: see [`Sequencer`]).
pub fn decode(batch: &[u8]) -> Result<Vec<Command<'_>>, StreamError> {
    if batch.len() < HEADER_LEN {
        return Err(StreamError::TooShort { needed: HEADER_LEN, got: batch.len() });
    }
    let magic = [batch[0], batch[1], batch[2], batch[3]];
    if magic != MAGIC {
        return Err(StreamError::BadMagic(magic));
    }
    let version = word(batch, 4);
    if version != VERSION {
        return Err(StreamError::WrongVersion { expected: VERSION, got: version });
    }
    let body_len = word(batch, 8) as usize;
    if body_len != batch.len() - HEADER_LEN {
        return Err(StreamError::BodyLength {
            declared: body_len,
            actual: batch.len() - HEADER_LEN,
        });
    }
    let mut out = Vec::new();
    let mut at = HEADER_LEN;
    while at < batch.len() {
        if batch.len() - at < COMMAND_HEADER_LEN {
            return Err(StreamError::TooShort {
                needed: at + COMMAND_HEADER_LEN,
                got: batch.len(),
            });
        }
        let op = word(batch, at);
        let len = word(batch, at + 4) as usize;
        let Some(opcode) = Opcode::from_u32(op) else {
            return Err(StreamError::UnknownOpcode { opcode: op, at });
        };
        let start = at + COMMAND_HEADER_LEN;
        let bad = |why| StreamError::BadPayload { opcode, at, why };
        if !len.is_multiple_of(4) {
            return Err(bad("payload length isn't a multiple of 4"));
        }
        if batch.len() - start < len {
            return Err(bad("payload runs past the end of the batch"));
        }
        let p = &batch[start..start + len];
        let words = len / 4;
        let w = |i: usize| word(p, i * 4);
        let exactly = |n: usize| if words == n { Ok(()) } else { Err(bad(expected(n))) };
        // The `n` bytes of UTF-8 text at byte `from` of the payload (`not_utf8` if they aren't).
        let text = |from: usize, n: usize, not_utf8| {
            std::str::from_utf8(&p[from..from + n]).map_err(|_| bad(not_utf8))
        };
        // A text, then bytes, their lengths in words 1 and 2 (`mismatch` if they don't fill the
        // payload, `not_utf8` if the text isn't UTF-8).
        let two_runs = |mismatch, not_utf8| {
            if words < 3 {
                return Err(bad("expected at least 3 words"));
            }
            let (n, m) = (w(1) as usize, w(2) as usize);
            if padded(n).checked_add(padded(m)) != Some(len - 12) {
                return Err(bad(mismatch));
            }
            let at = 12 + padded(n);
            Ok((text(12, n, not_utf8)?, &p[at..at + m]))
        };
        let cmd = match opcode {
            Opcode::CreateBuffer => {
                exactly(2)?;
                let size = w(1);
                if size == 0 || size % 4 != 0 {
                    return Err(bad("size must be a positive multiple of 4"));
                }
                Command::CreateBuffer { handle: w(0), size }
            }
            Opcode::WriteBuffer => {
                if words < 3 {
                    return Err(bad("expected at least 3 words"));
                }
                let (offset, n) = (w(1), w(2) as usize);
                if offset % 4 != 0 || n % 4 != 0 {
                    return Err(bad("offset and length must be multiples of 4"));
                }
                if n != len - 12 {
                    return Err(bad("data length doesn't match the payload"));
                }
                Command::WriteBuffer { handle: w(0), offset, data: &p[12..] }
            }
            Opcode::Dispatch => {
                let (bindings, uniforms) = bindings_and_uniforms(p, 4).map_err(bad)?;
                Command::Dispatch { pipeline: w(0), groups: [w(1), w(2), w(3)], bindings, uniforms }
            }
            Opcode::Draw => {
                let (bindings, uniforms) = bindings_and_uniforms(p, 3).map_err(bad)?;
                Command::Draw {
                    pipeline: w(0),
                    vertices: w(1),
                    instances: w(2),
                    bindings,
                    uniforms,
                }
            }
            Opcode::DispatchIndirect | Opcode::DrawIndirect => {
                let (bindings, uniforms) = bindings_and_uniforms(p, 3).map_err(bad)?;
                if w(2) % 4 != 0 {
                    return Err(bad("the arguments' offset must be a multiple of 4"));
                }
                let (pipeline, arguments, offset) = (w(0), w(1), w(2));
                if opcode == Opcode::DispatchIndirect {
                    Command::DispatchIndirect { pipeline, arguments, offset, bindings, uniforms }
                } else {
                    Command::DrawIndirect { pipeline, arguments, offset, bindings, uniforms }
                }
            }
            Opcode::DrawIndexedIndirect => {
                let (bindings, uniforms) = bindings_and_uniforms(p, 6).map_err(bad)?;
                if w(2) % 4 != 0 || w(3) % 4 != 0 {
                    return Err(bad("the indices' offset and size must be multiples of 4"));
                }
                if w(5) % 4 != 0 {
                    return Err(bad("the arguments' offset must be a multiple of 4"));
                }
                Command::DrawIndexedIndirect {
                    pipeline: w(0),
                    indices: w(1),
                    index_offset: w(2),
                    index_size: w(3),
                    arguments: w(4),
                    offset: w(5),
                    bindings,
                    uniforms,
                }
            }
            Opcode::BeginScreenPass => {
                exactly(4)?;
                Command::BeginScreenPass { clear: [0, 1, 2, 3].map(|i| f32::from_bits(w(i))) }
            }
            Opcode::Present => {
                exactly(0)?;
                Command::Present
            }
            Opcode::EndPass => {
                exactly(0)?;
                Command::EndPass
            }
            Opcode::DestroyBuffer | Opcode::DestroyTexture | Opcode::DestroySampler => {
                exactly(1)?;
                match opcode {
                    Opcode::DestroyBuffer => Command::DestroyBuffer { handle: w(0) },
                    Opcode::DestroyTexture => Command::DestroyTexture { handle: w(0) },
                    _ => Command::DestroySampler { handle: w(0) },
                }
            }
            Opcode::CopyBuffer => {
                exactly(5)?;
                if [w(1), w(3), w(4)].iter().any(|x| x % 4 != 0) {
                    return Err(bad("offsets and size must be multiples of 4"));
                }
                Command::CopyBuffer {
                    source: w(0),
                    source_offset: w(1),
                    destination: w(2),
                    destination_offset: w(3),
                    size: w(4),
                }
            }
            Opcode::CreateTexture => {
                exactly(4)?;
                let Some(format) = TextureFormat::from_u32(w(3)) else {
                    return Err(bad("unknown texture format"));
                };
                if w(1) == 0 || w(2) == 0 {
                    return Err(bad("a texture's width and height are positive"));
                }
                Command::CreateTexture { handle: w(0), width: w(1), height: w(2), format }
            }
            Opcode::WriteTexture => {
                if words < 6 {
                    return Err(bad("expected at least 6 words"));
                }
                let n = w(5) as usize;
                if n != len - 24 {
                    return Err(bad("data length doesn't match the payload"));
                }
                Command::WriteTexture {
                    handle: w(0),
                    x: w(1),
                    y: w(2),
                    width: w(3),
                    height: w(4),
                    data: &p[24..],
                }
            }
            Opcode::CreateSampler => {
                exactly(4)?;
                if w(1) > 1 || w(2) > 1 {
                    return Err(bad("a sampler's filter and address are 0 or 1"));
                }
                let compare = match w(3) {
                    0 => None,
                    c => match Compare::from_u32(c) {
                        Some(c) => Some(c),
                        None => return Err(bad("unknown comparison")),
                    },
                };
                Command::CreateSampler {
                    handle: w(0),
                    linear: w(1) == 1,
                    repeat: w(2) == 1,
                    compare,
                }
            }
            Opcode::BeginPass => {
                exactly(9)?;
                if w(1) > 1 || w(7) > 1 {
                    return Err(bad("a load is 0 (clear) or 1 (keep)"));
                }
                Command::BeginPass(Pass {
                    color: w(0),
                    keep_color: w(1) == 1,
                    clear: [2, 3, 4, 5].map(|i| f32::from_bits(w(i))),
                    depth: w(6),
                    keep_depth: w(7) == 1,
                    clear_depth: f32::from_bits(w(8)),
                })
            }
            Opcode::ReadBuffer => {
                exactly(4)?;
                if w(2) % 4 != 0 || w(3) % 4 != 0 {
                    return Err(bad("offset and size must be multiples of 4"));
                }
                Command::ReadBuffer { request: w(0), handle: w(1), offset: w(2), size: w(3) }
            }
            Opcode::StorageRead | Opcode::Fetch => {
                if words < 2 {
                    return Err(bad("expected at least 2 words"));
                }
                let n = w(1) as usize;
                if padded(n) != len - 8 {
                    return Err(bad("the text's length doesn't match the payload"));
                }
                let text = text(8, n, "the text isn't UTF-8")?;
                if opcode == Opcode::Fetch {
                    Command::Fetch { request: w(0), url: text }
                } else {
                    Command::StorageRead { request: w(0), path: text }
                }
            }
            Opcode::Log => {
                if words < 1 {
                    return Err(bad("expected at least 1 word"));
                }
                let n = w(0) as usize;
                if padded(n) != len - 4 {
                    return Err(bad("the text's length doesn't match the payload"));
                }
                Command::Log { text: text(4, n, "the text isn't UTF-8")? }
            }
            Opcode::StorageWrite => {
                let (path, data) = two_runs(
                    "the path's and data's lengths don't match the payload",
                    "the path isn't UTF-8",
                )?;
                Command::StorageWrite { request: w(0), path, data }
            }
            Opcode::Post => {
                let (url, body) = two_runs(
                    "the URL's and body's lengths don't match the payload",
                    "the URL isn't UTF-8",
                )?;
                Command::Post { request: w(0), url, body }
            }
        };
        out.push(cmd);
        at = start + len;
    }
    Ok(out)
}

/// `n` rounded up to a multiple of 4: text and data in a payload are padded to whole words.
fn padded(n: usize) -> usize {
    n.div_ceil(4) * 4
}

/// What [`decode`] says when a command doesn't have exactly the number of words it needs, by
/// that number. Only the numbers that some command needs are here.
fn expected(words: usize) -> &'static str {
    match words {
        0 => "expected no payload",
        1 => "expected 1 word",
        2 => "expected 2 words",
        4 => "expected 4 words",
        5 => "expected 5 words",
        9 => "expected 9 words",
        _ => unreachable!("no command has exactly {words} words"),
    }
}

/// Where commands can be, between [`Sequencer`] steps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Place {
    #[default]
    Outside,
    ScreenPass,
    Pass,
}

/// Checks that commands come in a valid order across batches (see the module docs).
#[derive(Clone, Debug, Default)]
pub struct Sequencer {
    at: Place,
}

impl Sequencer {
    pub fn new() -> Sequencer {
        Sequencer::default()
    }

    pub fn step(&mut self, cmd: &Command) -> Result<(), StreamError> {
        let op = cmd.opcode();
        let err = |why| Err(StreamError::Sequence { opcode: op, why });
        match (cmd, self.at) {
            (Command::BeginScreenPass { .. } | Command::BeginPass(_), Place::Outside) => {
                let screen = match cmd {
                    Command::BeginPass(p) => p.color == SCREEN,
                    _ => true,
                };
                self.at = if screen { Place::ScreenPass } else { Place::Pass };
                Ok(())
            }
            (Command::BeginScreenPass { .. } | Command::BeginPass(_), _) => {
                err("a pass is already open")
            }
            (
                Command::Draw { .. }
                | Command::DrawIndirect { .. }
                | Command::DrawIndexedIndirect { .. },
                at,
            ) => {
                if at == Place::Outside {
                    err("a draw must come inside a pass")
                } else {
                    Ok(())
                }
            }
            (Command::Present, Place::ScreenPass) => {
                self.at = Place::Outside;
                Ok(())
            }
            (Command::Present, _) => err("Present must close a pass on the screen"),
            (Command::EndPass, Place::Pass) => {
                self.at = Place::Outside;
                Ok(())
            }
            (Command::EndPass, _) => err("EndPass must close a pass that isn't on the screen"),
            (_, Place::Outside) => Ok(()),
            (_, _) => err("only draws can happen inside a pass"),
        }
    }

    /// Checks the end of a call: a host calls this after each call into the program returns.
    pub fn end_frame(&self) -> Result<(), StreamError> {
        if self.at == Place::Outside { Ok(()) } else { Err(StreamError::UnclosedPass) }
    }
}

/// Builds batches. The compiler writes the same bytes from WASM; this encoder is the reference
/// for tests and fixtures.
#[derive(Debug, Default)]
pub struct Encoder {
    body: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Encoder {
        Encoder::default()
    }

    fn put(&mut self, w: u32) {
        self.body.extend_from_slice(&w.to_le_bytes());
    }

    fn command(&mut self, op: Opcode, payload_words: &[u32], bytes: &[u8]) {
        assert!(bytes.len().is_multiple_of(4), "payload bytes must be a multiple of 4");
        self.put(op as u32);
        self.put((payload_words.len() * 4 + bytes.len()) as u32);
        for &w in payload_words {
            self.put(w);
        }
        self.body.extend_from_slice(bytes);
    }

    /// The words of a binding list and a uniform byte count.
    fn bound(words: &mut Vec<u32>, bindings: &[Binding], uniforms: &[u8]) {
        words.push(bindings.len() as u32);
        for b in bindings {
            words.extend([b.handle, b.offset, b.size]);
        }
        words.push(uniforms.len() as u32);
    }

    pub fn create_buffer(&mut self, handle: u32, size: u32) -> &mut Self {
        self.command(Opcode::CreateBuffer, &[handle, size], &[]);
        self
    }

    pub fn write_buffer(&mut self, handle: u32, offset: u32, data: &[u8]) -> &mut Self {
        self.command(Opcode::WriteBuffer, &[handle, offset, data.len() as u32], data);
        self
    }

    pub fn dispatch(
        &mut self,
        pipeline: u32,
        groups: [u32; 3],
        bindings: &[Binding],
        uniforms: &[u8],
    ) -> &mut Self {
        let mut words = vec![pipeline, groups[0], groups[1], groups[2]];
        Self::bound(&mut words, bindings, uniforms);
        self.command(Opcode::Dispatch, &words, uniforms);
        self
    }

    pub fn begin_screen_pass(&mut self, clear: [f32; 4]) -> &mut Self {
        self.command(Opcode::BeginScreenPass, &clear.map(f32::to_bits), &[]);
        self
    }

    pub fn draw(
        &mut self,
        pipeline: u32,
        vertices: u32,
        instances: u32,
        bindings: &[Binding],
        uniforms: &[u8],
    ) -> &mut Self {
        let mut words = vec![pipeline, vertices, instances];
        Self::bound(&mut words, bindings, uniforms);
        self.command(Opcode::Draw, &words, uniforms);
        self
    }

    pub fn present(&mut self) -> &mut Self {
        self.command(Opcode::Present, &[], &[]);
        self
    }

    pub fn destroy_buffer(&mut self, handle: u32) -> &mut Self {
        self.command(Opcode::DestroyBuffer, &[handle], &[]);
        self
    }

    pub fn copy_buffer(
        &mut self,
        source: u32,
        source_offset: u32,
        destination: u32,
        destination_offset: u32,
        size: u32,
    ) -> &mut Self {
        let words = [source, source_offset, destination, destination_offset, size];
        self.command(Opcode::CopyBuffer, &words, &[]);
        self
    }

    pub fn create_texture(
        &mut self,
        handle: u32,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> &mut Self {
        self.command(Opcode::CreateTexture, &[handle, width, height, format as u32], &[]);
        self
    }

    pub fn write_texture(
        &mut self,
        handle: u32,
        origin: [u32; 2],
        size: [u32; 2],
        data: &[u8],
    ) -> &mut Self {
        let words = [handle, origin[0], origin[1], size[0], size[1], data.len() as u32];
        self.command(Opcode::WriteTexture, &words, data);
        self
    }

    pub fn destroy_texture(&mut self, handle: u32) -> &mut Self {
        self.command(Opcode::DestroyTexture, &[handle], &[]);
        self
    }

    pub fn create_sampler(
        &mut self,
        handle: u32,
        linear: bool,
        repeat: bool,
        compare: Option<Compare>,
    ) -> &mut Self {
        let words = [handle, u32::from(linear), u32::from(repeat), compare.map_or(0, |c| c as u32)];
        self.command(Opcode::CreateSampler, &words, &[]);
        self
    }

    pub fn destroy_sampler(&mut self, handle: u32) -> &mut Self {
        self.command(Opcode::DestroySampler, &[handle], &[]);
        self
    }

    pub fn begin_pass(&mut self, pass: Pass) -> &mut Self {
        let c = pass.clear.map(f32::to_bits);
        let words = [
            pass.color,
            u32::from(pass.keep_color),
            c[0],
            c[1],
            c[2],
            c[3],
            pass.depth,
            u32::from(pass.keep_depth),
            pass.clear_depth.to_bits(),
        ];
        self.command(Opcode::BeginPass, &words, &[]);
        self
    }

    pub fn end_pass(&mut self) -> &mut Self {
        self.command(Opcode::EndPass, &[], &[]);
        self
    }

    pub fn dispatch_indirect(
        &mut self,
        pipeline: u32,
        arguments: u32,
        offset: u32,
        bindings: &[Binding],
        uniforms: &[u8],
    ) -> &mut Self {
        let mut words = vec![pipeline, arguments, offset];
        Self::bound(&mut words, bindings, uniforms);
        self.command(Opcode::DispatchIndirect, &words, uniforms);
        self
    }

    pub fn draw_indirect(
        &mut self,
        pipeline: u32,
        arguments: u32,
        offset: u32,
        bindings: &[Binding],
        uniforms: &[u8],
    ) -> &mut Self {
        let mut words = vec![pipeline, arguments, offset];
        Self::bound(&mut words, bindings, uniforms);
        self.command(Opcode::DrawIndirect, &words, uniforms);
        self
    }

    /// `indices` is the index buffer's handle, offset and size in bytes; `arguments` and
    /// `offset` say where the counts are.
    pub fn draw_indexed_indirect(
        &mut self,
        pipeline: u32,
        indices: [u32; 3],
        arguments: u32,
        offset: u32,
        bindings: &[Binding],
        uniforms: &[u8],
    ) -> &mut Self {
        let mut words = vec![pipeline, indices[0], indices[1], indices[2], arguments, offset];
        Self::bound(&mut words, bindings, uniforms);
        self.command(Opcode::DrawIndexedIndirect, &words, uniforms);
        self
    }

    pub fn read_buffer(&mut self, request: u32, handle: u32, offset: u32, size: u32) -> &mut Self {
        self.command(Opcode::ReadBuffer, &[request, handle, offset, size], &[]);
        self
    }

    /// Bytes padded with zeros to a multiple of 4.
    fn pad(bytes: &[u8]) -> Vec<u8> {
        let mut b = bytes.to_vec();
        b.resize(padded(b.len()), 0);
        b
    }

    pub fn storage_read(&mut self, request: u32, path: &str) -> &mut Self {
        let bytes = Self::pad(path.as_bytes());
        self.command(Opcode::StorageRead, &[request, path.len() as u32], &bytes);
        self
    }

    pub fn storage_write(&mut self, request: u32, path: &str, data: &[u8]) -> &mut Self {
        let mut bytes = Self::pad(path.as_bytes());
        bytes.extend(Self::pad(data));
        let words = [request, path.len() as u32, data.len() as u32];
        self.command(Opcode::StorageWrite, &words, &bytes);
        self
    }

    pub fn fetch(&mut self, request: u32, url: &str) -> &mut Self {
        let bytes = Self::pad(url.as_bytes());
        self.command(Opcode::Fetch, &[request, url.len() as u32], &bytes);
        self
    }

    pub fn post(&mut self, request: u32, url: &str, body: &[u8]) -> &mut Self {
        let mut bytes = Self::pad(url.as_bytes());
        bytes.extend(Self::pad(body));
        let words = [request, url.len() as u32, body.len() as u32];
        self.command(Opcode::Post, &words, &bytes);
        self
    }

    pub fn log(&mut self, text: &str) -> &mut Self {
        let bytes = Self::pad(text.as_bytes());
        self.command(Opcode::Log, &[text.len() as u32], &bytes);
        self
    }

    /// The batch: header and commands.
    pub fn finish(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.body.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(self.body.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.body);
        out
    }
}

#[cfg(test)]
mod tests;
