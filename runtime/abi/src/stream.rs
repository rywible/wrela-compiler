//! # The command stream, version 12
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
//! | 9 | `CreateTexture` | `handle`, `width`, `height`, `format` ([`TextureFormat`]; bit 16 set: kernels write it, a storage texture too, of a format [`TextureFormat::storable`]), `kind` ([`TextureKind`]: 2D, 3D, 2D array or cube), `depth or layers` (a 3D texture's depth, an array's layers, 6 for a cube, 1 for a 2D texture), `levels` (mip levels, at least 1; a 3D texture has 1). It starts zeroed. |
//! | 10 | `WriteTexture` | `handle`, `level`, `layer` (a 3D texture's: its slice), `x`, `y`, `width`, `height`, `length`, then `length` bytes: rows of `width` texels (of a compressed format, rows of blocks, `width` and `height` in texels, whole blocks but at the level's edge), top to bottom, no row padding; the byte run is padded to 4 bytes |
//! | 11 | `DestroyTexture` | `handle` |
//! | 12 | `CreateSampler` | `handle`, `filter` (0 nearest, 1 linear), `address` (0 clamp, 1 repeat), `compare` ([`Compare`]; 0 for a sampler that doesn't compare), `mip filter` (0 nearest, 1 linear: between mip levels), `anisotropy` (1 to 16; above 1 only with `filter` and `mip filter` linear) |
//! | 13 | `DestroySampler` | `handle` |
//! | 14 | `BeginPass` | `colours` (0 to 4), `join` (1: it may join the pass before, [`Pass::join`]), then for each colour target `texture` (a texture, or [`SCREEN`] alone), `level`, `layer` (of an array or a cube; 0 of a 2D texture), `load` (0 clear, 1 keep), the clear colour `r`, `g`, `b`, `a` as f32; then `depth` (a depth texture or [`NONE`]), `depth layer`, `depth load`, `clear depth` as f32 |
//! | 15 | `EndPass` | none. Ends a pass that isn't on the screen. |
//! | 16 | `DispatchIndirect` | `pipeline`, `arguments buffer`, `offset` (three `u32` group counts there), the bindings, the uniforms |
//! | 17 | `DrawIndirect` | `pipeline`, `arguments buffer`, `offset` (vertex count, instance count, first vertex, first instance there), the bindings, the uniforms |
//! | 18 | `ReadBuffer` | `request`, `handle`, `offset`, `size`: the buffer's bytes, a request |
//! | 19 | `StorageRead` | `request`, `path length`, then the path's UTF-8, padded to 4 bytes: the bytes stored at the path, a request |
//! | 20 | `StorageWrite` | `request`, `path length`, `data length`, then the path's UTF-8 and the data, each padded to 4 bytes: stores the bytes at the path, a request |
//! | 21 | `Fetch` | `request`, `url length`, then the URL's UTF-8, padded to 4 bytes: the bytes at the URL, relative to the build, a request |
//! | 22 | | none: it was `Log`, a printed line, until a line went to the host at once (`wrela.print`, version 8) |
//! | 23 | `Post` | `request`, `url length`, `body length`, then the URL's UTF-8 and the body, each padded to 4 bytes: sends the body to the host that serves the build, at the URL relative to the build; the answer is the host's reply, a request |
//! | 24 | `DrawIndexedIndirect` | `pipeline`, `index buffer`, `index offset`, `index size` (bytes of `u32` indices there, each a multiple of 4), `arguments buffer`, `offset` (index count, instance count, first index, base vertex, first instance there), the bindings, the uniforms |
//! | 25 | `Label` | `length`, then a name of UTF-8, padded to 4 bytes: the name of the next pass or dispatch in the host's timings (`pass`, `screen pass` or the dispatch's pipeline when there's none). Outside a pass only. |
//! | 26 | `Prepare` | `request`, `path length`, then the path's UTF-8, padded to 4 bytes: gets the build at the path, relative to the program's build, ready to start (its WASM compiled, its pipelines built), in the background; answered with no bytes when it is, a request |
//! | 27 | `Unprepare` | `path length`, then the path's UTF-8, padded to 4 bytes: lets go of what `Prepare` got ready for that build (its pipelines that the running build doesn't use) |
//! | 28 | `HandOver` | `request`, `path length`, then the path's UTF-8, padded to 4 bytes: after this call, the build at the path takes this program's place, with what it kept (`std::reload`). The request is answered only if the hand-over fails, and then this program goes on |
//! | 29 | `HostRequest` | `request`, `service length`, `body length`, then UTF-8 service and opaque bytes, each padded to 4 bytes: a host capability request |
//! | 30 | `Timed` | `request`, `label length`, then the label's UTF-8, padded to 4 bytes: the GPU time of the passes and dispatches with that `Label` in the first frame from this one that runs one and whose time the host has read, summed: an `f32` of microseconds, a request. It fails on a host that can't time GPU work. |
//! | 31 | `CopyTexture` | `source`, `source level`, `source layer`, `x`, `y`, `destination`, `destination level`, `destination layer`, `x`, `y`, `width`, `height`: texels between two textures of the same format |
//! | 32 | `CopyTextureToBuffer` | `texture`, `level`, `layer`, `x`, `y`, `width`, `height`, `buffer`, `offset` (a multiple of 256), `bytes per row` (a multiple of 256, at least a row's): texels into a buffer, row by row |
//! | 33 | `CopyBufferToTexture` | `buffer`, `offset` (a multiple of 4), `bytes per row` (a multiple of 256), `texture`, `level`, `layer`, `x`, `y`, `width`, `height`: the reverse |
//! | 34 | `SetScissor` | `x`, `y`, `width`, `height`: the draws after it in its pass touch only that rectangle of its targets. Inside a pass only. |
//! | 35 | `SetViewport` | `x`, `y`, `width`, `height`, `min depth`, `max depth`, all f32: the draws after it in its pass map clip space to that rectangle and depth range. Inside a pass only. |
//! | 36 | `DrawIndexed` | `pipeline`, `index buffer`, `index offset`, `index size` (as `DrawIndexedIndirect`'s), `indices`, `instances`, `first index`, `base vertex` (an `i32`), `first instance`, the bindings, the uniforms |
//! | 37 | `MultiDrawIndirect` | `pipeline`, `arguments buffer`, `offset`, `count` (the most draws: `DrawArgs` one after another there), `count buffer` (a `u32` there holds how many of them to draw, or [`NONE`]: all of them), `count offset`, the bindings, the uniforms |
//! | 38 | `MultiDrawIndexedIndirect` | `pipeline`, `index buffer`, `index offset`, `index size`, `arguments buffer`, `offset`, `count`, `count buffer`, `count offset` (as `MultiDrawIndirect`'s, of `DrawIndexedArgs`), the bindings, the uniforms |
//!
//! **Requests** (language.md §17.1) are answered on a later call: the program polls each with
//! the imports `wrela.request_status(request) -> i32` (-1 while it's pending, -2 if it failed,
//! else the answer's length in bytes) and `wrela.request_take(request, ptr)` (copies the answer
//! to `ptr` and forgets the request). The program chooses request numbers, each live once.
//! Storage is "bytes at a path": a path is relative, `/`-separated, with no `.` or `..` parts.
//!
//! **Bindings** are a count, then three words each, in the order the pipeline's manifest entry
//! lists its bindings: a buffer's `handle`, `offset` and `size` in bytes; a sampler's `handle`,
//! `0`, `0`; a texture's `handle`, its levels and its layers: `0` for all of them, or one more
//! than the one level, or the one layer (bound as a 2D texture), that its view holds
//! ([`View`]). A level view's level counts from the texture's first, and its layer view's
//! levels are all of them. **Uniforms** are a byte count, then the uniform block's bytes
//! (exactly `uniform.size` bytes, or none when the pipeline has no uniform block).
//!
//! - **Handles** are chosen by the program: small integers, each naming one live resource (a
//!   buffer, a texture or a sampler) at a time. A resource lives until the program destroys it
//!   (a compiled wrela program does when the value that owns it is dropped: language.md §18.3);
//!   work recorded before that still uses it.
//! - **Pipelines** index [`crate::Manifest::pipelines`]. A dispatch names a compute pipeline and
//!   a draw a render pipeline.
//! - **Order:** commands take effect in the order they're recorded. A host that batches GPU
//!   work must flush recorded dispatches before applying a later `WriteBuffer`.
//! - **Passes:** `BeginScreenPass` or `BeginPass`, then any number of draws (`Draw`,
//!   `DrawIndirect`, `DrawIndexed`, `DrawIndexedIndirect`, `MultiDrawIndirect`,
//!   `MultiDrawIndexedIndirect`), `SetScissor`s and `SetViewport`s, then `Present` (a pass on the
//!   screen) or `EndPass` (any other). Those happen only inside a pass; everything else only
//!   outside one. A pass closes in the same
//!   call that opened it (a browser's canvas texture lives only until the frame's task ends):
//!   [`Sequencer::end_frame`].
//!
//! Malformed input is an error in every host, never undefined behaviour: [`decode`] and
//! [`Sequencer`] say what's wrong.

