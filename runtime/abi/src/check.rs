//! Checks each command against the manifest and the program's resources before it runs.
//!
//! WebGPU reports most misuse asynchronously, or as a device error that names no command; wgpu
//! reports it as a validation error. Checking up front means both hosts reject the same
//! programs, at the command that's wrong, with the same messages (the browser runtime's
//! `src/check.ts` mirrors this file, and the test vectors hold both to it):
//!
//! - **Creating** a buffer, texture or sampler: the handle names no live resource; a buffer's
//!   size and a texture's are within the device's limits ([`crate::Limits`]).
//! - **Writing, copying, reading back, destroying:** the resource exists and is the right kind;
//!   every range fits inside it; a copy's source and destination are different buffers.
//! - **`Dispatch`, `Draw` and their indirect forms:** the pipeline exists and is the right kind;
//!   the command lists one binding for each of the pipeline's, of the kind it binds, each
//!   existing, a buffer's range inside the buffer and starting at a multiple of 256 bytes
//!   (WebGPU's default `minStorageBufferOffsetAlignment`), and exactly the uniform block's size
//!   in uniform bytes; a dispatch's group counts are within the limit; an indirect command's
//!   arguments fit in their buffer; an indexed draw's indices are a non-empty range of `u32`s
//!   inside their buffer; a draw's pipeline is one its pass's targets take (one that gives its
//!   fragments their depth needs a depth target, and one whose fragment shader returns a `u32`
//!   an `r32uint` colour target, which no other is drawn into).
//! - **Usage scopes** (WebGPU's rule): no buffer or texture is used both read-only and
//!   read-write in one dispatch or one pass, none is bound read-write twice by one command, and
//!   a pass's attachments aren't bound by its draws. An indirect command's arguments buffer and
//!   an indexed draw's index buffer are read-only use.
//! - **Passes:** the clear colour and depth are finite; a pass's attachments exist, the colour
//!   one isn't a depth texture and the depth one is, and they're the same size.
//! - **Requests:** a storage path or a URL is relative, `/`-separated, with no empty, `.` or
//!   `..` parts ([`path_problem`]).

use crate::Manifest;
use crate::manifest::{BindingKind, Stage};
use crate::stream::{Binding, Command, MAX_TEXTURE_3D, NONE, Opcode, SCREEN, TextureFormat};
use std::collections::HashMap;
use std::fmt;

/// `minStorageBufferOffsetAlignment`: where a bound range of a buffer may start.
pub const BINDING_OFFSET_ALIGNMENT: u32 = 256;

/// The screen's format ([`crate::manifest::SCREEN_FORMAT`]).
const SCREEN_FORMAT: TextureFormat = TextureFormat::Rgba8;

/// A well-formed command the host can't carry out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandError {
    pub opcode: Opcode,
    pub why: String,
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed: {}", self.opcode.name(), self.why)
    }
}

impl std::error::Error for CommandError {}

type Result<T> = std::result::Result<T, CommandError>;

/// What's wrong with a storage path or a fetched URL, if anything: it's relative to the program's
/// storage (or its build), with `/` between its parts, and none of them empty, `.` or `..`.
pub fn path_problem(path: &str) -> Option<&'static str> {
    if path.is_empty() {
        return Some("is empty");
    }
    if path.starts_with('/') || path.contains(['\\', ':', '?', '#']) {
        return Some("must be relative, with `/` between its parts");
    }
    if path.split('/').any(|part| part.is_empty() || part == "." || part == "..") {
        return Some("has an empty, `.` or `..` part");
    }
    None
}

struct Shape {
    name: String,
    compute: bool,
    uniform_size: usize,
    /// Each binding's kind, and a colour texture's format.
    bindings: Vec<(BindingKind, Option<TextureFormat>)>,
    /// A render pipeline's: each fragment gives its own depth (drawn only with a depth target),
    /// and its fragment shader returns a `u32` (drawn only into an `r32uint` target).
    writes_depth: bool,
    uint: bool,
}

