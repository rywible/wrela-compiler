//! # The manifest, version 6
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
//!   `"write"`, whether it replaces it (`true` by default). Hosts make one pipeline per combination of target formats a
//!   pipeline is drawn with. A render pipeline with `"blend": true` draws its colour over what's
//!   in the target (alpha blending, straight alpha: `src × src.a + dst × (1 − src.a)` for the
//!   colour, `src.a + dst.a × (1 − src.a)` for alpha); without it, its colour replaces it. One
//!   with `"writes_depth": true` gives each fragment its own depth (WGSL's `frag_depth`), so it
//!   is drawn only in a pass with a depth target: hosts make no variant of it without one, and
//!   refuse a draw of it in a pass without one.
//! - A render pipeline's `"cull"` drops triangles by facing (`"none"`, the default, `"front"`
//!   or `"back"`; a triangle whose vertices run counter-clockwise on the target faces the
//!   front). Its `"depth_bias"` moves each fragment's depth by `constant` steps of the depth
//!   format and `slope_scale` times the triangle's depth slope, at most `clamp` (when not 0)
//!   in size: WebGPU's `depthBias`, `depthBiasSlopeScale` and `depthBiasClamp`. None of these
//!   may be given at run time, so a program states them where it draws (language.md §12).

use crate::Limits;
pub use crate::stream::Compare;
use serde::Serialize;
use serde_json::{Map, Value};
use std::fmt;

pub const VERSION: u32 = 6;
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
    },
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
    /// A filterable float texture.
    Texture,
    /// A depth texture, for comparison sampling.
    DepthTexture,
    /// A filtering sampler.
    Sampler,
    /// A comparison sampler.
    ComparisonSampler,
    /// An `rgba16float` texture a kernel writes (a storage texture, written only).
    StorageTexture,
    /// A filterable float 3D texture.
    #[serde(rename = "texture_3d")]
    Texture3d,
    /// An `rgba16float` 3D texture a kernel writes.
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

    pub fn is_texture(self) -> bool {
        matches!(self, BindingKind::Texture | BindingKind::DepthTexture | BindingKind::Texture3d)
    }

    pub fn is_sampler(self) -> bool {
        matches!(self, BindingKind::Sampler | BindingKind::ComparisonSampler)
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
            // Each stage's storage buffers: its bindings', the uniform block's when it's in
            // storage, and the debug flag (a fragment shader's or a kernel's).
            let in_storage =
                usize::from(p.uniform.as_ref().is_some_and(|u| u.space == UniformSpace::Storage));
            let stages: &[BindingStage] = if render {
                &[BindingStage::Vertex, BindingStage::Fragment]
            } else {
                &[BindingStage::Both]
            };
            for &stage in stages {
                let storage =
                    p.bindings.iter().filter(|b| b.kind.is_buffer() && b.stage.sees(stage)).count()
                        + in_storage
                        + usize::from(p.debug_flag.is_some() && stage != BindingStage::Vertex);
                if storage > MAX_STORAGE_BUFFERS_PER_STAGE {
                    let what = match stage {
                        BindingStage::Vertex => " in its vertex shader",
                        BindingStage::Fragment => " in its fragment shader",
                        BindingStage::Both => "",
                    };
                    return err(format!(
                        "pipeline {i} has {storage} storage buffers{what}; WebGPU's default limit is {MAX_STORAGE_BUFFERS_PER_STAGE}"
                    ));
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
                Stage::Render { vertex_entry, fragment_entry, .. } => {
                    if vertex_entry.is_empty() || fragment_entry.is_empty() {
                        return err(format!("pipeline {i} is missing an entry point"));
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
                Ok(ResourceBinding {
                    binding: u32(bo, "binding", &bp)?,
                    kind: one_of(bo, "kind", &bp, &kinds)?,
                    // A missing `stage` is both.
                    stage: match bo.get("stage") {
                        None => BindingStage::Both,
                        Some(_) => one_of(
                            bo,
                            "stage",
                            &bp,
                            &[
                                ("vertex", BindingStage::Vertex),
                                ("fragment", BindingStage::Fragment),
                            ],
                        )?,
                    },
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
                blend: match o.get("blend").unwrap_or(&Value::Bool(false)) {
                    Value::Bool(b) => *b,
                    _ => return fail(format!("{place}.blend must be true or false")),
                },
                // A missing `cull` is none, and a missing `depth_bias` none.
                cull: match o.get("cull") {
                    None => Cull::None,
                    Some(_) => one_of(
                        o,
                        "cull",
                        &place,
                        &[("none", Cull::None), ("front", Cull::Front), ("back", Cull::Back)],
                    )?,
                },
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
                            compare: match d.get("compare") {
                                None => Compare::Less,
                                Some(_) => one_of(d, "compare", &dp, &compares)?,
                            },
                            write: match d.get("write").unwrap_or(&Value::Bool(true)) {
                                Value::Bool(b) => *b,
                                _ => return fail(format!("{dp}.write must be true or false")),
                            },
                        }
                    }
                },
                // A missing `writes_depth` is false.
                writes_depth: match o.get("writes_depth").unwrap_or(&Value::Bool(false)) {
                    Value::Bool(b) => *b,
                    _ => return fail(format!("{place}.writes_depth must be true or false")),
                },
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
  "manifest_version": 6,
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
      "uniform": null,
      "bindings": [
        {
          "binding": 0,
          "kind": "texture"
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