use std::fmt;

/// The stream format's version. Bumped by any change a host could notice.
pub const VERSION: u32 = 13;
pub const MAGIC: [u8; 4] = *b"WRCS";
/// The batch header: magic, version, body length.
pub const HEADER_LEN: usize = 12;
/// A command's header: opcode, payload length.
pub const COMMAND_HEADER_LEN: usize = 8;
/// A pass's attachment that isn't there.
pub const NONE: u32 = 0xFFFF_FFFF;
/// A pass's colour attachment that is the screen.
pub const SCREEN: u32 = 0xFFFF_FFFE;
/// How both hosts carry out buffer and texture writes without splitting a submission: their
/// bytes go into an upload ring, [`UPLOAD_START`] bytes at first, and a copy from there is
/// recorded in order. Texture rows are padded to 256 bytes. A write over [`UPLOAD_MAX`] bytes
/// (including row padding) goes through the queue, between submissions.
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
    Post = 23,
    DrawIndexedIndirect = 24,
    Label = 25,
    Prepare = 26,
    Unprepare = 27,
    HandOver = 28,
    HostRequest = 29,
    Timed = 30,
    CopyTexture = 31,
    CopyTextureToBuffer = 32,
    CopyBufferToTexture = 33,
    SetScissor = 34,
    SetViewport = 35,
    DrawIndexed = 36,
    MultiDrawIndirect = 37,
    MultiDrawIndexedIndirect = 38,
}

impl Opcode {
    pub const ALL: [Opcode; 37] = [
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
        Opcode::Post,
        Opcode::DrawIndexedIndirect,
        Opcode::Label,
        Opcode::Prepare,
        Opcode::Unprepare,
        Opcode::HandOver,
        Opcode::HostRequest,
        Opcode::Timed,
        Opcode::CopyTexture,
        Opcode::CopyTextureToBuffer,
        Opcode::CopyBufferToTexture,
        Opcode::SetScissor,
        Opcode::SetViewport,
        Opcode::DrawIndexed,
        Opcode::MultiDrawIndirect,
        Opcode::MultiDrawIndexedIndirect,
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
            Opcode::Post => "Post",
            Opcode::DrawIndexedIndirect => "DrawIndexedIndirect",
            Opcode::Label => "Label",
            Opcode::Prepare => "Prepare",
            Opcode::Unprepare => "Unprepare",
            Opcode::HandOver => "HandOver",
            Opcode::HostRequest => "HostRequest",
            Opcode::Timed => "Timed",
            Opcode::CopyTexture => "CopyTexture",
            Opcode::CopyTextureToBuffer => "CopyTextureToBuffer",
            Opcode::CopyBufferToTexture => "CopyBufferToTexture",
            Opcode::SetScissor => "SetScissor",
            Opcode::SetViewport => "SetViewport",
            Opcode::DrawIndexed => "DrawIndexed",
            Opcode::MultiDrawIndirect => "MultiDrawIndirect",
            Opcode::MultiDrawIndexedIndirect => "MultiDrawIndexedIndirect",
        }
    }

    /// Whether it's a draw: it happens inside a pass.
    pub fn is_draw(self) -> bool {
        matches!(
            self,
            Opcode::Draw
                | Opcode::DrawIndirect
                | Opcode::DrawIndexed
                | Opcode::DrawIndexedIndirect
                | Opcode::MultiDrawIndirect
                | Opcode::MultiDrawIndexedIndirect
        )
    }

    /// Whether it happens only inside a pass: a draw, a scissor or a viewport.
    pub fn in_pass(self) -> bool {
        self.is_draw() || matches!(self, Opcode::SetScissor | Opcode::SetViewport)
    }
}

/// `CreateTexture`'s format word with this bit set: kernels write the texture's texels.
pub const WRITABLE: u32 = 1 << 16;

/// `BeginPass`'s join word: the pass may join the one before it ([`Pass::join`]).
pub const PASS_JOIN: u32 = 1;

/// The most mip levels a texture has: a side of WebGPU's largest 2D texture, 8192 texels, halved
/// 13 times.
pub const MAX_LEVELS: u32 = 14;

/// The most mip levels a `width` × `height` texture has.
pub fn max_levels(width: u32, height: u32) -> u32 {
    32 - width.max(height).max(1).leading_zeros()
}

/// The width and height of level `level` of a `width` × `height` texture.
pub fn level_size(width: u32, height: u32, level: u32) -> (u32, u32) {
    ((width >> level).max(1), (height >> level).max(1))
}

/// The most layers of a 2D array: WebGPU's default `maxTextureArrayLayers`.
pub const MAX_LAYERS: u32 = 256;
/// The most anisotropy a sampler takes (WebGPU's `maxAnisotropy` is clamped to 16).
pub const MAX_ANISOTROPY: u32 = 16;
/// Where a copy between a buffer and a texture starts in the buffer, and how its rows are
/// padded: WebGPU's `bytesPerRow` alignment.
pub const ROW_ALIGNMENT: u32 = 256;

/// What a texture is: its dimension, and how many 2D layers it has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum TextureKind {
    #[default]
    D2 = 0,
    D3 = 1,
    /// A 2D array: `layers` 2D textures of one size, sampled by layer.
    Array = 2,
    /// Six square layers, sampled by direction.
    Cube = 3,
}

impl TextureKind {
    pub const ALL: [TextureKind; 4] =
        [TextureKind::D2, TextureKind::D3, TextureKind::Array, TextureKind::Cube];

    pub fn from_u32(v: u32) -> Option<TextureKind> {
        TextureKind::ALL.into_iter().find(|k| *k as u32 == v)
    }

    pub fn name(self) -> &'static str {
        match self {
            TextureKind::D2 => "2d",
            TextureKind::D3 => "3d",
            TextureKind::Array => "2d-array",
            TextureKind::Cube => "cube",
        }
    }
}