/// A live texture.
#[derive(Clone, Copy, Debug)]
struct Texture {
    width: u32,
    height: u32,
    /// A 3D texture's depth, or 0 for a 2D texture.
    depth: u32,
    format: TextureFormat,
    /// Kernels may write it.
    writable: bool,
}

/// A live resource.
#[derive(Clone, Copy, Debug)]
enum Resource {
    Buffer { size: u32 },
    Texture(Texture),
    Sampler { comparison: bool },
}

impl Resource {
    fn kind(self) -> &'static str {
        match self {
            Resource::Buffer { .. } => "a buffer",
            Resource::Texture(_) => "a texture",
            Resource::Sampler { .. } => "a sampler",
        }
    }
}

pub struct Checker {
    limits: crate::Limits,
    pipelines: Vec<Shape>,
    resources: HashMap<u32, Resource>,
    /// The current usage scope, a dispatch or the open pass: each buffer's or texture's use
    /// so far, (read-only, read-write). Dispatches happen only outside a pass (the sequencer
    /// checks), so the two kinds of scope never overlap and share this map.
    scope: HashMap<u32, (bool, bool)>,
    /// The open pass's attachments.
    attachments: Vec<u32>,
    /// The open pass's colour target's format (the screen's, a texture's, or none), and whether
    /// it has a depth target: which pipelines its draws may draw with.
    targets: (Option<TextureFormat>, bool),
}

impl Checker {
    /// A checker for a device with WebGPU's default limits.
    pub fn new(manifest: &Manifest) -> Checker {
        Checker::with_limits(manifest, crate::Limits::DEFAULT)
    }

    /// A checker for a device with these limits.
    pub fn with_limits(manifest: &Manifest, limits: crate::Limits) -> Checker {
        let pipelines = manifest
            .pipelines
            .iter()
            .map(|p| {
                let (writes_depth, uint) = match p.stage {
                    Stage::Render { writes_depth, uint, .. } => (writes_depth, uint),
                    Stage::Compute { .. } => (false, false),
                };
                Shape {
                    name: p.name.clone(),
                    compute: matches!(p.stage, Stage::Compute { .. }),
                    uniform_size: p.uniform.as_ref().map_or(0, |u| u.size as usize),
                    bindings: p.bindings.iter().map(|b| (b.kind, b.format)).collect(),
                    writes_depth,
                    uint,
                }
            })
            .collect();
        Checker {
            limits,
            pipelines,
            resources: HashMap::new(),
            scope: HashMap::new(),
            attachments: Vec::new(),
            targets: (None, false),
        }
    }

