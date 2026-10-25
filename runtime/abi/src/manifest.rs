//! # The manifest, version 10
//!
//! `manifest.json` describes everything game-specific a host needs besides the WASM: each
//! pipeline's shader, entry points and bindings (D-099). The runtime reads it at load; nothing
//! is generated from it per game.
//!
//! - All bindings are in bind group 0.
//! - A pipeline's **uniform block** holds every small `GpuData` argument of its entry points,
//!   packed in parameter order by WGSL's layout rules. It's a `uniform` binding when its layout
//!   satisfies WGSL's uniform rules, else a read-only `storage` binding (same bytes either way).
//! - Its other **bindings** are, in parameter order, what its entry points take besides
//!   uniforms and builtins: buffers for `[T]` (read) and `Slots<T>` (read-write) parameters,
//!   textures, depth textures, samplers and comparison samplers. A dispatch or draw lists one
//!   binding of the command stream for each, in the same order. A render pipeline's binding
//!   that only one of its shaders reads says which (`"stage"`: `"vertex"` or `"fragment"`);
//!   one without is visible to both. WebGPU limits the storage buffers each stage sees, so a
//!   pipeline's vertex and fragment shaders may bind up to that many each.
//! - A debug build's pipeline may have a **debug flag**: a read-write storage buffer of one
//!   `atomic<u32>` at the binding `debug_flag` names, which no command lists. The host makes
//!   one 4-byte buffer for the program, binds it there in every dispatch and draw, and reads it
//!   after each frame: a pipeline that indexed an array out of range stored its number in the
//!   manifest's list, from 1 (the largest such number stays).
//! - A render pipeline draws a triangle list into its pass's colour target: the screen, whose
//!   format is [`SCREEN_FORMAT`] without sRGB encoding (so hosts produce the same bytes), or a
//!   texture. A host may draw the screen in another 8-bit unorm format that holds the same
//!   bytes (a browser's canvas in the device's preferred format, `bgra8unorm` on some), so a
//!   screen target says it is one. With a depth target, a fragment is kept where its depth is less than what's
//!   there, which it replaces, unless its `"depth"` says otherwise: `"compare"`, how its depth
//!   compares with what's there for it to be kept (WebGPU's names, `"less"` by default), and
//!   `"write"`, whether it replaces it (`true` by default). A render pipeline's `"targets"`
//!   are every combination of targets the program draws it into, as the compiler knows them
//!   from each draw's pass, at least one: each its `"colors"`, whether there's a depth target
//!   (`"depth"`, `depth32float`), and whether it's the screen (`"screen": true`). Hosts make one
//!   pipeline for each when the program loads, and refuse a draw into any other. A target's
//!   `"colors"` are its colour formats in order (none in a pass that draws only depths; one,
//!   [`SCREEN_FORMAT`], for the screen), up to four, within WebGPU's 32 bytes per sample
//!   ([`crate::stream::color_attachment_bytes`]). A render pipeline's
//!   `"outputs"` say what its fragment shader writes to each colour location, `"float"` or
//!   `"uint"`, so a target's colours are as many, an integer format exactly where it's `"uint"`.
//!   Its `"blends"` (one per output; all `"replace"` when it's left out) say how each colour
//!   meets what's in its target ([`Blend`]). One with `"writes_depth": true` gives each fragment
//!   its own depth (WGSL's `frag_depth`), so its targets all have depth.
//! - A pipeline's `"requires"` lists the device features its shader needs (WebGPU's names:
//!   `"shader-f16"`, `"subgroups"`): a host builds it only on a device with them, and a command
//!   that records it on one without is an error that names the feature.
//! - A render pipeline's `"cull"` drops triangles by facing (`"none"`, the default, `"front"`
//!   or `"back"`; a triangle whose vertices run counter-clockwise on the target faces the
//!   front). Its `"depth_bias"` moves each fragment's depth by `constant` steps of the depth
//!   format and `slope_scale` times the triangle's depth slope, at most `clamp` (when not 0)
//!   in size: WebGPU's `depthBias`, `depthBiasSlopeScale` and `depthBiasClamp`. None of these
//!   may be given at run time, so a program states them where it draws (language.md §18).

use crate::Limits;
pub use crate::stream::Compare;
use crate::stream::{MAX_COLOR_ATTACHMENTS, TextureFormat, TextureKind};
use serde::Serialize;
use serde_json::{Map, Value};
use std::fmt;

pub const VERSION: u32 = 10;
/// The screen's texture format: what a screen target's colour is, and the format of the bytes a
/// host reads back from the screen.
pub const SCREEN_TEXTURE: TextureFormat = TextureFormat::Rgba8;
/// [`SCREEN_TEXTURE`] in WebGPU's spelling.
pub const SCREEN_FORMAT: &str = SCREEN_TEXTURE.info().name;
/// WebGPU's default limits that the manifest is checked against ([`Limits::DEFAULT`]).
pub const MAX_WORKGROUP_SIZE: [u32; 3] = Limits::DEFAULT.max_workgroup_size;
pub const MAX_WORKGROUP_INVOCATIONS: u32 = Limits::DEFAULT.max_workgroup_invocations;
pub const MAX_STORAGE_BUFFERS_PER_STAGE: usize =
    Limits::DEFAULT.max_storage_buffers_per_stage as usize;
pub const MAX_UNIFORM_BUFFER_BINDING_SIZE: u32 = Limits::DEFAULT.max_uniform_buffer_binding_size;
/// WebGPU's default `maxBindingsPerBindGroup`: every binding number is below this.
pub const MAX_BINDINGS_PER_GROUP: u32 = 1000;
/// WebGPU's default limits on what one shader stage binds (`maxSampledTexturesPerShaderStage`,
/// `maxSamplersPerShaderStage`, `maxStorageTexturesPerShaderStage`).
pub const MAX_SAMPLED_TEXTURES_PER_STAGE: usize = 16;
pub const MAX_SAMPLERS_PER_STAGE: usize = 16;
pub const MAX_STORAGE_TEXTURES_PER_STAGE: usize = 4;

/// What a binding counts against in its shader stage: one of WebGPU's per-stage limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageLimit {
    StorageBuffers,
    SampledTextures,
    Samplers,
    StorageTextures,
}

impl StageLimit {
    pub const ALL: [StageLimit; 4] = [
        StageLimit::StorageBuffers,
        StageLimit::SampledTextures,
        StageLimit::Samplers,
        StageLimit::StorageTextures,
    ];

    /// What it counts, as a manifest's error names it.
    pub fn name(self) -> &'static str {
        match self {
            StageLimit::StorageBuffers => "storage buffers",
            StageLimit::SampledTextures => "sampled textures",
            StageLimit::Samplers => "samplers",
            StageLimit::StorageTextures => "storage textures",
        }
    }

    /// The most a stage may have: WebGPU's default.
    pub fn max(self) -> usize {
        match self {
            StageLimit::StorageBuffers => MAX_STORAGE_BUFFERS_PER_STAGE,
            StageLimit::SampledTextures => MAX_SAMPLED_TEXTURES_PER_STAGE,
            StageLimit::Samplers => MAX_SAMPLERS_PER_STAGE,
            StageLimit::StorageTextures => MAX_STORAGE_TEXTURES_PER_STAGE,
        }
    }
}