/// Which part of a texture a binding or a target holds: its levels from `level` on, `levels` of
/// them, and its layers from `layer` on, `layers` of them. The stream's two words for a binding
/// (see the module doc) give it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct View {
    /// One level, or `None`: all of them.
    pub level: Option<u32>,
    /// One layer (bound as a 2D texture), or `None`: all of them.
    pub layer: Option<u32>,
}

impl View {
    /// A binding's view, from its two words: 0 all, else one more than the one level or layer.
    pub fn of_words(level: u32, layer: u32) -> View {
        let one = |w: u32| w.checked_sub(1);
        View { level: one(level), layer: one(layer) }
    }
}

/// The most texels a side of a 3D texture: WebGPU's default `maxTextureDimension3D`.
pub const MAX_TEXTURE_3D: u32 = 2048;

/// A texture's format: how its texels are stored and sampled. Every capability of each one is
/// in [`TextureFormat::info`], the one table the compiler, both checkers and both hosts read
/// (WebGPU's "Texture Format Capabilities").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum TextureFormat {
    /// Four 8-bit channels, read as floats in [0, 1]: the screen's format.
    Rgba8 = 0,
    /// Four 16-bit floats.
    Rgba16Float = 1,
    /// A 32-bit float depth, for depth testing and comparison sampling.
    Depth32Float = 2,
    /// One 16-bit float.
    R16Float = 3,
    /// Two 16-bit floats.
    Rg16Float = 4,
    /// One 32-bit float, not filtered without `float32-filterable`.
    R32Float = 5,
    /// One 32-bit unsigned integer, never filtered.
    R32Uint = 6,
    /// One 8-bit channel, read as a float in [0, 1].
    R8 = 7,
    /// Two 8-bit channels, read as floats in [0, 1].
    Rg8 = 8,
    /// Red, green and blue of 10 bits and alpha of 2, read as floats in [0, 1].
    Rgb10a2 = 9,
    /// Two 32-bit floats.
    Rg32Float = 10,
    /// Four 32-bit floats.
    Rgba32Float = 11,
    /// Two 32-bit unsigned integers.
    Rg32Uint = 12,
    /// Red and green of 11 bits and blue of 10, unsigned floats: HDR colour in 4 bytes.
    Rg11b10Float = 13,
    /// BC1: 4 × 4 texels of RGB (and 1-bit alpha) in 8 bytes.
    Bc1 = 14,
    /// BC4: 4 × 4 texels of one channel in 8 bytes.
    Bc4 = 15,
    /// BC5: 4 × 4 texels of two channels in 16 bytes (normals).
    Bc5 = 16,
    /// BC7: 4 × 4 texels of RGBA in 16 bytes.
    Bc7 = 17,
    /// ETC2: 4 × 4 texels of RGB in 8 bytes.
    Etc2Rgb8 = 18,
    /// ASTC: 4 × 4 texels of RGBA in 16 bytes.
    Astc4x4 = 19,
}

/// A device feature a format's capability needs (WebGPU's names). A format in WebGPU's core
/// with the capability needs none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FormatFeature {
    Float32Filterable,
    Float32Blendable,
    Rg11b10Renderable,
    TextureCompressionBc,
    TextureCompressionEtc2,
    TextureCompressionAstc,
}

impl FormatFeature {
    /// WebGPU's name for it.
    pub fn name(self) -> &'static str {
        match self {
            FormatFeature::Float32Filterable => "float32-filterable",
            FormatFeature::Float32Blendable => "float32-blendable",
            FormatFeature::Rg11b10Renderable => "rg11b10ufloat-renderable",
            FormatFeature::TextureCompressionBc => "texture-compression-bc",
            FormatFeature::TextureCompressionEtc2 => "texture-compression-etc2",
            FormatFeature::TextureCompressionAstc => "texture-compression-astc",
        }
    }
}

/// A capability a format has: always, never, or with a device feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Can {
    No,
    Yes,
    With(FormatFeature),
}

impl Can {
    /// Whether it has it on a device with `features` (each a WebGPU name).
    pub fn on(self, has: impl Fn(&str) -> bool) -> bool {
        match self {
            Can::No => false,
            Can::Yes => true,
            Can::With(f) => has(f.name()),
        }
    }

    /// Whether it has it on some device.
    pub fn ever(self) -> bool {
        self != Can::No
    }

    /// The feature it needs, if one.
    pub fn feature(self) -> Option<FormatFeature> {
        match self {
            Can::With(f) => Some(f),
            _ => None,
        }
    }
}

/// One row of the format table: what a texture of the format is and can do. The render target
/// cost and alignment are WebGPU's "render target pixel byte cost" and "render target component
/// alignment", which the colour attachment limit (`maxColorAttachmentBytesPerSample`, 32) sums
/// over a pass's targets: not the storage size (`rgba8unorm` costs 8 as an attachment).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatInfo {
    /// WebGPU's name.
    pub name: &'static str,
    /// The bytes of one block: a texel, or a compressed format's 4 × 4 texels.
    pub block_bytes: u32,
    /// A block's side in texels: 1, or 4 for a compressed format.
    pub block: u32,
    /// How many channels GPU code reads (a texel's type: a scalar, `vec2`, `vec3` or `vec4`).
    pub channels: u32,
    pub filterable: Can,
    /// Kernels write it (a storage texture).
    pub storable: Can,
    /// Passes draw into it.
    pub renderable: Can,
    /// Draws blend into it (`Blend` other than `Replace`).
    pub blendable: Can,
    /// Its texels are unsigned integers, read and written as `u32`s.
    pub uint: bool,
    pub depth: bool,
    /// As a colour attachment: its byte cost and the alignment the running total is rounded to
    /// before it's added (0 for a format that is never a colour target).
    pub target_cost: u32,
    pub target_align: u32,
}

impl TextureFormat {
    pub const ALL: [TextureFormat; 20] = [
        TextureFormat::Rgba8,
        TextureFormat::Rgba16Float,
        TextureFormat::Depth32Float,
        TextureFormat::R16Float,
        TextureFormat::Rg16Float,
        TextureFormat::R32Float,
        TextureFormat::R32Uint,
        TextureFormat::R8,
        TextureFormat::Rg8,
        TextureFormat::Rgb10a2,
        TextureFormat::Rg32Float,
        TextureFormat::Rgba32Float,
        TextureFormat::Rg32Uint,
        TextureFormat::Rg11b10Float,
        TextureFormat::Bc1,
        TextureFormat::Bc4,
        TextureFormat::Bc5,
        TextureFormat::Bc7,
        TextureFormat::Etc2Rgb8,
        TextureFormat::Astc4x4,
    ];

    pub fn from_u32(v: u32) -> Option<TextureFormat> {
        TextureFormat::ALL.iter().copied().find(|f| *f as u32 == v)
    }