    /// Checks a command that has passed the [`crate::stream::Sequencer`], and records its
    /// effect on what later commands may do.
    pub fn check(&mut self, cmd: &Command<'_>) -> Result<()> {
        let op = cmd.opcode();
        let err = |why: String| Err(CommandError { opcode: op, why });
        match cmd {
            Command::CreateBuffer { handle, size } => {
                let max = self.limits.max_buffer_size;
                if *size > max {
                    return err(format!("buffer {handle} is {size} bytes; the limit is {max}"));
                }
                self.create(op, *handle, Resource::Buffer { size: *size })?;
            }
            Command::CreateTexture { handle, width, height, format, writable, depth } => {
                let max = self.limits.max_texture_size;
                if *width > max || *height > max {
                    return err(format!(
                        "texture {handle} is {width}x{height}; the limit is {max} a side"
                    ));
                }
                if [*width, *height, *depth].iter().any(|&s| s > MAX_TEXTURE_3D) && *depth > 0 {
                    return err(format!(
                        "3D texture {handle} is {width}x{height}x{depth}; the limit is {MAX_TEXTURE_3D} a side"
                    ));
                }
                let t = Texture {
                    width: *width,
                    height: *height,
                    depth: *depth,
                    format: *format,
                    writable: *writable,
                };
                self.create(op, *handle, Resource::Texture(t))?;
            }
            Command::CreateSampler { handle, compare, .. } => {
                self.create(op, *handle, Resource::Sampler { comparison: compare.is_some() })?;
            }
            Command::DestroyBuffer { handle } => {
                self.buffer(op, *handle)?;
                self.resources.remove(handle);
            }
            Command::DestroyTexture { handle } => {
                self.texture(op, *handle)?;
                self.resources.remove(handle);
            }
            Command::DestroySampler { handle } => {
                self.sampler(op, *handle)?;
                self.resources.remove(handle);
            }
            Command::WriteBuffer { handle, offset, data } => {
                self.range(op, *handle, *offset, data.len() as u32, "writing")?;
            }
            Command::ReadBuffer { handle, offset, size, .. } => {
                self.range(op, *handle, *offset, *size, "reading")?;
            }
            Command::CopyBuffer {
                source,
                source_offset,
                destination,
                destination_offset,
                size,
            } => {
                if source == destination {
                    return err(format!(
                        "buffer {source} is both the copy's source and its destination"
                    ));
                }
                self.range(op, *source, *source_offset, *size, "copying")?;
                self.range(op, *destination, *destination_offset, *size, "copying")?;
            }
            Command::WriteTexture { handle, x, y, width, height, data } => {
                let Texture { width: w, height: h, depth, format, .. } =
                    self.texture(op, *handle)?;
                if depth > 0 {
                    return err(format!(
                        "texture {handle} is a 3D texture, whose texels kernels write"
                    ));
                }
                if u64::from(*x) + u64::from(*width) > u64::from(w)
                    || u64::from(*y) + u64::from(*height) > u64::from(h)
                {
                    return err(format!(
                        "writing {width}x{height} texels at ({x}, {y}) overruns texture {handle} ({w}x{h})"
                    ));
                }
                if format.is_depth() {
                    return err(format!(
                        "texture {handle} is a depth texture, which only a pass can write"
                    ));
                }
                let want =
                    u64::from(*width) * u64::from(*height) * u64::from(format.bytes_per_texel());
                if data.len() as u64 != want {
                    return err(format!(
                        "{width}x{height} texels of {} are {want} bytes, not {}",
                        format.name(),
                        data.len()
                    ));
                }
            }
            Command::Dispatch { pipeline, groups, bindings, uniforms } => {
                self.scope.clear();
                self.bindings(op, *pipeline, bindings, uniforms.len())?;
                let max = self.limits.max_workgroups_per_dimension;
                if groups.iter().any(|&g| g > max) {
                    let [x, y, z] = groups;
                    return err(format!(
                        "{x}x{y}x{z} workgroups is over the limit of {max} per dimension"
                    ));
                }
            }
            Command::DispatchIndirect { pipeline, arguments, offset, bindings, uniforms } => {
                self.scope.clear();
                self.bindings(op, *pipeline, bindings, uniforms.len())?;
                self.read_range(op, *arguments, *offset, 12, "reading arguments:")?;
            }
            Command::BeginScreenPass { clear } => {
                self.clear_colour(op, clear)?;
                self.scope.clear();
                self.attachments.clear();
                self.targets = (Some(SCREEN_FORMAT), false);
            }
            Command::BeginPass(pass) => {
                self.clear_colour(op, &pass.clear)?;
                if !pass.clear_depth.is_finite() {
                    return err("the clear depth isn't a finite number".into());
                }
                self.scope.clear();
                self.attachments.clear();
                let mut size = None;
                for t in [pass.color, pass.depth] {
                    let three =
                        matches!(self.resources.get(&t), Some(Resource::Texture(x)) if x.depth > 0);
                    if t != SCREEN && t != NONE && three {
                        return err(format!(
                            "texture {t} is a 3D texture, so it can't be a target"
                        ));
                    }
                }
                let mut colour = (pass.color == SCREEN).then_some(SCREEN_FORMAT);
                if pass.color != SCREEN && pass.color != NONE {
                    let t = self.texture(op, pass.color)?;
                    if t.format.is_depth() {
                        return err(format!(
                            "texture {} is a depth texture, so it can't be a colour target",
                            pass.color
                        ));
                    }
                    size = Some((t.width, t.height));
                    colour = Some(t.format);
                    self.attachments.push(pass.color);
                }
                if pass.depth != NONE {
                    let t = self.texture(op, pass.depth)?;
                    if !t.format.is_depth() {
                        return err(format!("texture {} isn't a depth texture", pass.depth));
                    }
                    if size.is_some_and(|s| s != (t.width, t.height)) {
                        return err("the pass's colour and depth targets differ in size".into());
                    }
                    self.attachments.push(pass.depth);
                }
                if pass.color == NONE && pass.depth == NONE {
                    return err("a pass needs a colour target or a depth target".into());
                }
                self.targets = (colour, pass.depth != NONE);
            }
            Command::Draw { pipeline, bindings, uniforms, .. } => {
                self.bindings(op, *pipeline, bindings, uniforms.len())?;
            }
            Command::DrawIndirect { pipeline, arguments, offset, bindings, uniforms } => {
                self.bindings(op, *pipeline, bindings, uniforms.len())?;
                self.read_range(op, *arguments, *offset, 16, "reading arguments:")?;
            }
            Command::DrawIndexedIndirect {
                pipeline,
                indices,
                index_offset,
                index_size,
                arguments,
                offset,
                bindings,
                uniforms,
            } => {
                self.bindings(op, *pipeline, bindings, uniforms.len())?;
                if *index_size == 0 {
                    return err(format!("the index range of buffer {indices} is empty"));
                }
                self.read_range(op, *indices, *index_offset, *index_size, "reading indices:")?;
                self.read_range(op, *arguments, *offset, 20, "reading arguments:")?;
            }
            Command::Present | Command::EndPass => self.attachments.clear(),
            Command::StorageRead { path, .. } | Command::StorageWrite { path, .. } => {
                if let Some(why) = path_problem(path) {
                    return err(format!("the storage path `{path}` {why}"));
                }
            }
            Command::Fetch { url, .. } | Command::Post { url, .. } => {
                if let Some(why) = path_problem(url) {
                    return err(format!("the URL `{url}` {why}"));
                }
            }
            Command::Log { .. } | Command::Label { .. } => {}
        }
        Ok(())
    }

