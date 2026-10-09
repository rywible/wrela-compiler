//! # The manifest, version 8
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
//!   texture. With a depth target, a fragment is kept where its depth is less than what's
//!   there, which it replaces, unless its `"depth"` says otherwise: `"compare"`, how its depth
//!   compares with what's there for it to be kept (WebGPU's names, `"less"` by default), and
//!   `"write"`, whether it replaces it (`true` by default). A render pipeline's `"targets"`
//!   are every combination of targets the program draws it into, as the compiler knows them
//!   from each draw's pass: each a colour format (`null` in a pass that draws only depths; the
//!   screen's is [`SCREEN_FORMAT`]) and whether there's a depth target (`depth32float`), at
//!   least one. Hosts make one pipeline for each when the program loads, and refuse a draw into
//!   any other. A render pipeline with `"blend": true` draws its colour over what's
//!   in the target (alpha blending, straight alpha: `src × src.a + dst × (1 − src.a)` for the
//!   colour, `src.a + dst.a × (1 − src.a)` for alpha); without it, its colour replaces it. One
//!   with `"writes_depth": true` gives each fragment its own depth (WGSL's `frag_depth`), so its
//!   targets all have depth; one with `"uint": true` returns a `u32`, so its targets' colour, if
//!   they have one, is `r32uint`, which no other's is.
//! - A render pipeline's `"cull"` drops triangles by facing (`"none"`, the default, `"front"`
//!   or `"back"`; a triangle whose vertices run counter-clockwise on the target faces the
//!   front). Its `"depth_bias"` moves each fragment's depth by `constant` steps of the depth
//!   format and `slope_scale` times the triangle's depth slope, at most `clamp` (when not 0)
//!   in size: WebGPU's `depthBias`, `depthBiasSlopeScale` and `depthBiasClamp`. None of these
//!   may be given at run time, so a program states them where it draws (language.md §12).

use crate::Limits;
pub use crate::stream::Compare;
use crate::stream::TextureFormat;
use serde::Serialize;
use serde_json::{Map, Value};
use std::fmt;

pub const VERSION: u32 = 8;
/// The screen's texture format, in WebGPU's spelling.
pub const SCREEN_FORMAT: &str = "rgba8unorm";
/// WebGPU's default limits that the manifest is checked against ([`Limits::DEFAULT`]).
pub const MAX_WORKGROUP_SIZE: [u32; 3] = Limits::DEFAULT.max_workgroup_size;
pub const MAX_WORKGROUP_INVOCATIONS: u32 = Limits::DEFAULT.max_workgroup_invocations;
pub const MAX_STORAGE_BUFFERS_PER_STAGE: usize =
    Limits::DEFAULT.max_storage_buffers_per_stage as usize;
pub const MAX_UNIFORM_BUFFER_BINDING_SIZE: u32 = Limits::DEFAULT.max_uniform_buffer_binding_size;
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
        /// Draws its colour over the target's (alpha blending) rather than replacing it.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        blend: bool,
        #[serde(skip_serializing_if = "Cull::is_none")]
        cull: Cull,
        #[serde(skip_serializing_if = "DepthBias::is_none")]
        depth_bias: DepthBias,
        #[serde(skip_serializing_if = "DepthState::is_default")]
        depth: DepthState,
        /// Gives each fragment its own depth: drawn only in a pass with a depth target.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        writes_depth: bool,
        /// Its fragment shader returns a `u32`: drawn only into an `r32uint` target, which a
        /// `vec4` colour isn't drawn into.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        uint: bool,
        /// Every combination of targets it's drawn into, first drawn first.
        targets: Vec<RenderTarget>,
    },
}

/// What a render pipeline draws into: a colour target of a format (none in a pass that draws
/// only depths; the screen's is [`SCREEN_FORMAT`]), and a depth target (`depth32float`) or
/// none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RenderTarget {
    pub color: Option<TextureFormat>,
    pub depth: bool,
}

impl RenderTarget {
    /// The screen's target, with depth or without.
    pub fn screen(depth: bool) -> RenderTarget {
        let color = TextureFormat::ALL.into_iter().find(|f| f.name() == SCREEN_FORMAT);
        RenderTarget { color, depth }
    }
}

impl fmt::Display for RenderTarget {
    /// As errors name it: `rgba16float with depth`, `depth alone`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.color, self.depth) {
            (Some(c), true) => write!(f, "{} with depth", c.name()),
            (Some(c), false) => write!(f, "{}", c.name()),
            (None, _) => write!(f, "depth alone"),
        }
    }
}

impl Serialize for RenderTarget {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut o = s.serialize_struct("RenderTarget", 2)?;
        o.serialize_field("color", &self.color.map(TextureFormat::name))?;
        o.serialize_field("depth", &self.depth)?;
        o.end()
    }
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
}

