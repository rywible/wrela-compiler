//! Checks each command against the manifest and the program's buffers before it runs.
//!
//! WebGPU reports most misuse asynchronously, or as a device error that names no command; wgpu
//! reports it as a validation error. Checking up front means both hosts reject the same
//! programs, at the command that's wrong, with the same messages (the browser runtime's
//! `src/check.ts` mirrors this file, and the test vectors hold both to it):
//!
//! - `CreateBuffer`: the handle is new; the size is within WebGPU's default limits.
//! - `WriteBuffer`: the buffer exists; the write fits inside it.
//! - `DestroyBuffer`: the buffer exists; later commands can't use it.
//! - `Dispatch`, `Draw`: the pipeline exists and is the right kind; the command lists as many
//!   buffers as the pipeline binds, each one existing, and exactly its uniform block's size in
//!   uniform bytes; a dispatch's group counts are within the limit.
//! - No buffer is bound both read-only and read-write in one usage scope: a dispatch, or a whole
//!   screen pass (WebGPU's rule); and none is bound read-write twice by one dispatch or draw
//!   (WebGPU's rule against aliased writable bindings, which wgpu doesn't check).
//! - `BeginScreenPass`: the clear colour is finite (WebGPU's `GPUColor` is; wgpu takes any).

use crate::Manifest;
use crate::manifest::{Access, Stage};
use crate::stream::{Command, Opcode};
use std::collections::HashMap;
use std::fmt;

/// WebGPU's default limits that commands are checked against (both hosts request a device with
/// the defaults). The largest buffer is the lesser of `maxBufferSize` (256 MiB) and
/// `maxStorageBufferBindingSize` (128 MiB): every buffer is bound as storage.
pub const MAX_BUFFER_SIZE: u32 = 134_217_728;
pub const MAX_WORKGROUPS_PER_DIMENSION: u32 = 65_535;

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

struct Shape {
    name: String,
    compute: bool,
    uniform_size: usize,
    buffers: Vec<Access>,
}

pub struct Checker {
    pipelines: Vec<Shape>,
    /// Each buffer's size.
    buffers: HashMap<u32, u32>,
    /// The current usage scope, a dispatch or the open screen pass: each bound buffer's access
    /// so far, (read-only, read-write). Dispatches happen only outside a screen pass (the
    /// sequencer checks), so the two kinds of scope never overlap and share this map.
    scope: HashMap<u32, (bool, bool)>,
}

impl Checker {
    pub fn new(manifest: &Manifest) -> Checker {
        let pipelines = manifest
            .pipelines
            .iter()
            .map(|p| Shape {
                name: p.name.clone(),
                compute: matches!(p.stage, Stage::Compute { .. }),
                uniform_size: p.uniform.as_ref().map_or(0, |u| u.size as usize),
                buffers: p.buffers.iter().map(|b| b.access).collect(),
            })
            .collect();
        Checker { pipelines, buffers: HashMap::new(), scope: HashMap::new() }
    }

    /// Checks a command that has passed the [`crate::stream::Sequencer`], and records its
    /// effect on what later commands may do.
    pub fn check(&mut self, cmd: &Command<'_>) -> Result<()> {
        let op = cmd.opcode();
        let err = |why: String| Err(CommandError { opcode: op, why });
        match cmd {
            Command::CreateBuffer { handle, size } => {
                if self.buffers.contains_key(handle) {
                    return err(format!("buffer {handle} already exists"));
                }
                if *size > MAX_BUFFER_SIZE {
                    return err(format!(
                        "buffer {handle} is {size} bytes; the limit is {MAX_BUFFER_SIZE}"
                    ));
                }
                self.buffers.insert(*handle, *size);
            }
            Command::DestroyBuffer { handle } => {
                self.size(op, *handle)?;
                self.buffers.remove(handle);
            }
            Command::WriteBuffer { handle, offset, data } => {
                let size = self.size(op, *handle)?;
                if u64::from(*offset) + data.len() as u64 > u64::from(size) {
                    return err(format!(
                        "writing {} bytes at offset {offset} overruns buffer {handle} ({size} bytes)",
                        data.len()
                    ));
                }
            }
            Command::Dispatch { pipeline, groups, buffers, uniforms } => {
                self.scope.clear();
                self.binding(op, *pipeline, buffers, uniforms.len())?;
                let max = MAX_WORKGROUPS_PER_DIMENSION;
                if groups.iter().any(|&g| g > max) {
                    let [x, y, z] = groups;
                    return err(format!(
                        "{x}x{y}x{z} workgroups is over the limit of {max} per dimension"
                    ));
                }
            }
            Command::BeginScreenPass { clear } => {
                let names = ["r", "g", "b", "a"];
                if let Some(i) = clear.iter().position(|c| !c.is_finite()) {
                    return err(format!("the clear colour's {} isn't a finite number", names[i]));
                }
                self.scope.clear();
            }
            Command::Draw { pipeline, buffers, uniforms, .. } => {
                self.binding(op, *pipeline, buffers, uniforms.len())?;
            }
            Command::Present => {}
        }
        Ok(())
    }

    fn size(&self, op: Opcode, handle: u32) -> Result<u32> {
        self.buffers
            .get(&handle)
            .copied()
            .ok_or_else(|| CommandError { opcode: op, why: format!("there's no buffer {handle}") })
    }

    /// Checks a dispatch's or draw's bindings, and adds its buffers to the usage scope.
    fn binding(
        &mut self,
        op: Opcode,
        pipeline: u32,
        buffers: &[u32],
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
        match (p.compute, op) {
            (true, Opcode::Draw) => {
                return err(format!(
                    "pipeline {pipeline} ({name}) is a compute pipeline; Draw needs a render pipeline"
                ));
            }
            (false, Opcode::Dispatch) => {
                return err(format!(
                    "pipeline {pipeline} ({name}) is a render pipeline; Dispatch needs a compute pipeline"
                ));
            }
            _ => {}
        }
        if buffers.len() != p.buffers.len() {
            let (want, got) = (p.buffers.len(), buffers.len());
            return err(format!(
                "pipeline {pipeline} ({name}) binds {want} buffers, but the command lists {got}"
            ));
        }
        if uniform_len != p.uniform_size {
            let want = p.uniform_size;
            return err(format!(
                "pipeline {pipeline} ({name}) takes {want} uniform bytes, but the command has {uniform_len}"
            ));
        }
        let (scope, one) =
            if op == Opcode::Dispatch { ("dispatch", "dispatch") } else { ("screen pass", "draw") };
        let mut written = Vec::new();
        for (&handle, &access) in buffers.iter().zip(&p.buffers) {
            self.size(op, handle)?;
            if access == Access::ReadWrite {
                if written.contains(&handle) {
                    return err(format!("buffer {handle} is bound read-write twice in one {one}"));
                }
                written.push(handle);
            }
            let seen = self.scope.entry(handle).or_default();
            match access {
                Access::Read => seen.0 = true,
                Access::ReadWrite => seen.1 = true,
            }
            if seen.0 && seen.1 {
                return err(format!(
                    "buffer {handle} is bound both read-only and read-write in one {scope}"
                ));
            }
        }
        Ok(())
    }
}