    fn clear_colour(&self, op: Opcode, clear: &[f32; 4]) -> Result<()> {
        let names = ["r", "g", "b", "a"];
        match clear.iter().position(|c| !c.is_finite()) {
            Some(i) => Err(CommandError {
                opcode: op,
                why: format!("the clear colour's {} isn't a finite number", names[i]),
            }),
            None => Ok(()),
        }
    }

    fn create(&mut self, op: Opcode, handle: u32, r: Resource) -> Result<()> {
        if let Some(old) = self.resources.get(&handle) {
            return Err(CommandError {
                opcode: op,
                why: format!("handle {handle} already names {}", old.kind()),
            });
        }
        self.resources.insert(handle, r);
        Ok(())
    }

    fn missing(op: Opcode, what: &str, handle: u32) -> CommandError {
        CommandError { opcode: op, why: format!("there's no {what} {handle}") }
    }

    fn buffer(&self, op: Opcode, handle: u32) -> Result<u32> {
        match self.resources.get(&handle) {
            Some(Resource::Buffer { size }) => Ok(*size),
            _ => Err(Self::missing(op, "buffer", handle)),
        }
    }

    /// That texture `handle`, of format `got`, is of the format its binding names.
    fn format(
        op: Opcode,
        handle: u32,
        got: TextureFormat,
        want: Option<TextureFormat>,
    ) -> Result<()> {
        match want {
            Some(w) if w != got => Err(CommandError {
                opcode: op,
                why: format!(
                    "texture {handle} is {}, and it's bound where a {} texture goes",
                    got.name(),
                    w.name()
                ),
            }),
            _ => Ok(()),
        }
    }

