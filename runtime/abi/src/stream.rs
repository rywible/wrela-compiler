//! # The command stream, version 2
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
//! | 3 | `Dispatch` | `pipeline`, `groups x`, `groups y`, `groups z`, `buffer count`, the buffer handles, `uniform length`, then the uniform bytes |
//! | 4 | `BeginScreenPass` | the clear colour: `r`, `g`, `b`, `a` as f32 |
//! | 5 | `Draw` | `pipeline`, `vertices`, `instances`, `buffer count`, the buffer handles, `uniform length`, then the uniform bytes |
//! | 6 | `Present` | none |
//! | 7 | `DestroyBuffer` | `handle`. Later commands can't use the buffer; work already recorded still can. |
//!
//! - **Handles** are chosen by the program: small integers, each naming one live buffer at a
//!   time. A buffer lives until the program destroys it (a compiled wrela program destroys a
//!   call's buffers when its next call begins: language.md §12).
//! - **Pipelines** index [`crate::Manifest::pipelines`]. A dispatch names a compute pipeline and
//!   a draw a render pipeline; the buffer handles bind to the pipeline's `buffers`, in order, and
//!   the uniform bytes are its uniform block (exactly `uniform.size` bytes, or none when it has
//!   no uniform block).
//! - **Order:** commands take effect in the order they're recorded. A host that batches GPU
//!   work must flush recorded dispatches before applying a later `WriteBuffer`.
//! - **Frames:** `BeginScreenPass`, then any number of `Draw`s, then `Present`. Draws happen
//!   only inside a screen pass; dispatches, buffer creation, destruction and writes only
//!   outside one. A
//!   screen pass closes in the same call of `frame` that opened it (a browser's canvas texture
//!   lives only until the frame's task ends): [`Sequencer::end_frame`].
//!
//! Malformed input is an error in every host, never undefined behaviour: [`decode`] and
//! [`Sequencer`] say what's wrong.

use std::fmt;

/// The stream format's version. Bumped by any change a host could notice.
pub const VERSION: u32 = 2;
pub const MAGIC: [u8; 4] = *b"WRCS";
/// The batch header: magic, version, body length.
pub const HEADER_LEN: usize = 12;
/// A command's header: opcode, payload length.
pub const COMMAND_HEADER_LEN: usize = 8;

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
}

impl Opcode {
    pub const ALL: [Opcode; 7] = [
        Opcode::CreateBuffer,
        Opcode::WriteBuffer,
        Opcode::Dispatch,
        Opcode::BeginScreenPass,
        Opcode::Draw,
        Opcode::Present,
        Opcode::DestroyBuffer,
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
        }
    }
}

/// A decoded command. Byte payloads borrow from the batch.
#[derive(Clone, Debug, PartialEq)]
pub enum Command<'a> {
    CreateBuffer { handle: u32, size: u32 },
    WriteBuffer { handle: u32, offset: u32, data: &'a [u8] },
    Dispatch { pipeline: u32, groups: [u32; 3], buffers: Vec<u32>, uniforms: &'a [u8] },
    BeginScreenPass { clear: [f32; 4] },
    Draw { pipeline: u32, vertices: u32, instances: u32, buffers: Vec<u32>, uniforms: &'a [u8] },
    Present,
    DestroyBuffer { handle: u32 },
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
    /// `frame` returned with a screen pass still open.
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
                write!(f, "frame returned with a screen pass still open (no Present)")
            }
        }
    }
}

impl std::error::Error for StreamError {}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Decodes one batch: its header, then every command. It doesn't check frame sequencing
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
        let cmd = match opcode {
            Opcode::CreateBuffer => {
                if words != 2 {
                    return Err(bad("expected 2 words"));
                }
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
            Opcode::Dispatch | Opcode::Draw => {
                let fixed = if opcode == Opcode::Dispatch { 4 } else { 3 };
                if words < fixed + 2 {
                    return Err(bad("payload too short"));
                }
                let nbuf = w(fixed) as usize;
                if words < fixed + 1 + nbuf + 1 {
                    return Err(bad("buffer list runs past the payload"));
                }
                let buffers: Vec<u32> = (0..nbuf).map(|i| w(fixed + 1 + i)).collect();
                let ulen = w(fixed + 1 + nbuf) as usize;
                let ustart = (fixed + 2 + nbuf) * 4;
                if ulen != len - ustart {
                    return Err(bad("uniform length doesn't match the payload"));
                }
                let uniforms = &p[ustart..];
                if opcode == Opcode::Dispatch {
                    Command::Dispatch {
                        pipeline: w(0),
                        groups: [w(1), w(2), w(3)],
                        buffers,
                        uniforms,
                    }
                } else {
                    Command::Draw {
                        pipeline: w(0),
                        vertices: w(1),
                        instances: w(2),
                        buffers,
                        uniforms,
                    }
                }
            }
            Opcode::BeginScreenPass => {
                if words != 4 {
                    return Err(bad("expected 4 words"));
                }
                Command::BeginScreenPass { clear: [0, 1, 2, 3].map(|i| f32::from_bits(w(i))) }
            }
            Opcode::Present => {
                if words != 0 {
                    return Err(bad("expected no payload"));
                }
                Command::Present
            }
            Opcode::DestroyBuffer => {
                if words != 1 {
                    return Err(bad("expected 1 word"));
                }
                Command::DestroyBuffer { handle: w(0) }
            }
        };
        out.push(cmd);
        at = start + len;
    }
    Ok(out)
}

/// Checks that commands come in a valid order across batches (see the module docs).
#[derive(Clone, Debug, Default)]
pub struct Sequencer {
    in_pass: bool,
}

impl Sequencer {
    pub fn new() -> Sequencer {
        Sequencer::default()
    }

    pub fn step(&mut self, cmd: &Command) -> Result<(), StreamError> {
        let op = cmd.opcode();
        let err = |why| Err(StreamError::Sequence { opcode: op, why });
        match cmd {
            Command::BeginScreenPass { .. } if self.in_pass => err("a screen pass is already open"),
            Command::BeginScreenPass { .. } => {
                self.in_pass = true;
                Ok(())
            }
            Command::Draw { .. } if !self.in_pass => err("a draw must come after BeginScreenPass"),
            Command::Present if !self.in_pass => err("Present must close a screen pass"),
            Command::Present => {
                self.in_pass = false;
                Ok(())
            }
            Command::Dispatch { .. }
            | Command::CreateBuffer { .. }
            | Command::WriteBuffer { .. }
            | Command::DestroyBuffer { .. }
                if self.in_pass =>
            {
                err("only draws can happen inside a screen pass")
            }
            _ => Ok(()),
        }
    }

    /// Checks the end of a frame: a host calls this after each call of `frame` returns.
    pub fn end_frame(&self) -> Result<(), StreamError> {
        if self.in_pass { Err(StreamError::UnclosedPass) } else { Ok(()) }
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
        buffers: &[u32],
        uniforms: &[u8],
    ) -> &mut Self {
        let mut words = vec![pipeline, groups[0], groups[1], groups[2], buffers.len() as u32];
        words.extend_from_slice(buffers);
        words.push(uniforms.len() as u32);
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
        buffers: &[u32],
        uniforms: &[u8],
    ) -> &mut Self {
        let mut words = vec![pipeline, vertices, instances, buffers.len() as u32];
        words.extend_from_slice(buffers);
        words.push(uniforms.len() as u32);
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