    /// The format table's row (WebGPU's "Texture Format Capabilities").
    pub const fn info(self) -> FormatInfo {
        use Can::{No, With, Yes};
        use FormatFeature as F;
        // name, block bytes, block side, channels, filter, store, render, blend, uint, depth,
        // target cost, target alignment
        // A row of the table, its columns in order: a struct literal would name each again.
        #[allow(clippy::too_many_arguments)]
        const fn row(
            name: &'static str,
            block_bytes: u32,
            block: u32,
            channels: u32,
            filterable: Can,
            storable: Can,
            renderable: Can,
            blendable: Can,
            target: (u32, u32),
        ) -> FormatInfo {
            FormatInfo {
                name,
                block_bytes,
                block,
                channels,
                filterable,
                storable,
                renderable,
                blendable,
                uint: false,
                depth: false,
                target_cost: target.0,
                target_align: target.1,
            }
        }
        const fn uint(mut i: FormatInfo) -> FormatInfo {
            i.uint = true;
            i
        }
        const fn compressed(
            name: &'static str,
            block_bytes: u32,
            channels: u32,
            f: F,
        ) -> FormatInfo {
            row(name, block_bytes, 4, channels, With(f), No, No, No, (0, 0))
        }
        match self {
            TextureFormat::Rgba8 => row("rgba8unorm", 4, 1, 4, Yes, Yes, Yes, Yes, (8, 1)),
            TextureFormat::Rgba16Float => row("rgba16float", 8, 1, 4, Yes, Yes, Yes, Yes, (8, 2)),
            TextureFormat::Depth32Float => {
                let mut i = row("depth32float", 4, 1, 1, No, No, Yes, No, (0, 0));
                i.depth = true;
                i
            }
            TextureFormat::R16Float => row("r16float", 2, 1, 1, Yes, No, Yes, Yes, (2, 2)),
            TextureFormat::Rg16Float => row("rg16float", 4, 1, 2, Yes, No, Yes, Yes, (4, 2)),
            TextureFormat::R32Float => row(
                "r32float",
                4,
                1,
                1,
                With(F::Float32Filterable),
                Yes,
                Yes,
                With(F::Float32Blendable),
                (4, 4),
            ),
            TextureFormat::R32Uint => uint(row("r32uint", 4, 1, 1, No, Yes, Yes, No, (4, 4))),
            TextureFormat::R8 => row("r8unorm", 1, 1, 1, Yes, No, Yes, Yes, (1, 1)),
            TextureFormat::Rg8 => row("rg8unorm", 2, 1, 2, Yes, No, Yes, Yes, (2, 1)),
            TextureFormat::Rgb10a2 => row("rgb10a2unorm", 4, 1, 4, Yes, No, Yes, Yes, (8, 4)),
            TextureFormat::Rg32Float => row(
                "rg32float",
                8,
                1,
                2,
                With(F::Float32Filterable),
                Yes,
                Yes,
                With(F::Float32Blendable),
                (8, 4),
            ),
            TextureFormat::Rgba32Float => row(
                "rgba32float",
                16,
                1,
                4,
                With(F::Float32Filterable),
                Yes,
                Yes,
                With(F::Float32Blendable),
                (16, 4),
            ),
            TextureFormat::Rg32Uint => uint(row("rg32uint", 8, 1, 2, No, Yes, Yes, No, (8, 4))),
            TextureFormat::Rg11b10Float => row(
                "rg11b10ufloat",
                4,
                1,
                3,
                Yes,
                No,
                With(F::Rg11b10Renderable),
                With(F::Rg11b10Renderable),
                (8, 4),
            ),
            TextureFormat::Bc1 => compressed("bc1-rgba-unorm", 8, 4, F::TextureCompressionBc),
            TextureFormat::Bc4 => compressed("bc4-r-unorm", 8, 1, F::TextureCompressionBc),
            TextureFormat::Bc5 => compressed("bc5-rg-unorm", 16, 2, F::TextureCompressionBc),
            TextureFormat::Bc7 => compressed("bc7-rgba-unorm", 16, 4, F::TextureCompressionBc),
            TextureFormat::Etc2Rgb8 => {
                compressed("etc2-rgb8unorm", 8, 3, F::TextureCompressionEtc2)
            }
            TextureFormat::Astc4x4 => {
                compressed("astc-4x4-unorm", 16, 4, F::TextureCompressionAstc)
            }
        }
    }

    /// WebGPU's name for it.
    pub fn name(self) -> &'static str {
        self.info().name
    }

    /// The bytes of one texel; of a compressed format, of one 4 × 4 block.
    pub fn bytes_per_texel(self) -> u32 {
        self.info().block_bytes
    }

    /// A block's side in texels: 4 for a compressed format, else 1.
    pub fn block(self) -> u32 {
        self.info().block
    }

    pub fn is_compressed(self) -> bool {
        self.info().block > 1
    }

    /// The bytes of a `width` × `height` rectangle of it, whole blocks.
    pub fn bytes_of(self, width: u32, height: u32) -> u64 {
        let b = u64::from(self.block());
        u64::from(width).div_ceil(b)
            * u64::from(height).div_ceil(b)
            * u64::from(self.bytes_per_texel())
    }

    pub fn is_depth(self) -> bool {
        self.info().depth
    }

    /// Whether kernels can write it on some device (a storage texture).
    pub fn storable(self) -> bool {
        self.info().storable.ever()
    }

    /// Whether a shader can filter between its texels on every device (a colour format's sample
    /// type is then `float`, else `unfilterable-float` or `uint`). A format filterable only with
    /// a feature (`r32float` with `float32-filterable`) binds as `unfilterable-float`.
    pub fn filterable(self) -> bool {
        // A compressed texture exists only on a device with its feature, so it always filters.
        self.info().filterable == Can::Yes || self.is_compressed()
    }

    /// Whether a pass can draw into it on some device.
    pub fn renderable(self) -> bool {
        self.info().renderable.ever()
    }

    /// Whether a draw can blend into it on some device.
    pub fn blendable(self) -> bool {
        self.info().blendable.ever()
    }

    /// Whether its texels are integers, read and written as `u32`s.
    pub fn is_uint(self) -> bool {
        self.info().uint
    }
}

/// The colour attachments' bytes per sample of a pass drawing into `formats`, in order, by
/// WebGPU's algorithm ("calculating color attachment bytes per sample"): for each target, the
/// running total is rounded up to the format's component alignment, then its byte cost is
/// added. The order matters: `[rgba32float, rgba8unorm, rgba8unorm, rgba16float]` is 40, and
/// `[rgba16float, rg16float, r8unorm, r32uint]` is 8, 12, 13, then 16 + 4 = 20.
pub fn color_attachment_bytes(formats: &[TextureFormat]) -> u32 {
    formats.iter().fold(0, |total, f| {
        let i = f.info();
        total.next_multiple_of(i.target_align.max(1)) + i.target_cost
    })
}

/// WebGPU's default `maxColorAttachmentBytesPerSample`.
pub const MAX_COLOR_ATTACHMENT_BYTES: u32 = 32;
/// WebGPU's default `maxColorAttachments`.
pub const MAX_COLOR_ATTACHMENTS: usize = 4;

impl serde::Serialize for TextureFormat {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
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

/// One colour target of a pass: a texture's level and layer, or the screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorTarget {
    /// A texture, or [`SCREEN`].
    pub texture: u32,
    pub level: u32,
    pub layer: u32,
    /// Keep what's there rather than clear it.
    pub keep: bool,
    pub clear: [f32; 4],
}

impl ColorTarget {
    /// Texture `texture`'s first level and layer, cleared to `clear` or kept.
    pub fn of(texture: u32, keep: bool, clear: [f32; 4]) -> ColorTarget {
        ColorTarget { texture, level: 0, layer: 0, keep, clear }
    }

    /// Whether it draws into the same texel as `other` (the same texture, level and layer).
    pub fn same_place(&self, other: &ColorTarget) -> bool {
        (self.texture, self.level, self.layer) == (other.texture, other.level, other.layer)
    }
}