    fn texture(&self, op: Opcode, handle: u32) -> Result<Texture> {
        match self.resources.get(&handle) {
            Some(Resource::Texture(t)) => Ok(*t),
            _ => Err(Self::missing(op, "texture", handle)),
        }
    }

    /// That texture `handle`, 3D or not (`is`), is 3D where the binding is (`three`), and 2D
    /// where it isn't.
    fn dimensions(op: Opcode, handle: u32, is: bool, three: bool) -> Result<()> {
        if is == three {
            return Ok(());
        }
        let (what, want) = if is { ("a 3D", "a 2D") } else { ("a 2D", "a 3D") };
        let why = format!("texture {handle} is {what} texture, bound where {want} texture goes");
        Err(CommandError { opcode: op, why })
    }

    fn sampler(&self, op: Opcode, handle: u32) -> Result<bool> {
        match self.resources.get(&handle) {
            Some(Resource::Sampler { comparison }) => Ok(*comparison),
            _ => Err(Self::missing(op, "sampler", handle)),
        }
    }

    /// Checks that `size` bytes at `offset` fit in buffer `handle`.
    fn range(&self, op: Opcode, handle: u32, offset: u32, size: u32, doing: &str) -> Result<()> {
        let total = self.buffer(op, handle)?;
        if u64::from(offset) + u64::from(size) > u64::from(total) {
            return Err(CommandError {
                opcode: op,
                why: format!(
                    "{doing} {size} bytes at offset {offset} overruns buffer {handle} ({total} bytes)"
                ),
            });
        }
        Ok(())
    }

    /// `size` bytes at `offset` in buffer `handle`, which the command reads (`doing` says what):
    /// an indirect command's arguments, or its indices.
    fn read_range(
        &mut self,
        op: Opcode,
        handle: u32,
        offset: u32,
        size: u32,
        doing: &str,
    ) -> Result<()> {
        self.range(op, handle, offset, size, doing)?;
        Self::used(&mut self.scope, op, "buffer", handle, false)
    }

    /// Adds a use of a buffer or texture (`what`) to the usage scope. (A function of the scope
    /// alone, so a caller can hold a pipeline's shape meanwhile.)
    fn used(
        scope: &mut HashMap<u32, (bool, bool)>,
        op: Opcode,
        what: &str,
        handle: u32,
        write: bool,
    ) -> Result<()> {
        let seen = scope.entry(handle).or_default();
        if write {
            seen.1 = true
        } else {
            seen.0 = true
        }
        if seen.0 && seen.1 {
            let one = if matches!(op, Opcode::Dispatch | Opcode::DispatchIndirect) {
                "dispatch"
            } else {
                "pass"
            };
            return Err(CommandError {
                opcode: op,
                why: format!("{what} {handle} is used both read-only and read-write in one {one}"),
            });
        }
        Ok(())
    }