/// Whether a file the manifest names is one in the build directory, as both hosts read it: a
/// name of letters, digits, `_`, `-` and `.`, not starting with `.`. Not a path that leaves
/// the directory (`/`, `..`), nor anything a browser would read as a URL (`:`, `#`, `?`, `\`).
pub fn plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Manifest {
    pub manifest_version: u32,
    pub stream_version: u32,
    /// The program's WASM file, relative to the manifest.
    pub wasm: String,
    pub pipelines: Vec<Pipeline>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Pipeline {
    /// For diagnostics: the entry points' names.
    pub name: String,
    /// The WGSL file, relative to the manifest.
    pub shader: String,
    #[serde(flatten)]
    pub stage: Stage,
    pub uniform: Option<UniformBlock>,
    pub bindings: Vec<ResourceBinding>,
    /// A debug build's: the binding of the flag its bounds checks set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug_flag: Option<u32>,
    /// The device features its shader needs (WebGPU's names).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Stage {
    Compute {
        entry: String,
        workgroup_size: [u32; 3],
    },
    Render {
        vertex_entry: String,
        fragment_entry: String,
        /// How each colour output meets its target's: none means all replace it.
        #[serde(skip_serializing_if = "Blend::all_replace")]
        blends: Vec<Blend>,
        #[serde(skip_serializing_if = "Cull::is_none")]
        cull: Cull,
        #[serde(skip_serializing_if = "DepthBias::is_none")]
        depth_bias: DepthBias,
        #[serde(skip_serializing_if = "DepthState::is_default")]
        depth: DepthState,
        /// Gives each fragment its own depth: drawn only in a pass with a depth target.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        writes_depth: bool,
        /// What its fragment shader writes to each colour location.
        outputs: Vec<Output>,
        /// Every combination of targets it's drawn into, first drawn first.
        targets: Vec<RenderTarget>,
    },
}

/// What a render pipeline draws into: its colour targets' formats in order (none in a pass
/// that draws only depths), and a depth target (`depth32float`) or none. The screen's says it's
/// the screen (`"screen": true`, which a texture's leaves out): its one colour is
/// [`SCREEN_FORMAT`] here, and a host may draw it in another (a browser's canvas).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RenderTarget {
    /// The colours: those before the first `None`.
    pub colors: [Option<TextureFormat>; MAX_COLOR_ATTACHMENTS],
    pub depth: bool,
    pub screen: bool,
}

impl RenderTarget {
    /// The screen's target, with depth or without.
    pub fn screen(depth: bool) -> RenderTarget {
        RenderTarget { screen: true, ..RenderTarget::of(&[SCREEN_TEXTURE], depth) }
    }

    /// Textures of these formats, with depth or without.
    pub fn of(colors: &[TextureFormat], depth: bool) -> RenderTarget {
        let mut all = [None; MAX_COLOR_ATTACHMENTS];
        for (slot, &f) in all.iter_mut().zip(colors) {
            *slot = Some(f);
        }
        RenderTarget { colors: all, depth, screen: false }
    }

    /// Its colour formats.
    pub fn colors(&self) -> impl Iterator<Item = TextureFormat> + '_ {
        self.colors.iter().map_while(|c| *c)
    }

    /// Its first colour's format, if it has one.
    pub fn color(&self) -> Option<TextureFormat> {
        self.colors().next()
    }
}

impl fmt::Display for RenderTarget {
    /// As errors name it: `rgba16float with depth`, `rgba16float and r32uint`, `depth alone`,
    /// `the screen`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.screen {
            return write!(
                f,
                "{}",
                if self.depth { "the screen with depth" } else { "the screen" }
            );
        }
        let names: Vec<&str> = self.colors().map(TextureFormat::name).collect();
        match (names.is_empty(), self.depth) {
            (false, true) => write!(f, "{} with depth", names.join(" and ")),
            (false, false) => write!(f, "{}", names.join(" and ")),
            (true, true) => write!(f, "depth alone"),
            (true, false) => write!(f, "nothing"),
        }
    }
}

impl Serialize for RenderTarget {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut o = s.serialize_struct("RenderTarget", 2 + usize::from(self.screen))?;
        let colors: Vec<&str> = self.colors().map(TextureFormat::name).collect();
        o.serialize_field("colors", &colors)?;
        o.serialize_field("depth", &self.depth)?;
        if self.screen {
            o.serialize_field("screen", &true)?;
        }
        o.end()
    }
}

/// How a fragment shader's colour meets its target's (WebGPU's `GPUBlendState`): the colour
/// and alpha components' factors and operations are [`Blend::components`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Blend {
    /// It replaces what's there.
    #[default]
    Replace,
    /// Straight alpha: colour `src × src.a + dst × (1 − src.a)`, alpha `src.a + dst.a × (1 −
    /// src.a)`.
    Over,
    /// Premultiplied alpha: colour `src + dst × (1 − src.a)`, alpha likewise.
    Premultiplied,
    /// `src + dst`, each component.
    Add,
    /// `src × dst`, each component.
    Multiply,
    /// The least of `src` and `dst`, each component.
    Min,
    /// The greatest, each component.
    Max,
}

/// One component of a blend: (source factor, destination factor, operation), WebGPU's names.
pub type BlendComponent = (&'static str, &'static str, &'static str);

impl Blend {
    pub const ALL: [Blend; 7] = [
        Blend::Replace,
        Blend::Over,
        Blend::Premultiplied,
        Blend::Add,
        Blend::Multiply,
        Blend::Min,
        Blend::Max,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Blend::Replace => "replace",
            Blend::Over => "over",
            Blend::Premultiplied => "premultiplied",
            Blend::Add => "add",
            Blend::Multiply => "multiply",
            Blend::Min => "min",
            Blend::Max => "max",
        }
    }

    /// Its colour and alpha components, as WebGPU's `GPUBlendComponent`s; `None` for `Replace`,
    /// which blends nothing.
    pub fn components(self) -> Option<(BlendComponent, BlendComponent)> {
        Some(match self {
            Blend::Replace => return None,
            Blend::Over => {
                (("src-alpha", "one-minus-src-alpha", "add"), ("one", "one-minus-src-alpha", "add"))
            }
            Blend::Premultiplied => {
                let c = ("one", "one-minus-src-alpha", "add");
                (c, c)
            }
            Blend::Add => (("one", "one", "add"), ("one", "one", "add")),
            Blend::Multiply => (("dst", "zero", "add"), ("dst-alpha", "zero", "add")),
            Blend::Min => (("one", "one", "min"), ("one", "one", "min")),
            Blend::Max => (("one", "one", "max"), ("one", "one", "max")),
        })
    }

    /// Whether every one of `blends` replaces: the manifest leaves them out.
    pub fn all_replace(blends: &[Blend]) -> bool {
        blends.iter().all(|&b| b == Blend::Replace)
    }
}

