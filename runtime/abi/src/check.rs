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
//!   inside their buffer; a draw's pass's targets are one of its pipeline's (the manifest's
//!   `targets`).
//! - **Usage scopes** (WebGPU's rule): no buffer or texture is used both read-only and
//!   read-write in one dispatch or one pass, none is bound read-write twice by one command, and
//!   a pass's attachments aren't bound by its draws. An indirect command's arguments buffer and
//!   an indexed draw's index buffer are read-only use.
//! - **Passes:** the clear colour and depth are finite; a pass's attachments exist, the colour
//!   one isn't a depth texture and the depth one is, and they're the same size.
//! - **Requests:** a storage path or a URL is relative, `/`-separated, with no empty, `.` or
//!   `..` parts ([`path_problem`]).
//!
//! A host that holds an exported call's commands until it returns brackets its checks with
//! [`Checker::begin_call`] and [`Checker::end_call`]. A trapped call discards its commands and
//! restores the checker's resources to those from before the call (language.md §18.3).

use crate::Manifest;
use crate::manifest::{BindingKind, RenderTarget, Stage};
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

/// What's wrong with the path of a build to hand over to, if anything: as [`path_problem`]'s,
/// except that it may start with one `..` part, so a build can name one beside it (a game's
/// builds sit side by side under one origin). One at most: each hand-over then climbs one
/// directory and goes down at least one, so no build is reached above the first one's site.
pub fn build_path_problem(path: &str) -> Option<&'static str> {
    let rest = path.strip_prefix("../").unwrap_or(path);
    if rest == ".." || rest.starts_with("../") {
        return Some("climbs more than one directory: a build names its own or one beside it");
    }
    path_problem(rest)
}