/// A pass's attachments (`BeginPass`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pass {
    /// The colour targets: the first `count` of them.
    pub colors: [ColorTarget; MAX_COLOR_ATTACHMENTS],
    pub count: u32,
    /// It may run as part of the pass before it, if that one ended right before it on the
    /// same targets and this one keeps them (the host's choice; serial timing times it alone).
    pub join: bool,
    /// A depth texture, or [`NONE`].
    pub depth: u32,
    /// Its layer (of a depth array; 0 of a depth texture).
    pub depth_layer: u32,
    pub keep_depth: bool,
    pub clear_depth: f32,
}

impl Pass {
    /// A pass with these colour targets (at most [`MAX_COLOR_ATTACHMENTS`]) and no depth.
    pub fn new(colors: &[ColorTarget]) -> Pass {
        assert!(colors.len() <= MAX_COLOR_ATTACHMENTS, "at most four colour targets");
        let mut all = [ColorTarget::of(NONE, false, [0.0; 4]); MAX_COLOR_ATTACHMENTS];
        all[..colors.len()].copy_from_slice(colors);
        Pass {
            colors: all,
            count: colors.len() as u32,
            join: false,
            depth: NONE,
            depth_layer: 0,
            keep_depth: false,
            clear_depth: 1.0,
        }
    }

    /// A pass into one colour target, `texture` (or [`SCREEN`]), or none ([`NONE`]).
    pub fn one(texture: u32, keep: bool, clear: [f32; 4]) -> Pass {
        if texture == NONE {
            return Pass::new(&[]);
        }
        Pass::new(&[ColorTarget::of(texture, keep, clear)])
    }

    /// With `depth` as its depth target.
    pub fn with_depth(self, depth: u32, keep_depth: bool, clear_depth: f32) -> Pass {
        Pass { depth, keep_depth, clear_depth, ..self }
    }

    /// With its colour target `texture` alone ([`NONE`]: none), keeping the first one's load
    /// and clear.
    pub fn with_color(self, texture: u32) -> Pass {
        let first =
            self.colors().first().copied().unwrap_or(ColorTarget::of(NONE, false, [0.0; 4]));
        let one = ColorTarget { texture, ..first };
        let fresh = Pass::new(if texture == NONE { &[] } else { std::slice::from_ref(&one) });
        Pass { colors: fresh.colors, count: fresh.count, ..self }
    }

    /// With every colour target kept (or cleared).
    pub fn keeping_color(mut self, keep: bool) -> Pass {
        for c in &mut self.colors {
            c.keep = keep;
        }
        self
    }

    /// Its colour targets.
    pub fn colors(&self) -> &[ColorTarget] {
        &self.colors[..self.count as usize]
    }

    /// Whether it draws on the screen.
    pub fn on_screen(&self) -> bool {
        self.colors().iter().any(|c| c.texture == SCREEN)
    }

    /// Whether this pass, beginning right after `ended` ended, may run as part of it, as one
    /// render pass: it may join, on the same targets, kept (not cleared), and not the screen.
    pub fn joins(&self, ended: &Pass) -> bool {
        self.join
            && !self.on_screen()
            && self.count == ended.count
            && self.colors().iter().zip(ended.colors()).all(|(a, b)| a.same_place(b) && a.keep)
            && self.depth == ended.depth
            && self.depth_layer == ended.depth_layer
            && (self.depth == NONE || self.keep_depth)
    }
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
        /// Kernels write its texels (a storage texture): a format [`TextureFormat::storable`].
        writable: bool,
        kind: TextureKind,
        /// A 3D texture's depth in texels, an array's layers (6 for a cube), 1 for a 2D texture.
        layers: u32,
        levels: u32,
    },
    WriteTexture {
        handle: u32,
        level: u32,
        layer: u32,
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
        /// Linear between mip levels.
        mip_linear: bool,
        anisotropy: u32,
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
    /// The name of the next pass or dispatch in the host's timings.
    Label {
        name: &'a str,
    },
    Post {
        request: u32,
        url: &'a str,
        body: &'a [u8],
    },
    /// A request of a host service, by its name, with opaque bytes.
    HostRequest {
        request: u32,
        service: &'a str,
        body: &'a [u8],
    },
    /// Another build made ready to start (`std::reload::prepare`).
    Prepare {
        request: u32,
        path: &'a str,
    },
    Unprepare {
        path: &'a str,
    },
    /// Another build to take this program's place after this call (`std::reload::hand_over`).
    HandOver {
        request: u32,
        path: &'a str,
    },
    /// The GPU time of a label's work in a frame (`std::gpu::timed`).
    Timed {
        request: u32,
        label: &'a str,
    },
    CopyTexture {
        source: TexelRect,
        destination: TexelAt,
    },
    CopyTextureToBuffer {
        source: TexelRect,
        buffer: u32,
        offset: u32,
        bytes_per_row: u32,
    },
    CopyBufferToTexture {
        buffer: u32,
        offset: u32,
        bytes_per_row: u32,
        destination: TexelRect,
    },
    SetScissor {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    SetViewport {
        rect: [f32; 4],
        min_depth: f32,
        max_depth: f32,
    },
    DrawIndexed {
        pipeline: u32,
        indices: u32,
        index_offset: u32,
        index_size: u32,
        index_count: u32,
        instances: u32,
        first_index: u32,
        base_vertex: i32,
        first_instance: u32,
        bindings: Vec<Binding>,
        uniforms: &'a [u8],
    },
    /// Up to `count` indirect draws, their arguments one after another from `offset` (with
    /// `indices`, indexed: the index buffer's handle, offset and size); `count_buffer` (or
    /// [`NONE`]) holds how many.
    MultiDrawIndirect {
        pipeline: u32,
        indices: Option<[u32; 3]>,
        arguments: u32,
        offset: u32,
        count: u32,
        count_buffer: u32,
        count_offset: u32,
        bindings: Vec<Binding>,
        uniforms: &'a [u8],
    },
}

/// A rectangle of texels at one level and layer of a texture: a copy's source, or what a copy
/// writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TexelRect {
    pub texture: u32,
    pub level: u32,
    pub layer: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl TexelRect {
    /// From its words in the stream, in [`TexelRect::words`]' order.
    pub fn of_words([texture, level, layer, x, y, width, height]: [u32; 7]) -> TexelRect {
        TexelRect { texture, level, layer, x, y, width, height }
    }

    /// Its words in the stream: the texture, level, layer, x, y, width and height.
    pub fn words(&self) -> [u32; 7] {
        [self.texture, self.level, self.layer, self.x, self.y, self.width, self.height]
    }
}

/// Where a texture-to-texture copy writes: a texture's level and layer, and the corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TexelAt {
    pub texture: u32,
    pub level: u32,
    pub layer: u32,
    pub x: u32,
    pub y: u32,
}