/// What a fragment shader writes to one colour location: floats, or unsigned integers (to an
/// integer format's target).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    #[default]
    Float,
    Uint,
}

/// How a render pipeline tests and writes depth, in a pass with a depth target: WebGPU's
/// `depthCompare` and `depthWriteEnabled`. The default keeps the nearest fragment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DepthState {
    pub compare: Compare,
    pub write: bool,
}

impl Default for DepthState {
    fn default() -> DepthState {
        DepthState { compare: Compare::Less, write: true }
    }
}

impl DepthState {
    pub fn is_default(&self) -> bool {
        *self == DepthState::default()
    }
}

impl Serialize for DepthState {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut o = s.serialize_struct("DepthState", 2)?;
        o.serialize_field("compare", self.compare.name())?;
        o.serialize_field("write", &self.write)?;
        o.end()
    }
}

/// Which triangles a render pipeline drops by facing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cull {
    #[default]
    None,
    Front,
    Back,
}

impl Cull {
    pub fn is_none(&self) -> bool {
        *self == Cull::None
    }
}

/// How far a render pipeline moves each fragment's depth (WebGPU's `depthBias`,
/// `depthBiasSlopeScale` and `depthBiasClamp`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct DepthBias {
    pub constant: i32,
    pub slope_scale: f32,
    pub clamp: f32,
}