struct Shape {
    name: String,
    compute: bool,
    uniform_size: usize,
    /// Each binding's kind, and a colour texture's format.
    bindings: Vec<(BindingKind, Option<TextureFormat>)>,
    /// A render pipeline's targets: the passes it may be drawn in.
    targets: Vec<RenderTarget>,
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
    /// Successful creates and destroys in the current exported call, reversed if it traps.
    resource_undo: Vec<(u32, Option<Resource>)>,
    in_call: bool,
    /// The current usage scope, a dispatch or the open pass: each buffer's or texture's use
    /// so far, (read-only, read-write). Dispatches happen only outside a pass (the sequencer
    /// checks), so the two kinds of scope never overlap and share this map.
    scope: HashMap<u32, (bool, bool)>,
    /// An empty map for a new scope: the previous scope stays intact until the command passes.
    spare_scope: HashMap<u32, (bool, bool)>,
    /// The open pass's attachments.
    attachments: Vec<u32>,
    /// Read-write buffers in this command: reused across draws and dispatches.
    written: Vec<u32>,
    /// The open pass's colour target's format (the screen's, a texture's, or none), and whether
    /// it has a depth target: which pipelines its draws may draw with.
    targets: (Option<TextureFormat>, bool),
    /// The current frame's screen size, when the host has begun a frame.
    screen_size: Option<(u32, u32)>,
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
                let targets = match &p.stage {
                    Stage::Render { targets, .. } => targets.clone(),
                    Stage::Compute { .. } => Vec::new(),
                };
                Shape {
                    name: p.name.clone(),
                    compute: matches!(p.stage, Stage::Compute { .. }),
                    uniform_size: p.uniform.as_ref().map_or(0, |u| u.size as usize),
                    bindings: p.bindings.iter().map(|b| (b.kind, b.format)).collect(),
                    targets,
                }
            })
            .collect();
        Checker {
            limits,
            pipelines,
            resources: HashMap::new(),
            resource_undo: Vec::new(),
            in_call: false,
            scope: HashMap::new(),
            spare_scope: HashMap::new(),
            attachments: Vec::new(),
            written: Vec::new(),
            targets: (None, false),
            screen_size: None,
        }
    }

    /// The screen's dimensions for the next frame's pass checks.
    pub fn set_screen_size(&mut self, width: u32, height: u32) {
        self.screen_size = Some((width, height));
    }

    /// Starts checking an exported call whose commands the host holds until it returns.
    pub fn begin_call(&mut self) {
        debug_assert!(!self.in_call);
        debug_assert!(self.resource_undo.is_empty());
        self.in_call = true;
    }

    /// Keeps a successful call's resource changes, or restores the resources it started with
    /// when the call traps and its held commands are discarded.
    pub fn end_call(&mut self, commit: bool) {
        debug_assert!(self.in_call);
        self.in_call = false;
        if commit {
            self.resource_undo.clear();
            return;
        }
        for (handle, previous) in self.resource_undo.drain(..).rev() {
            match previous {
                Some(resource) => {
                    self.resources.insert(handle, resource);
                }
                None => {
                    self.resources.remove(&handle);
                }
            }
        }
        self.scope.clear();
        self.spare_scope.clear();
        self.attachments.clear();
        self.written.clear();
        self.targets = (None, false);
    }

    /// Checks a command that has passed the [`crate::stream::Sequencer`], and records its
    /// effect on what later commands may do.
    pub fn check(&mut self, cmd: &Command<'_>) -> Result<()> {
        // A rejected command leaves no trace. Binding checks add uses as they go, and a later
        // binding (or an indirect range) may still reject the command.
        // A draw can only change the entries it names, so don't clone a pass's whole (possibly
        // large) scope for every draw. Commands that start a fresh scope swap in an empty,
        // reusable map and keep the previous scope intact until they pass.
        let whole_scope = matches!(
            cmd,
            Command::Dispatch { .. } | Command::DispatchIndirect { .. } | Command::BeginPass(_)
        );
        if whole_scope {
            debug_assert!(self.spare_scope.is_empty());
            std::mem::swap(&mut self.scope, &mut self.spare_scope);
        }
        let pass_state =
            matches!(cmd, Command::BeginPass(_)).then(|| (self.attachments.clone(), self.targets));
        let mut entries = Vec::new();
        {
            let mut remember = |handle: u32| {
                if !entries.iter().any(|(h, _)| *h == handle) {
                    entries.push((handle, self.scope.get(&handle).copied()));
                }
            };
            match cmd {
                Command::Draw { bindings, .. } => {
                    for b in bindings {
                        remember(b.handle);
                    }
                }
                Command::DrawIndirect { arguments, bindings, .. } => {
                    for b in bindings {
                        remember(b.handle);
                    }
                    remember(*arguments);
                }
                Command::DrawIndexedIndirect { indices, arguments, bindings, .. } => {
                    for b in bindings {
                        remember(b.handle);
                    }
                    remember(*indices);
                    remember(*arguments);
                }
                _ => {}
            }
        }
        let result = self.check_in_place(cmd);
        if result.is_err() {
            if whole_scope {
                std::mem::swap(&mut self.scope, &mut self.spare_scope);
            } else {
                for (handle, used) in entries {
                    match used {
                        Some(used) => {
                            self.scope.insert(handle, used);
                        }
                        None => {
                            self.scope.remove(&handle);
                        }
                    }
                }
            }
            if let Some((attachments, targets)) = pass_state {
                self.attachments = attachments;
                self.targets = targets;
            }
        }
        if whole_scope {
            self.spare_scope.clear();
        }
        result
    }

    fn check_in_place(&mut self, cmd: &Command<'_>) -> Result<()> {
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
                self.destroy(*handle);
            }
            Command::DestroyTexture { handle } => {
                self.texture(op, *handle)?;
                self.destroy(*handle);
            }
            Command::DestroySampler { handle } => {
                self.sampler(op, *handle)?;
                self.destroy(*handle);
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
            // A dispatch and a pass start from the empty scope `check` swapped in.
            Command::Dispatch { pipeline, groups, bindings, uniforms } => {
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
                if pass.depth != NONE
                    && !pass.keep_depth
                    && !(0.0..=1.0).contains(&pass.clear_depth)
                {
                    return err("the clear depth isn't from 0 to 1".into());
                }
                self.attachments.clear();
                let mut size = if pass.color == SCREEN { self.screen_size } else { None };
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
            Command::Prepare { path, .. }
            | Command::Unprepare { path }
            | Command::HandOver { path, .. } => {
                if let Some(why) = build_path_problem(path) {
                    return err(format!("the build path `{path}` {why}"));
                }
            }
            Command::HostRequest { service, .. } => {
                if let Some(why) = path_problem(service) {
                    return err(format!("the host service `{service}` {why}"));
                }
            }
            Command::Label { .. } => {}
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
        if self.in_call {
            self.resource_undo.push((handle, None));
        }
        Ok(())
    }

    /// Removes a resource whose kind the command has already checked.
    fn destroy(&mut self, handle: u32) {
        let previous = self.resources.remove(&handle);
        if self.in_call {
            self.resource_undo.push((handle, previous));
        }
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
        // A draw's pass's targets are one of its pipeline's.
        let (color, depth) = self.targets;
        let here = RenderTarget { color, depth };
        if !dispatch && !p.targets.contains(&here) {
            let theirs: Vec<String> = p.targets.iter().map(|t| t.to_string()).collect();
            return err(format!(
                "`{name}` is drawn into {here}, which isn't one of its targets ({})",
                theirs.join(", ")
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
        self.written.clear();
        for (b, &(kind, format)) in bindings.iter().zip(&p.bindings) {
            if self.attachments.contains(&b.handle) {
                return err(format!(
                    "texture {} is the pass's target, so its draws can't bind it",
                    b.handle
                ));
            }
            if !kind.is_buffer() && (b.offset != 0 || b.size != 0) {
                return err(format!(
                    "a texture or sampler binding has offset {} and size {}: both must be 0",
                    b.offset, b.size
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
                        if self.written.contains(&b.handle) {
                            let one = if dispatch { "dispatch" } else { "draw" };
                            return err(format!(
                                "buffer {} is bound read-write twice in one {one}",
                                b.handle
                            ));
                        }
                        self.written.push(b.handle);
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::stream::Pass;

    #[test]
    fn aborted_calls_restore_created_destroyed_and_reused_resources() {
        let mut checker = Checker::new(&crate::vectors::check_manifest());
        checker.check(&Command::CreateBuffer { handle: 1, size: 64 }).expect("buffer");
        checker
            .check(&Command::CreateTexture {
                handle: 2,
                width: 8,
                height: 8,
                format: TextureFormat::Rgba8,
                writable: false,
                depth: 0,
            })
            .expect("texture");
        checker
            .check(&Command::CreateSampler {
                handle: 3,
                linear: true,
                repeat: false,
                compare: None,
            })
            .expect("sampler");

        checker.begin_call();
        for size in [128, 256] {
            checker.check(&Command::DestroyBuffer { handle: 1 }).expect("destroy buffer");
            checker.check(&Command::CreateBuffer { handle: 1, size }).expect("reuse handle");
        }
        checker.check(&Command::DestroyTexture { handle: 2 }).expect("destroy texture");
        checker.check(&Command::CreateBuffer { handle: 2, size: 32 }).expect("change kind");
        checker.check(&Command::DestroySampler { handle: 3 }).expect("destroy sampler");
        checker
            .check(&Command::CreateTexture {
                handle: 3,
                width: 8,
                height: 8,
                format: TextureFormat::Rgba8,
                writable: false,
                depth: 0,
            })
            .expect("replace sampler with target");
        checker.check(&Command::CreateBuffer { handle: 4, size: 16 }).expect("new buffer");
        checker.check(&Command::CreateBuffer { handle: 5, size: 16 }).expect("temporary buffer");
        checker.check(&Command::DestroyBuffer { handle: 5 }).expect("drop temporary buffer");
        checker
            .check(&Command::BeginPass(Pass {
                color: 3,
                keep_color: false,
                join: false,
                clear: [0.0; 4],
                depth: NONE,
                keep_depth: false,
                clear_depth: 1.0,
            }))
            .expect("unfinished pass");
        checker
            .check(&Command::Draw {
                pipeline: 0,
                vertices: 3,
                instances: 1,
                bindings: vec![Binding::range(1, 0, 256), Binding::range(2, 0, 32)],
                uniforms: &[0; 16],
            })
            .expect("draw before trap");
        checker.end_call(false);

        assert_eq!(checker.buffer(Opcode::ReadBuffer, 1), Ok(64));
        assert_eq!(checker.texture(Opcode::BeginPass, 2).expect("restored texture").width, 8);
        assert_eq!(checker.sampler(Opcode::DestroySampler, 3), Ok(false));
        for handle in [4, 5] {
            assert!(checker.buffer(Opcode::ReadBuffer, handle).is_err());
        }
        assert!(checker.scope.is_empty());
        assert!(checker.spare_scope.is_empty());
        assert!(checker.attachments.is_empty());
        assert!(checker.written.is_empty());
        assert_eq!(checker.targets, (None, false));
        assert!(checker.resource_undo.is_empty());
    }

    #[test]
    fn committed_calls_keep_resources_and_do_not_affect_later_rollbacks() {
        let mut checker = Checker::new(&Manifest::new("game.wasm"));
        checker.check(&Command::CreateBuffer { handle: 1, size: 64 }).expect("original buffer");
        checker.begin_call();
        checker.check(&Command::DestroyBuffer { handle: 1 }).expect("destroy original");
        checker.check(&Command::CreateBuffer { handle: 2, size: 32 }).expect("committed buffer");
        checker.end_call(true);
        assert!(checker.buffer(Opcode::ReadBuffer, 1).is_err());
        assert_eq!(checker.buffer(Opcode::ReadBuffer, 2), Ok(32));

        checker.begin_call();
        assert!(checker.check(&Command::CreateBuffer { handle: 2, size: 128 }).is_err());
        assert!(checker.check(&Command::DestroyBuffer { handle: 1 }).is_err());
        checker.check(&Command::CreateBuffer { handle: 1, size: 16 }).expect("aborted buffer");
        checker.end_call(false);
        assert!(checker.buffer(Opcode::ReadBuffer, 1).is_err());
        assert_eq!(checker.buffer(Opcode::ReadBuffer, 2), Ok(32));
        assert!(checker.resource_undo.is_empty());
    }

    #[test]
    fn textures_and_samplers_cannot_bind_buffer_ranges() {
        let mut checker = Checker::new(&crate::vectors::sample_manifest());
        checker
            .check(&Command::CreateTexture {
                handle: 1,
                width: 1,
                height: 1,
                format: TextureFormat::Rgba8,
                writable: false,
                depth: 0,
            })
            .expect("texture");
        checker
            .check(&Command::CreateSampler {
                handle: 2,
                linear: true,
                repeat: false,
                compare: None,
            })
            .expect("sampler");
        checker.check(&Command::BeginScreenPass { clear: [0.0; 4] }).expect("pass");
        for slot in 0..2 {
            for (offset, size) in [(4, 0), (0, 4), (4, 4)] {
                let mut bindings = vec![Binding::of(1), Binding::of(2)];
                bindings[slot].offset = offset;
                bindings[slot].size = size;
                let error = checker
                    .check(&Command::Draw {
                        pipeline: 1,
                        vertices: 3,
                        instances: 1,
                        bindings,
                        uniforms: &[],
                    })
                    .expect_err("a texture or sampler has no buffer range");
                assert_eq!(
                    error.why,
                    format!(
                        "a texture or sampler binding has offset {offset} and size {size}: both must be 0"
                    )
                );
            }
        }
        checker
            .check(&Command::Draw {
                pipeline: 1,
                vertices: 3,
                instances: 1,
                bindings: vec![Binding::of(1), Binding::of(2)],
                uniforms: &[],
            })
            .expect("valid bindings after rejected ranges");
    }

    #[test]
    fn used_depth_clear_is_from_zero_to_one() {
        let mut checker = Checker::new(&Manifest::new("game.wasm"));
        checker
            .check(&Command::CreateTexture {
                handle: 1,
                width: 8,
                height: 8,
                format: TextureFormat::Depth32Float,
                writable: false,
                depth: 0,
            })
            .expect("depth texture");
        let pass = Pass {
            color: SCREEN,
            keep_color: false,
            join: false,
            clear: [0.0; 4],
            depth: 1,
            keep_depth: false,
            clear_depth: 1.0,
        };
        for clear_depth in [-0.01, 1.01, f32::MIN, f32::MAX] {
            assert_eq!(
                checker
                    .check(&Command::BeginPass(Pass { clear_depth, ..pass }))
                    .expect_err("depth clear outside range")
                    .why,
                "the clear depth isn't from 0 to 1"
            );
        }
        for clear_depth in [0.0, 1.0] {
            checker.check(&Command::BeginPass(Pass { clear_depth, ..pass })).expect("depth clear");
            checker.check(&Command::Present).expect("end pass");
        }
        for pass in [
            Pass { clear_depth: 2.0, keep_depth: true, ..pass },
            Pass { clear_depth: 2.0, depth: NONE, ..pass },
        ] {
            checker.check(&Command::BeginPass(pass)).expect("unused finite depth clear");
            checker.check(&Command::Present).expect("end pass");
        }
    }

    #[test]
    fn screen_pass_depth_must_match_the_current_frame_size() {
        let mut checker = Checker::new(&Manifest::new("game.wasm"));
        checker
            .check(&Command::CreateTexture {
                handle: 1,
                width: 8,
                height: 8,
                format: TextureFormat::Depth32Float,
                writable: false,
                depth: 0,
            })
            .expect("depth texture");
        let pass = Command::BeginPass(Pass {
            color: SCREEN,
            keep_color: false,
            join: false,
            clear: [0.0; 4],
            depth: 1,
            keep_depth: false,
            clear_depth: 1.0,
        });
        checker.set_screen_size(16, 8);
        assert_eq!(
            checker.check(&pass).expect_err("different attachment sizes").why,
            "the pass's colour and depth targets differ in size"
        );
        checker.set_screen_size(8, 8);
        checker.check(&pass).expect("matching attachment sizes");
    }

    #[test]
    fn writable_bindings_are_checked_per_command_even_after_an_error() {
        let mut checker = Checker::new(&crate::vectors::check_manifest());
        for handle in 1..=2 {
            checker.check(&Command::CreateBuffer { handle, size: 64 }).expect("created");
        }
        let dispatch = |bindings| Command::Dispatch {
            pipeline: 3,
            groups: [1; 3],
            bindings,
            uniforms: &[0; 16],
        };
        let a = Binding::range(1, 0, 64);
        let b = Binding::range(2, 0, 64);
        checker.check(&dispatch(vec![a, b])).expect("two distinct outputs");
        checker.check(&dispatch(vec![a, b])).expect("the next dispatch can write them again");
        let error = checker.check(&dispatch(vec![a, a])).expect_err("a duplicate output");
        assert_eq!(error.why, "buffer 1 is bound read-write twice in one dispatch");
        checker.check(&dispatch(vec![a, b])).expect("a rejected command does not retain outputs");
    }

    #[test]
    fn rejected_dispatches_restore_the_previous_scope_after_partial_checks() {
        let mut checker = Checker::new(&crate::vectors::check_manifest());
        for handle in 1..=4 {
            checker.check(&Command::CreateBuffer { handle, size: 64 }).expect("buffer");
        }
        let dispatch = |groups, a, b| Command::Dispatch {
            pipeline: 1,
            groups,
            bindings: vec![Binding::range(a, 0, 64), Binding::range(b, 0, 64)],
            uniforms: &[0; 16],
        };
        checker.check(&dispatch([1; 3], 1, 2)).expect("initial scope");
        let previous = checker.scope.clone();
        for _ in 0..4 {
            let indirect = Command::DispatchIndirect {
                pipeline: 1,
                arguments: 99,
                offset: 0,
                bindings: vec![Binding::range(3, 0, 64), Binding::range(4, 0, 64)],
                uniforms: &[0; 16],
            };
            assert_eq!(
                checker.check(&indirect).expect_err("missing arguments after binding checks").why,
                "there's no buffer 99"
            );
            assert_eq!(checker.scope, previous);
            let max = checker.limits.max_workgroups_per_dimension;
            assert_eq!(
                checker.check(&dispatch([max + 1, 1, 1], 3, 4)).expect_err("too many groups").why,
                format!("{}x1x1 workgroups is over the limit of {max} per dimension", max + 1)
            );
            assert_eq!(checker.scope, previous);
        }
        checker.check(&dispatch([1; 3], 3, 4)).expect("a new scope after rejected dispatches");
        assert_eq!(checker.scope.len(), 2);
        assert_eq!(checker.scope.get(&3), Some(&(true, false)));
        assert_eq!(checker.scope.get(&4), Some(&(false, true)));
        checker.check(&dispatch([1; 3], 2, 1)).expect("the next dispatch can reverse uses");
        assert_eq!(checker.scope.get(&1), Some(&(false, true)));
        assert_eq!(checker.scope.get(&2), Some(&(true, false)));
    }

    #[test]
    fn rejected_draws_and_passes_leave_checker_state_unchanged() {
        let mut checker = Checker::new(&crate::vectors::check_manifest());
        checker.check(&Command::CreateBuffer { handle: 1, size: 64 }).expect("buffer 1");
        checker.check(&Command::CreateBuffer { handle: 2, size: 64 }).expect("buffer 2");
        checker.check(&Command::BeginScreenPass { clear: [0.0; 4] }).expect("pass");
        let bad = Command::Draw {
            pipeline: 0,
            vertices: 3,
            instances: 1,
            bindings: vec![Binding::range(1, 0, 64), Binding::range(99, 0, 64)],
            uniforms: &[0; 16],
        };
        assert_eq!(checker.check(&bad).expect_err("missing buffer").why, "there's no buffer 99");

        let good = Command::Draw {
            pipeline: 2,
            vertices: 3,
            instances: 1,
            bindings: vec![Binding::range(1, 0, 64), Binding::range(2, 0, 64)],
            uniforms: &[0; 16],
        };
        checker.check(&good).expect("the rejected draw left no read-only use behind");

        checker.check(&Command::Present).expect("end pass");
        checker
            .check(&Command::CreateTexture {
                handle: 3,
                width: 8,
                height: 8,
                format: TextureFormat::Rgba8,
                writable: false,
                depth: 0,
            })
            .expect("texture");
        checker.check(&Command::BeginScreenPass { clear: [0.0; 4] }).expect("screen pass");
        checker.check(&Command::Present).expect("end screen pass");
        let bad_pass = Command::BeginPass(crate::stream::Pass {
            color: 3,
            keep_color: false,
            join: false,
            clear: [0.0; 4],
            depth: 99,
            keep_depth: false,
            clear_depth: 1.0,
        });
        assert_eq!(
            checker.check(&bad_pass).expect_err("missing depth target").why,
            "there's no texture 99"
        );
        assert!(checker.attachments.is_empty());
        assert_eq!(checker.targets, (Some(SCREEN_FORMAT), false));
    }
}