impl TexelAt {
    /// The `width` × `height` rectangle from this corner.
    pub fn with_size(self, width: u32, height: u32) -> TexelRect {
        let TexelAt { texture, level, layer, x, y } = self;
        TexelRect { texture, level, layer, x, y, width, height }
    }
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
            Command::Label { .. } => Opcode::Label,
            Command::Post { .. } => Opcode::Post,
            Command::Prepare { .. } => Opcode::Prepare,
            Command::Unprepare { .. } => Opcode::Unprepare,
            Command::HandOver { .. } => Opcode::HandOver,
            Command::HostRequest { .. } => Opcode::HostRequest,
            Command::Timed { .. } => Opcode::Timed,
            Command::CopyTexture { .. } => Opcode::CopyTexture,
            Command::CopyTextureToBuffer { .. } => Opcode::CopyTextureToBuffer,
            Command::CopyBufferToTexture { .. } => Opcode::CopyBufferToTexture,
            Command::SetScissor { .. } => Opcode::SetScissor,
            Command::SetViewport { .. } => Opcode::SetViewport,
            Command::DrawIndexed { .. } => Opcode::DrawIndexed,
            Command::MultiDrawIndirect { indices: None, .. } => Opcode::MultiDrawIndirect,
            Command::MultiDrawIndirect { indices: Some(_), .. } => Opcode::MultiDrawIndexedIndirect,
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
                exactly(7)?;
                let writable = w(3) & WRITABLE != 0;
                let Some(format) = TextureFormat::from_u32(w(3) & !WRITABLE) else {
                    return Err(bad("unknown texture format"));
                };
                let Some(kind) = TextureKind::from_u32(w(4)) else {
                    return Err(bad("unknown texture kind"));
                };
                let (layers, levels) = (w(5), w(6));
                if w(1) == 0 || w(2) == 0 || layers == 0 {
                    return Err(bad("a texture's width, height and depth or layers are positive"));
                }
                if levels == 0 || levels > MAX_LEVELS {
                    return Err(bad("a texture has 1 to 14 mip levels"));
                }
                if writable && !format.storable() {
                    return Err(bad("kernels write only a format the format table says they can"));
                }
                match kind {
                    TextureKind::D2 if layers != 1 => {
                        return Err(bad("a 2D texture has one layer"));
                    }
                    TextureKind::Cube if layers != 6 || w(1) != w(2) => {
                        return Err(bad("a cube has six square layers"));
                    }
                    TextureKind::D3 if format.is_depth() || format.is_compressed() => {
                        return Err(bad("a 3D texture holds uncompressed colours"));
                    }
                    TextureKind::D3 if levels != 1 => {
                        return Err(bad("a 3D texture has one level"));
                    }
                    _ => {}
                }
                Command::CreateTexture {
                    handle: w(0),
                    width: w(1),
                    height: w(2),
                    format,
                    writable,
                    kind,
                    layers,
                    levels,
                }
            }
            Opcode::WriteTexture => {
                if words < 8 {
                    return Err(bad("expected at least 8 words"));
                }
                let n = w(7) as usize;
                if padded(n) != len - 32 {
                    return Err(bad("data length doesn't match the payload"));
                }
                Command::WriteTexture {
                    handle: w(0),
                    level: w(1),
                    layer: w(2),
                    x: w(3),
                    y: w(4),
                    width: w(5),
                    height: w(6),
                    data: &p[32..32 + n],
                }
            }
            Opcode::CreateSampler => {
                exactly(6)?;
                if w(1) > 1 || w(2) > 1 || w(4) > 1 {
                    return Err(bad("a sampler's filters and address are 0 or 1"));
                }
                if w(5) == 0 || w(5) > MAX_ANISOTROPY {
                    return Err(bad("a sampler's anisotropy is 1 to 16"));
                }
                if w(5) > 1 && (w(1) == 0 || w(4) == 0) {
                    return Err(bad("an anisotropic sampler filters linearly, between levels too"));
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
                    mip_linear: w(4) == 1,
                    anisotropy: w(5),
                }
            }
            Opcode::BeginPass => {
                if words < 2 {
                    return Err(bad("expected at least 2 words"));
                }
                let n = w(0) as usize;
                if n > MAX_COLOR_ATTACHMENTS {
                    return Err(bad("a pass has at most four colour targets"));
                }
                if words != 2 + 8 * n + 4 {
                    return Err(bad("the colour targets don't fill the payload"));
                }
                if w(1) > PASS_JOIN {
                    return Err(bad("the join word is 0 or 1"));
                }
                let mut colors = Vec::with_capacity(n);
                for k in 0..n {
                    let at = 2 + 8 * k;
                    if w(at + 3) > 1 {
                        return Err(bad("a load is 0 (clear) or 1 (keep)"));
                    }
                    colors.push(ColorTarget {
                        texture: w(at),
                        level: w(at + 1),
                        layer: w(at + 2),
                        keep: w(at + 3) == 1,
                        clear: [4, 5, 6, 7].map(|i| f32::from_bits(w(at + i))),
                    });
                }
                if colors.iter().any(|c| c.texture == SCREEN) && n != 1 {
                    return Err(bad("a pass on the screen has the screen as its one colour"));
                }
                let d = 2 + 8 * n;
                if w(d + 2) > 1 {
                    return Err(bad("a load is 0 (clear) or 1 (keep)"));
                }
                let mut pass = Pass::new(&colors);
                pass.join = w(1) == PASS_JOIN;
                pass.depth = w(d);
                pass.depth_layer = w(d + 1);
                pass.keep_depth = w(d + 2) == 1;
                pass.clear_depth = f32::from_bits(w(d + 3));
                Command::BeginPass(pass)
            }
            Opcode::CopyTexture => {
                exactly(12)?;
                Command::CopyTexture {
                    source: TexelRect::of_words([w(0), w(1), w(2), w(3), w(4), w(10), w(11)]),
                    destination: TexelAt {
                        texture: w(5),
                        level: w(6),
                        layer: w(7),
                        x: w(8),
                        y: w(9),
                    },
                }
            }
            Opcode::CopyTextureToBuffer => {
                exactly(10)?;
                if w(8) % ROW_ALIGNMENT != 0 || w(9) % ROW_ALIGNMENT != 0 || w(9) == 0 {
                    return Err(bad("the buffer's offset and bytes per row are multiples of 256"));
                }
                Command::CopyTextureToBuffer {
                    source: TexelRect::of_words(std::array::from_fn(w)),
                    buffer: w(7),
                    offset: w(8),
                    bytes_per_row: w(9),
                }
            }
            Opcode::CopyBufferToTexture => {
                exactly(10)?;
                if w(1) % 4 != 0 || w(2) % ROW_ALIGNMENT != 0 || w(2) == 0 {
                    return Err(bad(
                        "the buffer's offset is a multiple of 4, its bytes per row of 256",
                    ));
                }
                Command::CopyBufferToTexture {
                    buffer: w(0),
                    offset: w(1),
                    bytes_per_row: w(2),
                    destination: TexelRect::of_words(std::array::from_fn(|i| w(3 + i))),
                }
            }
            Opcode::SetScissor => {
                exactly(4)?;
                Command::SetScissor { x: w(0), y: w(1), width: w(2), height: w(3) }
            }
            Opcode::SetViewport => {
                exactly(6)?;
                let f = |i: usize| f32::from_bits(w(i));
                let (rect, min_depth, max_depth) = ([f(0), f(1), f(2), f(3)], f(4), f(5));
                if !rect.iter().all(|x| x.is_finite()) || rect[2] < 0.0 || rect[3] < 0.0 {
                    return Err(bad("a viewport is a finite rectangle of non-negative size"));
                }
                if !(0.0..=1.0).contains(&min_depth)
                    || !(0.0..=1.0).contains(&max_depth)
                    || min_depth > max_depth
                {
                    return Err(bad("a viewport's depths are in [0, 1], the least first"));
                }
                Command::SetViewport { rect, min_depth, max_depth }
            }
            Opcode::DrawIndexed => {
                let (bindings, uniforms) = bindings_and_uniforms(p, 9).map_err(bad)?;
                if w(2) % 4 != 0 || w(3) % 4 != 0 {
                    return Err(bad("the indices' offset and size must be multiples of 4"));
                }
                Command::DrawIndexed {
                    pipeline: w(0),
                    indices: w(1),
                    index_offset: w(2),
                    index_size: w(3),
                    index_count: w(4),
                    instances: w(5),
                    first_index: w(6),
                    base_vertex: w(7) as i32,
                    first_instance: w(8),
                    bindings,
                    uniforms,
                }
            }
            Opcode::MultiDrawIndirect | Opcode::MultiDrawIndexedIndirect => {
                let indexed = opcode == Opcode::MultiDrawIndexedIndirect;
                let at = if indexed { 4 } else { 1 };
                let (bindings, uniforms) = bindings_and_uniforms(p, at + 5).map_err(bad)?;
                if indexed && (w(2) % 4 != 0 || w(3) % 4 != 0) {
                    return Err(bad("the indices' offset and size must be multiples of 4"));
                }
                if w(at + 1) % 4 != 0 || w(at + 4) % 4 != 0 {
                    return Err(bad(
                        "the arguments' and the count's offsets must be multiples of 4",
                    ));
                }
                Command::MultiDrawIndirect {
                    pipeline: w(0),
                    indices: indexed.then(|| [w(1), w(2), w(3)]),
                    arguments: w(at),
                    offset: w(at + 1),
                    count: w(at + 2),
                    count_buffer: w(at + 3),
                    count_offset: w(at + 4),
                    bindings,
                    uniforms,
                }
            }
            Opcode::ReadBuffer => {
                exactly(4)?;
                if w(2) % 4 != 0 || w(3) % 4 != 0 {
                    return Err(bad("offset and size must be multiples of 4"));
                }
                Command::ReadBuffer { request: w(0), handle: w(1), offset: w(2), size: w(3) }
            }
            Opcode::StorageRead
            | Opcode::Fetch
            | Opcode::Prepare
            | Opcode::HandOver
            | Opcode::Timed => {
                if words < 2 {
                    return Err(bad("expected at least 2 words"));
                }
                let n = w(1) as usize;
                if padded(n) != len - 8 {
                    return Err(bad("the text's length doesn't match the payload"));
                }
                let text = text(8, n, "the text isn't UTF-8")?;
                match opcode {
                    Opcode::Fetch => Command::Fetch { request: w(0), url: text },
                    Opcode::Prepare => Command::Prepare { request: w(0), path: text },
                    Opcode::HandOver => Command::HandOver { request: w(0), path: text },
                    Opcode::Timed => Command::Timed { request: w(0), label: text },
                    _ => Command::StorageRead { request: w(0), path: text },
                }
            }
            Opcode::Label | Opcode::Unprepare => {
                if words < 1 {
                    return Err(bad("expected at least 1 word"));
                }
                let n = w(0) as usize;
                if padded(n) != len - 4 {
                    return Err(bad("the text's length doesn't match the payload"));
                }
                let text = text(4, n, "the text isn't UTF-8")?;
                if opcode == Opcode::Unprepare {
                    Command::Unprepare { path: text }
                } else {
                    Command::Label { name: text }
                }
            }
            Opcode::StorageWrite => {
                let (path, data) = two_runs(
                    "the path's and data's lengths don't match the payload",
                    "the path isn't UTF-8",
                )?;
                Command::StorageWrite { request: w(0), path, data }
            }
            Opcode::HostRequest => {
                let (service, body) = two_runs(
                    "the service and body lengths do not match the payload",
                    "the service isn't UTF-8",
                )?;
                Command::HostRequest { request: w(0), service, body }
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
        6 => "expected 6 words",
        7 => "expected 7 words",
        10 => "expected 10 words",
        12 => "expected 12 words",
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
                    Command::BeginPass(p) => p.on_screen(),
                    _ => true,
                };
                self.at = if screen { Place::ScreenPass } else { Place::Pass };
                Ok(())
            }
            (Command::BeginScreenPass { .. } | Command::BeginPass(_), _) => {
                err("a pass is already open")
            }
            (_, at) if op.in_pass() => {
                if at != Place::Outside {
                    Ok(())
                } else if op.is_draw() {
                    err("a draw must come inside a pass")
                } else {
                    err("a scissor or a viewport is set inside a pass")
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
            (_, _) => err("only draws, scissors and viewports can happen inside a pass"),
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
        self.header(op, payload_words.len() * 4 + bytes.len());
        for &w in payload_words {
            self.put(w);
        }
        self.body.extend_from_slice(bytes);
    }

    fn header(&mut self, op: Opcode, payload_len: usize) {
        self.body.reserve(COMMAND_HEADER_LEN + payload_len);
        self.put(op as u32);
        self.put(payload_len as u32);
    }

    /// Writes a binding list directly into the batch, without a temporary word vector.
    fn bound_command(&mut self, op: Opcode, words: &[u32], bindings: &[Binding], uniforms: &[u8]) {
        assert!(uniforms.len().is_multiple_of(4), "payload bytes must be a multiple of 4");
        self.header(op, (words.len() + 2 + 3 * bindings.len()) * 4 + uniforms.len());
        for &word in words {
            self.put(word);
        }
        self.put(bindings.len() as u32);
        for b in bindings {
            self.put(b.handle);
            self.put(b.offset);
            self.put(b.size);
        }
        self.put(uniforms.len() as u32);
        self.body.extend_from_slice(uniforms);
    }

    /// Writes each byte run with its own padding, directly into the batch.
    fn padded_command(&mut self, op: Opcode, words: &[u32], runs: &[&[u8]]) {
        let bytes: usize = runs.iter().map(|run| padded(run.len())).sum();
        self.header(op, words.len() * 4 + bytes);
        for &word in words {
            self.put(word);
        }
        for run in runs {
            self.body.extend_from_slice(run);
            self.body.resize(self.body.len() + padded(run.len()) - run.len(), 0);
        }
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
        let words = [pipeline, groups[0], groups[1], groups[2]];
        self.bound_command(Opcode::Dispatch, &words, bindings, uniforms);
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
        self.bound_command(Opcode::Draw, &[pipeline, vertices, instances], bindings, uniforms);
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

    /// A 2D texture of one level.
    pub fn create_texture(
        &mut self,
        handle: u32,
        width: u32,
        height: u32,
        format: TextureFormat,
        writable: bool,
    ) -> &mut Self {
        self.create_texture_of(handle, [width, height, 1], format, writable, TextureKind::D2, 1)
    }

    /// A 3D texture: `create_texture`'s, with its depth.
    pub fn create_texture_3d(
        &mut self,
        handle: u32,
        width: u32,
        height: u32,
        depth: u32,
        format: TextureFormat,
        writable: bool,
    ) -> &mut Self {
        self.create_texture_of(handle, [width, height, depth], format, writable, TextureKind::D3, 1)
    }

    /// Any texture: `size` is its width, height and depth or layers.
    pub fn create_texture_of(
        &mut self,
        handle: u32,
        size: [u32; 3],
        format: TextureFormat,
        writable: bool,
        kind: TextureKind,
        levels: u32,
    ) -> &mut Self {
        let f = format as u32 | if writable { WRITABLE } else { 0 };
        let words = [handle, size[0], size[1], f, kind as u32, size[2], levels];
        self.command(Opcode::CreateTexture, &words, &[]);
        self
    }

    /// Texels into level 0 and layer 0.
    pub fn write_texture(
        &mut self,
        handle: u32,
        origin: [u32; 2],
        size: [u32; 2],
        data: &[u8],
    ) -> &mut Self {
        self.write_texture_at(handle, 0, 0, origin, size, data)
    }

    pub fn write_texture_at(
        &mut self,
        handle: u32,
        level: u32,
        layer: u32,
        origin: [u32; 2],
        size: [u32; 2],
        data: &[u8],
    ) -> &mut Self {
        let n = data.len() as u32;
        let words = [handle, level, layer, origin[0], origin[1], size[0], size[1], n];
        self.padded_command(Opcode::WriteTexture, &words, &[data]);
        self
    }

    pub fn copy_texture(&mut self, source: TexelRect, destination: TexelAt) -> &mut Self {
        let (s, d) = (source, destination);
        let words = [
            s.texture, s.level, s.layer, s.x, s.y, d.texture, d.level, d.layer, d.x, d.y, s.width,
            s.height,
        ];
        self.command(Opcode::CopyTexture, &words, &[]);
        self
    }

    pub fn copy_texture_to_buffer(
        &mut self,
        s: TexelRect,
        buffer: u32,
        offset: u32,
        bytes_per_row: u32,
    ) -> &mut Self {
        let words = [&s.words()[..], &[buffer, offset, bytes_per_row]].concat();
        self.command(Opcode::CopyTextureToBuffer, &words, &[]);
        self
    }

    pub fn copy_buffer_to_texture(
        &mut self,
        buffer: u32,
        offset: u32,
        bytes_per_row: u32,
        d: TexelRect,
    ) -> &mut Self {
        let words = [&[buffer, offset, bytes_per_row][..], &d.words()].concat();
        self.command(Opcode::CopyBufferToTexture, &words, &[]);
        self
    }

    pub fn set_scissor(&mut self, x: u32, y: u32, width: u32, height: u32) -> &mut Self {
        self.command(Opcode::SetScissor, &[x, y, width, height], &[]);
        self
    }

    pub fn set_viewport(&mut self, rect: [f32; 4], min_depth: f32, max_depth: f32) -> &mut Self {
        let words = [rect[0], rect[1], rect[2], rect[3], min_depth, max_depth].map(f32::to_bits);
        self.command(Opcode::SetViewport, &words, &[]);
        self
    }

    /// `indices` is the index buffer's handle, offset and size in bytes; `counts` the index
    /// count, the instance count, the first index, the base vertex (as its bits) and the first
    /// instance.
    pub fn draw_indexed(
        &mut self,
        pipeline: u32,
        indices: [u32; 3],
        counts: [u32; 5],
        bindings: &[Binding],
        uniforms: &[u8],
    ) -> &mut Self {
        let c = counts;
        let words = [pipeline, indices[0], indices[1], indices[2], c[0], c[1], c[2], c[3], c[4]];
        self.bound_command(Opcode::DrawIndexed, &words, bindings, uniforms);
        self
    }

    /// `arguments` is the arguments buffer's handle and offset; `count` the most draws, then
    /// the count buffer (or [`NONE`]) and its offset.
    pub fn multi_draw_indirect(
        &mut self,
        pipeline: u32,
        indices: Option<[u32; 3]>,
        arguments: [u32; 2],
        count: [u32; 3],
        bindings: &[Binding],
        uniforms: &[u8],
    ) -> &mut Self {
        let mut words = vec![pipeline];
        let op = match indices {
            Some(i) => {
                words.extend(i);
                Opcode::MultiDrawIndexedIndirect
            }
            None => Opcode::MultiDrawIndirect,
        };
        words.extend(arguments);
        words.extend(count);
        self.bound_command(op, &words, bindings, uniforms);
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
        self.create_sampler_with(handle, linear, repeat, compare, false, 1)
    }

    pub fn create_sampler_with(
        &mut self,
        handle: u32,
        linear: bool,
        repeat: bool,
        compare: Option<Compare>,
        mip_linear: bool,
        anisotropy: u32,
    ) -> &mut Self {
        let words = [
            handle,
            u32::from(linear),
            u32::from(repeat),
            compare.map_or(0, |c| c as u32),
            u32::from(mip_linear),
            anisotropy,
        ];
        self.command(Opcode::CreateSampler, &words, &[]);
        self
    }

    pub fn destroy_sampler(&mut self, handle: u32) -> &mut Self {
        self.command(Opcode::DestroySampler, &[handle], &[]);
        self
    }

    pub fn begin_pass(&mut self, pass: Pass) -> &mut Self {
        let mut words = vec![pass.count, if pass.join { PASS_JOIN } else { 0 }];
        for c in pass.colors() {
            words.extend([c.texture, c.level, c.layer, u32::from(c.keep)]);
            words.extend(c.clear.map(f32::to_bits));
        }
        words.extend([
            pass.depth,
            pass.depth_layer,
            u32::from(pass.keep_depth),
            pass.clear_depth.to_bits(),
        ]);
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
        self.bound_command(
            Opcode::DispatchIndirect,
            &[pipeline, arguments, offset],
            bindings,
            uniforms,
        );
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
        self.bound_command(
            Opcode::DrawIndirect,
            &[pipeline, arguments, offset],
            bindings,
            uniforms,
        );
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
        let words = [pipeline, indices[0], indices[1], indices[2], arguments, offset];
        self.bound_command(Opcode::DrawIndexedIndirect, &words, bindings, uniforms);
        self
    }

    pub fn read_buffer(&mut self, request: u32, handle: u32, offset: u32, size: u32) -> &mut Self {
        self.command(Opcode::ReadBuffer, &[request, handle, offset, size], &[]);
        self
    }

    pub fn storage_read(&mut self, request: u32, path: &str) -> &mut Self {
        self.padded_command(Opcode::StorageRead, &[request, path.len() as u32], &[path.as_bytes()]);
        self
    }

    pub fn storage_write(&mut self, request: u32, path: &str, data: &[u8]) -> &mut Self {
        let words = [request, path.len() as u32, data.len() as u32];
        self.padded_command(Opcode::StorageWrite, &words, &[path.as_bytes(), data]);
        self
    }

    pub fn timed(&mut self, request: u32, label: &str) -> &mut Self {
        self.padded_command(Opcode::Timed, &[request, label.len() as u32], &[label.as_bytes()]);
        self
    }

    pub fn fetch(&mut self, request: u32, url: &str) -> &mut Self {
        self.padded_command(Opcode::Fetch, &[request, url.len() as u32], &[url.as_bytes()]);
        self
    }

    pub fn post(&mut self, request: u32, url: &str, body: &[u8]) -> &mut Self {
        let words = [request, url.len() as u32, body.len() as u32];
        self.padded_command(Opcode::Post, &words, &[url.as_bytes(), body]);
        self
    }

    pub fn host_request(&mut self, request: u32, service: &str, body: &[u8]) -> &mut Self {
        let words = [request, service.len() as u32, body.len() as u32];
        self.padded_command(Opcode::HostRequest, &words, &[service.as_bytes(), body]);
        self
    }

    pub fn prepare(&mut self, request: u32, path: &str) -> &mut Self {
        self.padded_command(Opcode::Prepare, &[request, path.len() as u32], &[path.as_bytes()]);
        self
    }

    pub fn unprepare(&mut self, path: &str) -> &mut Self {
        self.padded_command(Opcode::Unprepare, &[path.len() as u32], &[path.as_bytes()]);
        self
    }

    pub fn hand_over(&mut self, request: u32, path: &str) -> &mut Self {
        self.padded_command(Opcode::HandOver, &[request, path.len() as u32], &[path.as_bytes()]);
        self
    }

    pub fn label(&mut self, name: &str) -> &mut Self {
        self.padded_command(Opcode::Label, &[name.len() as u32], &[name.as_bytes()]);
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