impl DepthBias {
    pub fn is_none(&self) -> bool {
        *self == DepthBias::default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UniformSpace {
    Uniform,
    Storage,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UniformBlock {
    pub binding: u32,
    /// Bytes; the uniform bytes of every dispatch or draw of this pipeline have this length.
    pub size: u32,
    pub space: UniformSpace,
}

/// What a pipeline binds at one binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingKind {
    /// A storage buffer the pipeline reads.
    Read,
    /// A storage buffer it reads and writes.
    ReadWrite,
    /// A colour texture (its `format` says what its texels are read as).
    Texture,
    /// A depth texture, for comparison sampling.
    DepthTexture,
    /// A filtering sampler.
    Sampler,
    /// A comparison sampler.
    ComparisonSampler,
    /// A colour texture a kernel writes, of its `format` (a storage texture, written only).
    StorageTexture,
    /// A colour 3D texture.
    #[serde(rename = "texture_3d")]
    Texture3d,
    /// A colour 3D texture a kernel writes.
    #[serde(rename = "storage_texture_3d")]
    StorageTexture3d,
    /// A 2D array of colour textures, sampled by layer.
    #[serde(rename = "texture_2d_array")]
    TextureArray,
    /// A cube of colour textures, sampled by direction.
    TextureCube,
    /// A 2D array of depth textures, for comparison sampling by layer.
    #[serde(rename = "depth_texture_2d_array")]
    DepthTextureArray,
    /// A 2D array a kernel writes, each invocation its own layer's texel.
    #[serde(rename = "storage_texture_2d_array")]
    StorageTextureArray,
}

impl BindingKind {
    pub const ALL: [BindingKind; 13] = [
        BindingKind::Read,
        BindingKind::ReadWrite,
        BindingKind::Texture,
        BindingKind::DepthTexture,
        BindingKind::Sampler,
        BindingKind::ComparisonSampler,
        BindingKind::StorageTexture,
        BindingKind::Texture3d,
        BindingKind::StorageTexture3d,
        BindingKind::TextureArray,
        BindingKind::TextureCube,
        BindingKind::DepthTextureArray,
        BindingKind::StorageTextureArray,
    ];

    pub fn is_buffer(self) -> bool {
        matches!(self, BindingKind::Read | BindingKind::ReadWrite)
    }

    /// Whether it's a colour texture, which names its format (`ResourceBinding::format`).
    pub fn has_format(self) -> bool {
        matches!(
            self,
            BindingKind::Texture
                | BindingKind::Texture3d
                | BindingKind::StorageTexture
                | BindingKind::StorageTexture3d
                | BindingKind::TextureArray
                | BindingKind::TextureCube
                | BindingKind::StorageTextureArray
        )
    }

    /// Whether kernels write it: a storage texture.
    pub fn is_storage(self) -> bool {
        matches!(
            self,
            BindingKind::StorageTexture
                | BindingKind::StorageTexture3d
                | BindingKind::StorageTextureArray
        )
    }

    /// Whether it's a texture of depths.
    pub fn is_depth(self) -> bool {
        matches!(self, BindingKind::DepthTexture | BindingKind::DepthTextureArray)
    }

    /// A texture binding's view dimension, as the kind of texture it views; `None` for the rest.
    pub fn view_kind(self) -> Option<TextureKind> {
        Some(match self {
            BindingKind::Texture | BindingKind::DepthTexture | BindingKind::StorageTexture => {
                TextureKind::D2
            }
            BindingKind::Texture3d | BindingKind::StorageTexture3d => TextureKind::D3,
            BindingKind::TextureArray
            | BindingKind::DepthTextureArray
            | BindingKind::StorageTextureArray => TextureKind::Array,
            BindingKind::TextureCube => TextureKind::Cube,
            _ => return None,
        })
    }

    /// A texture binding's view dimension (WebGPU's `viewDimension`); `None` for the rest.
    pub fn dimension(self) -> Option<&'static str> {
        self.view_kind().map(TextureKind::name)
    }

    /// The kinds of texture a binding of it may be given: a 2D view of any texture but a 3D
    /// one (one layer of an array or a cube), an array's or a cube's whole layers, a 3D one.
    pub fn takes(self, kind: TextureKind, one_layer: bool) -> bool {
        match self.view_kind() {
            Some(TextureKind::D2) => {
                kind == TextureKind::D2 || (one_layer && kind != TextureKind::D3)
            }
            Some(TextureKind::Array) => {
                !one_layer && matches!(kind, TextureKind::Array | TextureKind::Cube)
            }
            Some(TextureKind::Cube) => !one_layer && kind == TextureKind::Cube,
            Some(TextureKind::D3) => kind == TextureKind::D3,
            None => false,
        }
    }

    /// The per-stage limit it counts against.
    pub fn limit(self) -> StageLimit {
        match self {
            BindingKind::Read | BindingKind::ReadWrite => StageLimit::StorageBuffers,
            BindingKind::Texture
            | BindingKind::DepthTexture
            | BindingKind::Texture3d
            | BindingKind::TextureArray
            | BindingKind::TextureCube
            | BindingKind::DepthTextureArray => StageLimit::SampledTextures,
            BindingKind::Sampler | BindingKind::ComparisonSampler => StageLimit::Samplers,
            BindingKind::StorageTexture
            | BindingKind::StorageTexture3d
            | BindingKind::StorageTextureArray => StageLimit::StorageTextures,
        }
    }

    /// The name the manifest gives it.
    pub fn name(self) -> &'static str {
        match self {
            BindingKind::Read => "read",
            BindingKind::ReadWrite => "read_write",
            BindingKind::Texture => "texture",
            BindingKind::DepthTexture => "depth_texture",
            BindingKind::Sampler => "sampler",
            BindingKind::ComparisonSampler => "comparison_sampler",
            BindingKind::StorageTexture => "storage_texture",
            BindingKind::Texture3d => "texture_3d",
            BindingKind::StorageTexture3d => "storage_texture_3d",
            BindingKind::TextureArray => "texture_2d_array",
            BindingKind::TextureCube => "texture_cube",
            BindingKind::DepthTextureArray => "depth_texture_2d_array",
            BindingKind::StorageTextureArray => "storage_texture_2d_array",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResourceBinding {
    pub binding: u32,
    pub kind: BindingKind,
    /// Which of a render pipeline's shaders reads it; both, if not given.
    #[serde(skip_serializing_if = "BindingStage::is_both")]
    pub stage: BindingStage,
    /// A colour texture's format (`texture`, `texture_3d`, `texture_2d_array`, `texture_cube`,
    /// `storage_texture`, `storage_texture_3d`, `storage_texture_2d_array`: those whose kind
    /// [`BindingKind::has_format`]): what its texels are read as, and a storage texture's format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<TextureFormat>,
}

/// The shaders of a pipeline that see a binding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingStage {
    /// Every shader of the pipeline: its kernel, or its vertex and fragment shaders.
    #[default]
    Both,
    Vertex,
    Fragment,
}

impl BindingStage {
    pub fn is_both(&self) -> bool {
        *self == BindingStage::Both
    }

    /// Whether a shader of `stage` sees it.
    pub fn sees(self, stage: BindingStage) -> bool {
        self == BindingStage::Both || self == stage
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestError(pub String);

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid manifest: {}", self.0)
    }
}

impl std::error::Error for ManifestError {}

impl Manifest {
    pub fn new(wasm: impl Into<String>) -> Manifest {
        Manifest {
            manifest_version: VERSION,
            stream_version: crate::stream::VERSION,
            wasm: wasm.into(),
            pipelines: Vec::new(),
        }
    }

    /// Parses and validates a manifest; any other version is rejected.
    pub fn parse(json: &str) -> Result<Manifest, ManifestError> {
        let v: Value = serde_json::from_str(json).map_err(|e| ManifestError(e.to_string()))?;
        match v.get("manifest_version") {
            Some(got) if got.as_f64() == Some(f64::from(VERSION)) => {}
            Some(got) => {
                return Err(ManifestError(format!(
                    "manifest version {}, but this host reads version {VERSION}",
                    read::show(got)
                )));
            }
            None => {
                return Err(ManifestError(format!(
                    "no manifest_version; this host reads version {VERSION}"
                )));
            }
        }
        let m = read::manifest(&v)?;
        m.validate()?;
        Ok(m)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        let err = |s: String| Err(ManifestError(s));
        if self.manifest_version != VERSION {
            return err(format!("manifest version {}, expected {VERSION}", self.manifest_version));
        }
        if self.stream_version != crate::stream::VERSION {
            return err(format!(
                "command stream version {}, but this host reads {}",
                self.stream_version,
                crate::stream::VERSION
            ));
        }
        if self.wasm.is_empty() {
            return err("no WASM file".into());
        }
        if !plain_file_name(&self.wasm) {
            return err("the WASM file's name isn't a file name in the build directory".into());
        }
        for (i, p) in self.pipelines.iter().enumerate() {
            if p.shader.is_empty() {
                return err(format!("pipeline {i} has no shader"));
            }
            if !plain_file_name(&p.shader) {
                return err(format!(
                    "pipeline {i}'s shader name isn't a file name in the build directory"
                ));
            }
            let mut bindings: Vec<u32> = p.bindings.iter().map(|b| b.binding).collect();
            if let Some(u) = &p.uniform {
                if u.size == 0 || u.size % 4 != 0 {
                    return err(format!(
                        "pipeline {i}'s uniform size {} isn't a positive multiple of 4",
                        u.size
                    ));
                }
                if u.space == UniformSpace::Uniform && u.size % 16 != 0 {
                    return err(format!(
                        "pipeline {i}'s uniform block of {} bytes isn't a multiple of 16",
                        u.size
                    ));
                }
                if u.space == UniformSpace::Uniform && u.size > MAX_UNIFORM_BUFFER_BINDING_SIZE {
                    return err(format!(
                        "pipeline {i}'s uniform block of {} bytes is over WebGPU's default \
                         limit of {MAX_UNIFORM_BUFFER_BINDING_SIZE}",
                        u.size
                    ));
                }
                bindings.push(u.binding);
            }
            bindings.extend(p.debug_flag);
            let n = bindings.len();
            bindings.sort_unstable();
            bindings.dedup();
            if bindings.len() != n {
                return err(format!("pipeline {i} uses a binding twice"));
            }
            if let Some(binding) =
                bindings.iter().find(|&&binding| binding >= MAX_BINDINGS_PER_GROUP)
            {
                return err(format!(
                    "pipeline {i} uses binding {binding}; WebGPU's default limit is {MAX_BINDINGS_PER_GROUP}"
                ));
            }
            let render = matches!(p.stage, Stage::Render { .. });
            for b in &p.bindings {
                if !render && !b.stage.is_both() {
                    return err(format!(
                        "pipeline {i}'s binding {} names a stage, but it's a kernel's",
                        b.binding
                    ));
                }
                if render && b.kind.is_storage() {
                    return err(format!(
                        "pipeline {i}'s binding {} is a storage texture, which only a kernel can bind",
                        b.binding
                    ));
                }
                if b.stage == BindingStage::Vertex && b.kind == BindingKind::ReadWrite {
                    return err(format!(
                        "pipeline {i}'s binding {} is written, and a vertex shader can't write",
                        b.binding
                    ));
                }
            }
            // What each stage binds, against WebGPU's per-stage limits: its bindings, and among
            // its storage buffers the uniform block's when it's in storage, and the debug flag
            // (a fragment shader's or a kernel's).
            let in_storage =
                usize::from(p.uniform.as_ref().is_some_and(|u| u.space == UniformSpace::Storage));
            let stages: &[BindingStage] = if render {
                &[BindingStage::Vertex, BindingStage::Fragment]
            } else {
                &[BindingStage::Both]
            };
            for &stage in stages {
                for limit in StageLimit::ALL {
                    let mut n = p
                        .bindings
                        .iter()
                        .filter(|b| b.kind.limit() == limit && b.stage.sees(stage))
                        .count();
                    if limit == StageLimit::StorageBuffers {
                        n += in_storage
                            + usize::from(p.debug_flag.is_some() && stage != BindingStage::Vertex);
                    }
                    if n > limit.max() {
                        let what = match stage {
                            BindingStage::Vertex => " in its vertex shader",
                            BindingStage::Fragment => " in its fragment shader",
                            BindingStage::Both => "",
                        };
                        return err(format!(
                            "pipeline {i} has {n} {}{what}; WebGPU's default limit is {}",
                            limit.name(),
                            limit.max()
                        ));
                    }
                }
            }
            match &p.stage {
                Stage::Compute { entry, workgroup_size } => {
                    if entry.is_empty() {
                        return err(format!("pipeline {i} has no entry point"));
                    }
                    let ok = workgroup_size
                        .iter()
                        .zip(MAX_WORKGROUP_SIZE)
                        .all(|(&s, m)| s >= 1 && s <= m)
                        && workgroup_size.iter().map(|&x| u64::from(x)).product::<u64>()
                            <= u64::from(MAX_WORKGROUP_INVOCATIONS);
                    if !ok {
                        return err(format!(
                            "pipeline {i}'s workgroup size {workgroup_size:?} is outside WebGPU's limits"
                        ));
                    }
                }
                Stage::Render {
                    vertex_entry,
                    fragment_entry,
                    writes_depth,
                    outputs,
                    blends,
                    targets,
                    ..
                } => {
                    if outputs.len() > MAX_COLOR_ATTACHMENTS {
                        return err(format!("pipeline {i} has more than four colour outputs"));
                    }
                    if !blends.is_empty() && blends.len() != outputs.len() {
                        return err(format!(
                            "pipeline {i} has {} blends for {} colour outputs",
                            blends.len(),
                            outputs.len()
                        ));
                    }
                    if vertex_entry.is_empty() || fragment_entry.is_empty() {
                        return err(format!("pipeline {i} is missing an entry point"));
                    }
                    if targets.is_empty() {
                        return err(format!("pipeline {i} names no targets"));
                    }
                    for (k, t) in targets.iter().enumerate() {
                        if targets[..k].contains(t) {
                            return err(format!("pipeline {i} names the target {t} twice"));
                        }
                        if t.colors[0].is_none() && !t.depth {
                            return err(format!(
                                "pipeline {i}'s target {k} draws into nothing: a target has a colour, a depth, or both"
                            ));
                        }
                        // Its colours are those before the first `None`: none may follow one.
                        if t.colors.iter().skip_while(|c| c.is_some()).any(Option::is_some) {
                            return err(format!(
                                "pipeline {i}'s target {k} has a colour after a missing one: its colours come first"
                            ));
                        }
                        if t.screen
                            && (t.colors().count() != 1 || t.color() != Some(SCREEN_TEXTURE))
                        {
                            return err(format!(
                                "pipeline {i}'s target {k} is the screen, whose one colour is {SCREEN_FORMAT}"
                            ));
                        }
                        let colors: Vec<TextureFormat> = t.colors().collect();
                        let bytes = crate::stream::color_attachment_bytes(&colors);
                        let blend = |c: usize| blends.get(c).copied().unwrap_or_default();
                        let why = if *writes_depth && !t.depth {
                            Some(
                                "its fragments give their depth, so a target has depth".to_string(),
                            )
                        } else if !colors.is_empty() && colors.len() != outputs.len() {
                            Some(format!(
                                "its fragment shader writes {} colours, and it has {}",
                                outputs.len(),
                                colors.len()
                            ))
                        } else if bytes > crate::stream::MAX_COLOR_ATTACHMENT_BYTES {
                            Some(format!(
                                "its colours take {bytes} bytes a sample, over WebGPU's 32"
                            ))
                        } else {
                            colors.iter().enumerate().find_map(|(c, f)| {
                                let uint = outputs[c] == Output::Uint;
                                if f.is_depth() || f.is_compressed() || !f.renderable() {
                                    Some(format!("a pass can't draw into {}", f.name()))
                                } else if uint != f.is_uint() {
                                    Some(format!(
                                        "its fragment shader writes {} to colour {c}, and {} holds {}",
                                        if uint { "integers" } else { "floats" },
                                        f.name(),
                                        if f.is_uint() { "integers" } else { "floats" }
                                    ))
                                } else if blend(c) != Blend::Replace && !f.blendable() {
                                    Some(format!("colour {c} blends, and {} doesn't blend", f.name()))
                                } else {
                                    None
                                }
                            })
                        };
                        if let Some(why) = why {
                            return err(format!("pipeline {i}'s target {t} can't be one: {why}"));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Stable, pretty JSON with a trailing newline (builds are byte-reproducible).
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).unwrap_or_default();
        s.push('\n');
        s
    }
}

/// Reads a manifest from JSON one field at a time, in the order and words of the browser
/// runtime's `src/manifest.ts`, so both hosts accept the same files. (serde's derive would also
/// accept other shapes: a struct as an array, an enum tag as a number.)
mod read {
    use super::*;

    type Result<T> = std::result::Result<T, ManifestError>;
    type Json = Map<String, Value>;

    fn fail<T>(why: String) -> Result<T> {
        Err(ManifestError(why))
    }

    /// A JSON value as both hosts show it: a scalar as JavaScript writes it, else its kind.
    pub(super) fn show(v: &Value) -> String {
        match v {
            Value::Array(_) => "an array".into(),
            Value::Object(_) => "an object".into(),
            Value::Number(n) => js_number(n.as_f64().unwrap_or(f64::NAN)),
            _ => v.to_string(),
        }
    }

    /// `String(x)` in JavaScript: the shortest digits that read back as `x`, with an exponent
    /// below 1e-6 and from 1e21.
    fn js_number(x: f64) -> String {
        if x == 0.0 {
            return "0".into();
        }
        if (1e-6..1e21).contains(&x.abs()) {
            return format!("{x}");
        }
        let s = format!("{x:e}");
        match s.split_once('e') {
            Some((m, e)) if !e.starts_with('-') => format!("{m}e+{e}"),
            _ => s,
        }
    }

    fn field<'a>(o: &'a Json, key: &str, place: &str) -> Result<&'a Value> {
        match o.get(key) {
            Some(v) => Ok(v),
            None => fail(format!("missing field `{key}` in {place}")),
        }
    }

    /// A whole number in range. JSON can't tell `1` from `1.0`, so neither does this.
    fn as_u32(v: &Value, what: &str) -> Result<u32> {
        match v.as_f64() {
            Some(x) if x.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(&x) => Ok(x as u32),
            _ => fail(format!("{what} must be a u32, not {}", show(v))),
        }
    }

    fn u32(o: &Json, key: &str, place: &str) -> Result<u32> {
        as_u32(field(o, key, place)?, &format!("{place}.{key}"))
    }

    /// A finite number, as an f32.
    fn finite(o: &Json, key: &str, place: &str) -> Result<f32> {
        match field(o, key, place)?.as_f64() {
            Some(x) if (x as f32).is_finite() => Ok(x as f32),
            _ => fail(format!("{place}.{key} must be a finite number")),
        }
    }

    fn string(o: &Json, key: &str, place: &str) -> Result<String> {
        match field(o, key, place)? {
            Value::String(s) => Ok(s.clone()),
            _ => fail(format!("{place}.{key} must be a string")),
        }
    }

    fn object<'a>(v: &'a Value, place: &str) -> Result<&'a Json> {
        match v {
            Value::Object(o) => Ok(o),
            _ => fail(format!("{place} must be an object")),
        }
    }

    /// The option that `v`, a string, names; else an error that `place` must be `what` (`a
    /// format`).
    fn named<T: Copy>(v: &Value, place: &str, what: &str, options: &[(&str, T)]) -> Result<T> {
        match options.iter().find(|(name, _)| v.as_str() == Some(name)) {
            Some(&(_, t)) => Ok(t),
            None => fail(format!("{place} must be {what}, not {}", show(v))),
        }
    }

    /// The option that `o`'s `key` names, as [`named`]; its error, which lists every option, is
    /// made only when there's none.
    fn one_of<T: Copy>(o: &Json, key: &str, place: &str, options: &[(&str, T)]) -> Result<T> {
        let v = field(o, key, place)?;
        match options.iter().find(|(name, _)| v.as_str() == Some(name)) {
            Some(&(_, t)) => Ok(t),
            None => {
                let names: Vec<&str> = options.iter().map(|(n, _)| *n).collect();
                fail(format!("{place}.{key} must be one of {}, not {}", names.join(", "), show(v)))
            }
        }
    }

    /// An array, each item read by `item` from its place (`pipelines[0].outputs[1]`) and value.
    fn list<T>(
        o: &Json,
        key: &str,
        place: &str,
        item: impl Fn(&str, &Value) -> Result<T>,
    ) -> Result<Vec<T>> {
        let Value::Array(items) = field(o, key, place)? else {
            return fail(format!("{place}.{key} must be an array"));
        };
        items.iter().enumerate().map(|(k, v)| item(&format!("{place}.{key}[{k}]"), v)).collect()
    }

    /// [`list`], or none where the key is missing.
    fn list_or_empty<T>(
        o: &Json,
        key: &str,
        place: &str,
        item: impl Fn(&str, &Value) -> Result<T>,
    ) -> Result<Vec<T>> {
        if o.contains_key(key) { list(o, key, place, item) } else { Ok(Vec::new()) }
    }

    /// [`one_of`], or `default` where the key is missing.
    fn one_of_or<T: Copy>(
        o: &Json,
        key: &str,
        place: &str,
        options: &[(&str, T)],
        default: T,
    ) -> Result<T> {
        if o.contains_key(key) { one_of(o, key, place, options) } else { Ok(default) }
    }

    /// A render pipeline's `targets`: each its `colors` (up to four formats), whether it has
    /// depth, and whether it's the screen (`false` where the key is missing).
    fn render_targets(o: &Json, place: &str) -> Result<Vec<RenderTarget>> {
        let Some(Value::Array(ts)) = o.get("targets") else {
            return fail(format!("{place}.targets must be an array"));
        };
        let names = TextureFormat::ALL.map(|f| (f.name(), f));
        ts.iter()
            .enumerate()
            .map(|(k, v)| {
                let tp = format!("{place}.targets[{k}]");
                let to = object(v, &tp)?;
                let Some(Value::Array(cs)) = to.get("colors") else {
                    return fail(format!("{tp}.colors must be an array of formats"));
                };
                if cs.len() > MAX_COLOR_ATTACHMENTS {
                    return fail(format!("{tp}.colors has more than four formats"));
                }
                let colors = cs
                    .iter()
                    .enumerate()
                    .map(|(c, f)| named(f, &format!("{tp}.colors[{c}]"), "a format", &names))
                    .collect::<Result<Vec<_>>>()?;
                let depth = match to.get("depth") {
                    Some(Value::Bool(b)) => *b,
                    _ => return fail(format!("{tp}.depth must be true or false")),
                };
                let screen = flag(to, "screen", &tp, false)?;
                Ok(RenderTarget { screen, ..RenderTarget::of(&colors, depth) })
            })
            .collect()
    }

    /// `true` or `false`, or `default` where the key is missing.
    fn flag(o: &Json, key: &str, place: &str, default: bool) -> Result<bool> {
        match o.get(key) {
            None => Ok(default),
            Some(Value::Bool(b)) => Ok(*b),
            Some(_) => fail(format!("{place}.{key} must be true or false")),
        }
    }

    fn pipeline(v: &Value, i: usize) -> Result<Pipeline> {
        let place = format!("pipelines[{i}]");
        let o = object(v, &place)?;
        let name = string(o, "name", &place)?;
        let shader = string(o, "shader", &place)?;
        // A missing `uniform` is read as null.
        let uniform = match o.get("uniform").unwrap_or(&Value::Null) {
            Value::Null => None,
            u => {
                let up = format!("{place}.uniform");
                let uo = object(u, &up)?;
                let spaces =
                    [("uniform", UniformSpace::Uniform), ("storage", UniformSpace::Storage)];
                Some(UniformBlock {
                    binding: u32(uo, "binding", &up)?,
                    size: u32(uo, "size", &up)?,
                    space: one_of(uo, "space", &up, &spaces)?,
                })
            }
        };
        let kinds = BindingKind::ALL.map(|k| (k.name(), k));
        let bindings = list(o, "bindings", &place, |bp, b| {
            let bo = object(b, bp)?;
            let kind = one_of(bo, "kind", bp, &kinds)?;
            // A colour texture's format.
            let format = match (kind.has_format(), bo.get("format")) {
                (false, None) => None,
                (false, Some(_)) => {
                    return fail(format!("{bp}: only a colour texture has a `format`"));
                }
                (true, _) => {
                    let names = TextureFormat::ALL.map(|f| (f.name(), f));
                    let f = one_of(bo, "format", bp, &names)?;
                    if f.is_depth() {
                        return fail(format!("{bp}: a colour texture's format isn't a depth one"));
                    }
                    if kind.is_storage() && !f.storable() {
                        return fail(format!("{bp}: kernels can't write {} textures", f.name()));
                    }
                    Some(f)
                }
            };
            Ok(ResourceBinding {
                binding: u32(bo, "binding", bp)?,
                kind,
                format,
                // A missing `stage` is both.
                stage: one_of_or(
                    bo,
                    "stage",
                    bp,
                    &[("vertex", BindingStage::Vertex), ("fragment", BindingStage::Fragment)],
                    BindingStage::Both,
                )?,
            })
        })?;
        let stage = match one_of(o, "kind", &place, &[("compute", true), ("render", false)])? {
            true => {
                let sizes = match field(o, "workgroup_size", &place)? {
                    Value::Array(ws) if ws.len() == 3 => ws,
                    _ => return fail(format!("{place}.workgroup_size must be an array of 3 u32s")),
                };
                let mut workgroup_size = [0; 3];
                for (k, s) in sizes.iter().enumerate() {
                    workgroup_size[k] = as_u32(s, &format!("{place}.workgroup_size[{k}]"))?;
                }
                Stage::Compute { entry: string(o, "entry", &place)?, workgroup_size }
            }
            false => Stage::Render {
                vertex_entry: string(o, "vertex_entry", &place)?,
                fragment_entry: string(o, "fragment_entry", &place)?,
                // Missing `blends` all replace.
                blends: list_or_empty(o, "blends", &place, |bp, b| {
                    named(b, bp, "a blend", &Blend::ALL.map(|b| (b.name(), b)))
                })?,
                // A missing `cull` is none, and a missing `depth_bias` none.
                cull: one_of_or(
                    o,
                    "cull",
                    &place,
                    &[("none", Cull::None), ("front", Cull::Front), ("back", Cull::Back)],
                    Cull::None,
                )?,
                depth_bias: match o.get("depth_bias") {
                    None => DepthBias::default(),
                    Some(v) => {
                        let bp = format!("{place}.depth_bias");
                        let bo = object(v, &bp)?;
                        let constant = match field(bo, "constant", &bp)?.as_f64() {
                            Some(x)
                                if x.fract() == 0.0
                                    && (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&x) =>
                            {
                                x as i32
                            }
                            _ => return fail(format!("{bp}.constant must be an i32")),
                        };
                        DepthBias {
                            constant,
                            slope_scale: finite(bo, "slope_scale", &bp)?,
                            clamp: finite(bo, "clamp", &bp)?,
                        }
                    }
                },
                // A missing `depth` is the default; so are its missing fields.
                depth: match o.get("depth") {
                    None => DepthState::default(),
                    Some(v) => {
                        let dp = format!("{place}.depth");
                        let d = object(v, &dp)?;
                        let compares: Vec<(&str, Compare)> =
                            Compare::ALL.iter().map(|c| (c.name(), *c)).collect();
                        DepthState {
                            compare: one_of_or(d, "compare", &dp, &compares, Compare::Less)?,
                            write: flag(d, "write", &dp, true)?,
                        }
                    }
                },
                // A missing `writes_depth` is false.
                writes_depth: flag(o, "writes_depth", &place, false)?,
                outputs: list(o, "outputs", &place, |op, x| {
                    named(
                        x,
                        op,
                        "float or uint",
                        &[("float", Output::Float), ("uint", Output::Uint)],
                    )
                })?,
                targets: render_targets(o, &place)?,
            },
        };
        // A missing `debug_flag` is read as null.
        let debug_flag = match o.get("debug_flag").unwrap_or(&Value::Null) {
            Value::Null => None,
            v => Some(as_u32(v, &format!("{place}.debug_flag"))?),
        };
        // Missing `requires` is none.
        let features = crate::FEATURES.map(|f| (f, f));
        let requires = list_or_empty(o, "requires", &place, |rp, f| {
            Ok(named(f, rp, "a feature this host knows", &features)?.to_string())
        })?;
        Ok(Pipeline { name, shader, stage, uniform, bindings, debug_flag, requires })
    }

    /// A manifest whose version has been checked.
    pub(super) fn manifest(v: &Value) -> Result<Manifest> {
        let o = object(v, "the manifest")?;
        let Value::Array(pipelines) = field(o, "pipelines", "the manifest")? else {
            return fail("pipelines must be an array".into());
        };
        Ok(Manifest {
            manifest_version: u32(o, "manifest_version", "the manifest")?,
            stream_version: u32(o, "stream_version", "the manifest")?,
            wasm: string(o, "wasm", "the manifest")?,
            pipelines: pipelines
                .iter()
                .enumerate()
                .map(|(i, p)| pipeline(p, i))
                .collect::<Result<_>>()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vectors::sample_manifest as sample;

    /// Golden JSON: the manifest's format is part of the contract.
    #[test]
    fn golden_json() {
        let json = sample().to_json();
        let expected = r#"{
  "manifest_version": 10,
  "stream_version": 12,
  "wasm": "game.wasm",
  "pipelines": [
    {
      "name": "sample",
      "shader": "pipeline_0.wgsl",
      "kind": "compute",
      "entry": "main",
      "workgroup_size": [
        64,
        1,
        1
      ],
      "uniform": {
        "binding": 0,
        "size": 32,
        "space": "uniform"
      },
      "bindings": [
        {
          "binding": 1,
          "kind": "read_write"
        }
      ]
    },
    {
      "name": "cover+shade",
      "shader": "pipeline_1.wgsl",
      "kind": "render",
      "vertex_entry": "vs",
      "fragment_entry": "fs",
      "outputs": [
        "float"
      ],
      "targets": [
        {
          "colors": [
            "rgba8unorm"
          ],
          "depth": false,
          "screen": true
        }
      ],
      "uniform": null,
      "bindings": [
        {
          "binding": 0,
          "kind": "texture",
          "format": "rgba8unorm"
        },
        {
          "binding": 1,
          "kind": "sampler"
        }
      ]
    }
  ]
}
"#;
        assert_eq!(json, expected);
        assert_eq!(Manifest::parse(&json).expect("valid"), sample());
    }

    #[test]
    fn rejects_other_versions() {
        let json = sample()
            .to_json()
            .replace(&format!("\"manifest_version\": {VERSION}"), "\"manifest_version\": 1");
        assert!(Manifest::parse(&json).is_err());
        let json = sample().to_json().replace(
            &format!("\"stream_version\": {}", crate::stream::VERSION),
            "\"stream_version\": 99",
        );
        assert!(Manifest::parse(&json).is_err());
    }

    #[test]
    fn rejects_bad_pipelines() {
        let mut m = sample();
        if let Stage::Compute { workgroup_size, .. } = &mut m.pipelines[0].stage {
            *workgroup_size = [512, 1, 1];
        }
        assert!(m.validate().is_err());
        let mut m = sample();
        m.pipelines[0].bindings[0].binding = 0;
        assert!(m.validate().is_err());
        let mut m = sample();
        m.pipelines[0].uniform =
            Some(UniformBlock { binding: 0, size: 20, space: UniformSpace::Uniform });
        assert!(m.validate().is_err());
    }

    #[test]
    fn only_kernels_bind_storage_textures() {
        for kind in [BindingKind::StorageTexture, BindingKind::StorageTexture3d] {
            for stage in [BindingStage::Both, BindingStage::Vertex, BindingStage::Fragment] {
                let mut m = sample();
                m.pipelines[1].bindings[0].kind = kind;
                m.pipelines[1].bindings[0].stage = stage;
                assert_eq!(
                    Manifest::parse(&m.to_json()).expect_err("render storage texture").0,
                    "pipeline 1's binding 0 is a storage texture, which only a kernel can bind"
                );
            }
            let mut m = sample();
            m.pipelines[0].bindings[0].kind = kind;
            m.pipelines[0].bindings[0].format = Some(TextureFormat::Rgba8);
            Manifest::parse(&m.to_json()).expect("kernel storage texture");
        }
    }

    #[test]
    fn rejects_workgroup_sizes_whose_product_overflows_u64() {
        let mut m = sample();
        if let Stage::Compute { workgroup_size, .. } = &mut m.pipelines[0].stage {
            *workgroup_size = [u32::MAX; 3];
        }
        let error = Manifest::parse(&m.to_json()).expect_err("oversized workgroup");
        assert_eq!(
            error.0,
            "pipeline 0's workgroup size [4294967295, 4294967295, 4294967295] is outside WebGPU's limits"
        );
    }

    #[test]
    fn binding_numbers_are_below_the_bind_group_limit() {
        for slot in 0..3 {
            for binding in [MAX_BINDINGS_PER_GROUP - 1, MAX_BINDINGS_PER_GROUP, u32::MAX] {
                let mut m = sample();
                let p = &mut m.pipelines[0];
                match slot {
                    0 => p.bindings[0].binding = binding,
                    1 => p.uniform.as_mut().expect("uniform").binding = binding,
                    _ => p.debug_flag = Some(binding),
                }
                let result = Manifest::parse(&m.to_json());
                if binding < MAX_BINDINGS_PER_GROUP {
                    result.expect("last binding within the limit");
                } else {
                    assert_eq!(
                        result.expect_err("binding over the limit").0,
                        format!(
                            "pipeline 0 uses binding {binding}; WebGPU's default limit is {MAX_BINDINGS_PER_GROUP}"
                        )
                    );
                }
            }
        }
    }

    #[test]
    fn blended_targets_support_blending() {
        for (format, blendable) in [
            (TextureFormat::Rgba8, true),
            (TextureFormat::Rgba16Float, true),
            (TextureFormat::R16Float, true),
            (TextureFormat::Rg16Float, true),
            (TextureFormat::R8, true),
            (TextureFormat::Rgb10a2, true),
            // Blendable with `float32-blendable`, which the host checks at the draw.
            (TextureFormat::R32Float, true),
            (TextureFormat::Rgba32Float, true),
            (TextureFormat::R32Uint, false),
            (TextureFormat::Rg32Uint, false),
        ] {
            let mut m = sample();
            if let Stage::Render { blends, outputs, targets, .. } = &mut m.pipelines[1].stage {
                *blends = vec![Blend::Over];
                *outputs = vec![if format.is_uint() { Output::Uint } else { Output::Float }];
                *targets = vec![RenderTarget::of(&[format], false)];
            }
            let result = Manifest::parse(&m.to_json());
            if blendable {
                result.expect("blendable colour target");
            } else {
                assert_eq!(
                    result.expect_err("unblendable target").0,
                    format!(
                        "pipeline 1's target {format} can't be one: colour 0 blends, and {format} doesn't blend",
                        format = format.name()
                    )
                );
            }
        }
        let mut m = sample();
        if let Stage::Render { blends, targets, .. } = &mut m.pipelines[1].stage {
            *blends = vec![Blend::Over];
            *targets = vec![RenderTarget::of(&[], true)];
        }
        Manifest::parse(&m.to_json()).expect("a depth-only variant has no colour blend");
    }

    /// A target's colours are as many as the fragment shader writes, an integer format where
    /// it writes integers, within WebGPU's 32 bytes a sample (its algorithm: the running total
    /// aligned to each format, then its cost added).
    #[test]
    fn several_colour_targets_match_the_outputs() {
        use TextureFormat as F;
        let with = |outputs: Vec<Output>, colors: &[F]| {
            let mut m = sample();
            if let Stage::Render { outputs: o, targets, .. } = &mut m.pipelines[1].stage {
                *o = outputs;
                *targets = vec![RenderTarget::of(colors, true)];
            }
            Manifest::parse(&m.to_json()).map(|_| ())
        };
        let f = Output::Float;
        with(vec![f, f, f, Output::Uint], &[F::Rgba16Float, F::Rg16Float, F::R8, F::R32Uint])
            .expect("20 bytes a sample");
        let e = with(vec![f, f, f, f], &[F::Rgba32Float, F::Rgba8, F::Rgba8, F::Rgba16Float])
            .expect_err("40 bytes a sample");
        assert!(e.0.contains("40 bytes a sample, over WebGPU's 32"), "{}", e.0);
        let e = with(vec![f, f], &[F::Rgba8]).expect_err("two outputs into one colour");
        assert!(e.0.contains("writes 2 colours, and it has 1"), "{}", e.0);
        let e = with(vec![f], &[F::R32Uint]).expect_err("floats into integers");
        assert!(e.0.contains("writes floats to colour 0"), "{}", e.0);
    }

    /// A target's colours are those before its first `None`: one after a gap is refused, not
    /// dropped.
    #[test]
    fn a_target_with_a_gap_in_its_colours_is_refused() {
        let mut m = sample();
        if let Stage::Render { targets, .. } = &mut m.pipelines[1].stage {
            targets[0].colors =
                [Some(TextureFormat::Rgba8), None, Some(TextureFormat::Rgba8), None];
        }
        assert_eq!(
            m.validate().expect_err("a colour after a gap").0,
            "pipeline 1's target 0 has a colour after a missing one: its colours come first"
        );
    }

    #[test]
    fn screen_targets_say_so() {
        // The screen and a texture of the screen's format are two targets: only the screen's
        // JSON says it's the screen.
        let mut m = sample();
        let texture = RenderTarget::of(&[TextureFormat::Rgba8], false);
        if let Stage::Render { targets, .. } = &mut m.pipelines[1].stage {
            targets.push(texture);
            targets.push(RenderTarget::screen(true));
        }
        let json = m.to_json();
        assert_eq!(json.matches("\"screen\": true").count(), 2);
        let parsed = Manifest::parse(&json).expect("the screen and a texture of its format");
        let Stage::Render { targets, .. } = &parsed.pipelines[1].stage else { panic!("render") };
        assert_eq!(targets, &[RenderTarget::screen(false), texture, RenderTarget::screen(true)]);
        // A screen target's colour is the screen's format.
        for colors in [&[TextureFormat::Rgba16Float][..], &[]] {
            let mut m = sample();
            if let Stage::Render { targets, .. } = &mut m.pipelines[1].stage {
                targets[0] = RenderTarget { screen: true, ..RenderTarget::of(colors, true) };
            }
            assert_eq!(
                Manifest::parse(&m.to_json()).expect_err("a screen of another format").0,
                "pipeline 1's target 0 is the screen, whose one colour is rgba8unorm"
            );
        }
    }
}
