//! Checks each command against the manifest and the program's buffers before it runs.
//!
//! WebGPU reports most misuse asynchronously, or as a device error that names no command; wgpu
//! reports it as a validation error. Checking up front means both hosts reject the same
//! programs, at the command that's wrong, with the same messages (the browser runtime's
//! `src/check.ts` mirrors this file):
//!
//! - `CreateBuffer`: the handle is new; the size is within WebGPU's default limits.
//! - `WriteBuffer`: the buffer exists; the write fits inside it.
//! - `DestroyBuffer`: the buffer exists; later commands can't use it.
//! - `Dispatch`, `Draw`: the pipeline exists and is the right kind; the command lists as many
//!   buffers as the pipeline binds, each one existing, and exactly its uniform block's size in
//!   uniform bytes; a dispatch's group counts are within the limit.
//! - No buffer is bound both read-only and read-write in one usage scope: a dispatch, or a whole
//!   screen pass (WebGPU's rule).

use crate::error::{Error, Result};
use std::collections::HashMap;
use wrela_abi::Manifest;
use wrela_abi::manifest::{Access, Stage};
use wrela_abi::stream::{Command, Opcode};

/// The device limits the checks use: WebGPU's defaults, which both hosts request.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// The largest buffer: the lesser of `maxBufferSize` and `maxStorageBufferBindingSize`.
    pub max_buffer_size: u64,
    pub max_workgroups_per_dimension: u32,
}

impl Limits {
    pub(crate) fn webgpu_defaults() -> Limits {
        let l = wgpu::Limits::default();
        Limits {
            max_buffer_size: l.max_buffer_size.min(l.max_storage_buffer_binding_size),
            max_workgroups_per_dimension: l.max_compute_workgroups_per_dimension,
        }
    }
}

struct Shape {
    name: String,
    compute: bool,
    uniform_size: usize,
    buffers: Vec<Access>,
}

pub(crate) struct Checker {
    pipelines: Vec<Shape>,
    /// Each buffer's size.
    buffers: HashMap<u32, u32>,
    /// In a screen pass: each bound buffer's access so far, (read-only, read-write).
    pass: Option<HashMap<u32, (bool, bool)>>,
    limits: Limits,
}

impl Checker {
    pub(crate) fn new(manifest: &Manifest, limits: Limits) -> Checker {
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
        Checker { pipelines, buffers: HashMap::new(), pass: None, limits }
    }

    /// Checks a command that has passed the [`wrela_abi::stream::Sequencer`], and records its
    /// effect on what later commands may do.
    pub(crate) fn check(&mut self, cmd: &Command<'_>) -> Result<()> {
        let op = cmd.opcode();
        let err = |why: String| Err(Error::command(op, why));
        match cmd {
            Command::CreateBuffer { handle, size } => {
                if self.buffers.contains_key(handle) {
                    return err(format!("buffer {handle} already exists"));
                }
                let limit = self.limits.max_buffer_size;
                if u64::from(*size) > limit {
                    return err(format!("buffer {handle} is {size} bytes; the limit is {limit}"));
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
                self.binding(op, *pipeline, buffers, uniforms.len(), &mut HashMap::new())?;
                let max = self.limits.max_workgroups_per_dimension;
                if groups.iter().any(|&g| g > max) {
                    let [x, y, z] = groups;
                    return err(format!(
                        "{x}x{y}x{z} workgroups is over the limit of {max} per dimension"
                    ));
                }
            }
            Command::BeginScreenPass { .. } => self.pass = Some(HashMap::new()),
            Command::Draw { pipeline, buffers, uniforms, .. } => {
                let mut usage = self.pass.take().unwrap_or_default();
                let checked = self.binding(op, *pipeline, buffers, uniforms.len(), &mut usage);
                self.pass = Some(usage);
                checked?;
            }
            Command::Present => self.pass = None,
        }
        Ok(())
    }

    fn size(&self, op: Opcode, handle: u32) -> Result<u32> {
        self.buffers
            .get(&handle)
            .copied()
            .ok_or_else(|| Error::command(op, format!("there's no buffer {handle}")))
    }

    fn binding(
        &self,
        op: Opcode,
        pipeline: u32,
        buffers: &[u32],
        uniform_len: usize,
        usage: &mut HashMap<u32, (bool, bool)>,
    ) -> Result<()> {
        let err = |why: String| Err(Error::command(op, why));
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
        let scope = if op == Opcode::Dispatch { "dispatch" } else { "screen pass" };
        for (&handle, &access) in buffers.iter().zip(&p.buffers) {
            self.size(op, handle)?;
            let seen = usage.entry(handle).or_default();
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