    /// Checks a dispatch's or draw's bindings, and adds what they use to the usage scope.
    fn bindings(
        &mut self,
        op: Opcode,
        pipeline: u32,
        bindings: &[Binding],
        uniform_len: usize,
    ) -> Result<()> {
        let err = |why: String| Err(CommandError { opcode: op, why });
        let Some(p) = self.pipelines.get(pipeline as usize) else {
            return err(format!(
                "there's no pipeline {pipeline} (the manifest has {})",
                self.pipelines.len()
            ));
        };
        let name = &p.name;
        let dispatch = matches!(op, Opcode::Dispatch | Opcode::DispatchIndirect);
        if p.compute != dispatch {
            let (is, needs) = if p.compute { ("compute", "render") } else { ("render", "compute") };
            return err(format!(
                "pipeline {pipeline} ({name}) is a {is} pipeline; {} needs a {needs} pipeline",
                op.name()
            ));
        }
        // A draw's pipeline is one the pass's targets take.
        let (colour, depth) = self.targets;
        if !dispatch && p.writes_depth && !depth {
            return err(format!(
                "`{name}` gives its fragments their depth, so it's drawn in a pass with a depth \
                 target, and this pass has none"
            ));
        }
        if !dispatch
            && let Some(c) = colour
            && p.uint != (c == TextureFormat::R32Uint)
        {
            let (gives, wants) = if p.uint {
                ("a `u32`", "an r32uint target")
            } else {
                ("a colour", "a colour target, not an integer one")
            };
            return err(format!(
                "`{name}`'s fragment shader returns {gives}, so it's drawn into {wants}"
            ));
        }
        if bindings.len() != p.bindings.len() {
            let (want, got) = (p.bindings.len(), bindings.len());
            return err(format!(
                "pipeline {pipeline} ({name}) binds {want} resources, but the command lists {got}"
            ));
        }
        if uniform_len != p.uniform_size {
            let want = p.uniform_size;
            return err(format!(
                "pipeline {pipeline} ({name}) takes {want} uniform bytes, but the command has {uniform_len}"
            ));
        }
        let mut written = Vec::new();
        for (b, &(kind, format)) in bindings.iter().zip(&p.bindings) {
            if self.attachments.contains(&b.handle) {
                return err(format!(
                    "texture {} is the pass's target, so its draws can't bind it",
                    b.handle
                ));
            }
            match kind {
                BindingKind::Read | BindingKind::ReadWrite => {
                    self.range(op, b.handle, b.offset, b.size, "binding")?;
                    if b.size == 0 || b.size % 4 != 0 {
                        return err(format!(
                            "a bound range of buffer {} is {} bytes: a positive multiple of 4",
                            b.handle, b.size
                        ));
                    }
                    if b.offset % BINDING_OFFSET_ALIGNMENT != 0 {
                        return err(format!(
                            "a bound range of buffer {} starts at {}, not a multiple of {BINDING_OFFSET_ALIGNMENT} bytes",
                            b.handle, b.offset
                        ));
                    }
                    let write = kind == BindingKind::ReadWrite;
                    if write {
                        if written.contains(&b.handle) {
                            let one = if dispatch { "dispatch" } else { "draw" };
                            return err(format!(
                                "buffer {} is bound read-write twice in one {one}",
                                b.handle
                            ));
                        }
                        written.push(b.handle);
                    }
                    Self::used(&mut self.scope, op, "buffer", b.handle, write)?;
                }
                BindingKind::Texture
                | BindingKind::DepthTexture
                | BindingKind::Texture3d
                | BindingKind::StorageTexture
                | BindingKind::StorageTexture3d => {
                    let t = self.texture(op, b.handle)?;
                    let write = kind.is_storage();
                    if write && !t.writable {
                        return err(format!(
                            "texture {} is bound where a kernel writes it, but wasn't made writable",
                            b.handle
                        ));
                    }
                    let depth = kind == BindingKind::DepthTexture;
                    if !write && t.format.is_depth() != depth {
                        let want = if depth { "a depth texture" } else { "a colour texture" };
                        return err(format!("texture {} is bound where {want} goes", b.handle));
                    }
                    Self::format(op, b.handle, t.format, format)?;
                    Self::dimensions(op, b.handle, t.depth > 0, kind.is_3d())?;
                    Self::used(&mut self.scope, op, "texture", b.handle, write)?;
                }
                BindingKind::Sampler | BindingKind::ComparisonSampler => {
                    let comparison = self.sampler(op, b.handle)?;
                    if comparison != (kind == BindingKind::ComparisonSampler) {
                        let want = if comparison { "a filtering" } else { "a comparison" };
                        return err(format!(
                            "sampler {} is bound where {want} sampler goes",
                            b.handle
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}