impl BindingKind {
    pub const ALL: [BindingKind; 9] = [
        BindingKind::Read,
        BindingKind::ReadWrite,
        BindingKind::Texture,
        BindingKind::DepthTexture,
        BindingKind::Sampler,
        BindingKind::ComparisonSampler,
        BindingKind::StorageTexture,
        BindingKind::Texture3d,
        BindingKind::StorageTexture3d,
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
        )
    }

    /// Whether kernels write it: a storage texture.
    pub fn is_storage(self) -> bool {
        matches!(self, BindingKind::StorageTexture | BindingKind::StorageTexture3d)
    }

    /// Whether it's a 3D texture.
    pub fn is_3d(self) -> bool {
        matches!(self, BindingKind::Texture3d | BindingKind::StorageTexture3d)
    }

    /// The per-stage limit it counts against.
    pub fn limit(self) -> StageLimit {
        match self {
            BindingKind::Read | BindingKind::ReadWrite => StageLimit::StorageBuffers,
            BindingKind::Texture | BindingKind::DepthTexture | BindingKind::Texture3d => {
                StageLimit::SampledTextures
            }
            BindingKind::Sampler | BindingKind::ComparisonSampler => StageLimit::Samplers,
            BindingKind::StorageTexture | BindingKind::StorageTexture3d => {
                StageLimit::StorageTextures
            }
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
    /// A colour texture's format (`texture`, `texture_3d`, `storage_texture`,
    /// `storage_texture_3d`): what its texels are read as, and a storage texture's format.
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
            let render = matches!(p.stage, Stage::Render { .. });
            for b in &p.bindings {
                if !render && !b.stage.is_both() {
                    return err(format!(
                        "pipeline {i}'s binding {} names a stage, but it's a kernel's",
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
                    let total: u64 = workgroup_size.iter().map(|&x| x as u64).product();
                    let ok = workgroup_size
                        .iter()
                        .zip(MAX_WORKGROUP_SIZE)
                        .all(|(&s, m)| s >= 1 && s <= m)
                        && total <= MAX_WORKGROUP_INVOCATIONS as u64;
                    if !ok {
                        return err(format!(
                            "pipeline {i}'s workgroup size {workgroup_size:?} is outside WebGPU's limits"
                        ));
                    }
                }
                Stage::Render {
                    vertex_entry, fragment_entry, writes_depth, uint, targets, ..
                } => {
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
                        let integer = t.color.is_some_and(|c| c.is_uint());
                        let why = if *writes_depth && !t.depth {
                            Some("its fragments give their depth, so a target has depth")
                        } else if *uint && t.color.is_some() && !integer {
                            Some(
                                "its fragment shader returns a `u32`, so a colour target is r32uint",
                            )
                        } else if !*uint && integer {
                            Some(
                                "its fragment shader returns a colour, which an r32uint target isn't",
                            )
                        } else if t.color.is_some_and(|c| c.is_depth()) {
                            Some("a colour target's format isn't a depth one")
                        } else {
                            None
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

    /// A render pipeline's `targets`: each a colour format or `null`, and whether it has depth.
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
                let color = match to.get("color") {
                    Some(Value::Null) => None,
                    Some(_) => Some(one_of(to, "color", &tp, &names)?),
                    None => return fail(format!("{tp}.color must be a format or null")),
                };
                let depth = match to.get("depth") {
                    Some(Value::Bool(b)) => *b,
                    _ => return fail(format!("{tp}.depth must be true or false")),
                };
                Ok(RenderTarget { color, depth })
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
        let Value::Array(list) = field(o, "bindings", &place)? else {
            return fail(format!("{place}.bindings must be an array"));
        };
        let kinds = BindingKind::ALL.map(|k| (k.name(), k));
        let bindings = list
            .iter()
            .enumerate()
            .map(|(j, b)| {
                let bp = format!("{place}.bindings[{j}]");
                let bo = object(b, &bp)?;
                let kind = one_of(bo, "kind", &bp, &kinds)?;
                // A colour texture's format.
                let format = match (kind.has_format(), bo.get("format")) {
                    (false, None) => None,
                    (false, Some(_)) => {
                        return fail(format!("{bp}: only a colour texture has a `format`"));
                    }
                    (true, _) => {
                        let names = TextureFormat::ALL.map(|f| (f.name(), f));
                        let f = one_of(bo, "format", &bp, &names)?;
                        if f.is_depth() {
                            return fail(format!(
                                "{bp}: a colour texture's format isn't a depth one"
                            ));
                        }
                        if kind.is_storage() && !f.storable() {
                            return fail(format!(
                                "{bp}: kernels can't write {} textures",
                                f.name()
                            ));
                        }
                        Some(f)
                    }
                };
                Ok(ResourceBinding {
                    binding: u32(bo, "binding", &bp)?,
                    kind,
                    format,
                    // A missing `stage` is both.
                    stage: one_of_or(
                        bo,
                        "stage",
                        &bp,
                        &[("vertex", BindingStage::Vertex), ("fragment", BindingStage::Fragment)],
                        BindingStage::Both,
                    )?,
                })
            })
            .collect::<Result<_>>()?;
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
                // A missing `blend` is false.
                blend: flag(o, "blend", &place, false)?,
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
                // A missing `writes_depth` is false, and a missing `uint`.
                writes_depth: flag(o, "writes_depth", &place, false)?,
                uint: flag(o, "uint", &place, false)?,
                targets: render_targets(o, &place)?,
            },
        };
        // A missing `debug_flag` is read as null.
        let debug_flag = match o.get("debug_flag").unwrap_or(&Value::Null) {
            Value::Null => None,
            v => Some(as_u32(v, &format!("{place}.debug_flag"))?),
        };
        Ok(Pipeline { name, shader, stage, uniform, bindings, debug_flag })
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
  "manifest_version": 8,
  "stream_version": 7,
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
      "targets": [
        {
          "color": "rgba8unorm",
          "depth": false
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
}
