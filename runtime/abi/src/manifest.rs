//! # The manifest, version 1
//!
//! `manifest.json` describes everything game-specific a host needs besides the WASM: each
//! pipeline's shader, entry points and bindings (D-099). The runtime reads it at load; nothing
//! is generated from it per game.
//!
//! - All bindings are in bind group 0.
//! - A pipeline's **uniform block** holds every small `GpuData` argument of its entry points,
//!   packed in parameter order by WGSL's layout rules. It's a `uniform` binding when its layout
//!   satisfies WGSL's uniform rules, else a read-only `storage` binding (same bytes either way).
//! - Its **buffers** are the `GpuBuffer`s passed for `[T]` (read) and `Slots<T>` (read-write)
//!   parameters, in parameter order.
//! - A render pipeline draws a triangle list into the screen, whose format is [`SCREEN_FORMAT`]
//!   without sRGB encoding, so hosts produce the same bytes.

use serde::{Deserialize, Serialize};
use std::fmt;

pub const VERSION: u32 = 1;
/// The screen's texture format, in WebGPU's spelling.
pub const SCREEN_FORMAT: &str = "rgba8unorm";
/// WebGPU's default limits that the manifest is checked against.
pub const MAX_WORKGROUP_SIZE: [u32; 3] = [256, 256, 64];
pub const MAX_WORKGROUP_INVOCATIONS: u32 = 256;
pub const MAX_STORAGE_BUFFERS_PER_STAGE: usize = 8;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub manifest_version: u32,
    pub stream_version: u32,
    /// The program's WASM file, relative to the manifest.
    pub wasm: String,
    pub pipelines: Vec<Pipeline>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pipeline {
    /// For diagnostics: the entry points' names.
    pub name: String,
    /// The WGSL file, relative to the manifest.
    pub shader: String,
    #[serde(flatten)]
    pub stage: Stage,
    pub uniform: Option<UniformBlock>,
    pub buffers: Vec<BufferBinding>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Stage {
    Compute { entry: String, workgroup_size: [u32; 3] },
    Render { vertex_entry: String, fragment_entry: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UniformSpace {
    Uniform,
    Storage,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UniformBlock {
    pub binding: u32,
    /// Bytes; the uniform bytes of every dispatch or draw of this pipeline have this length.
    pub size: u32,
    pub space: UniformSpace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Read,
    ReadWrite,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BufferBinding {
    pub binding: u32,
    pub access: Access,
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
        Manifest { manifest_version: VERSION, stream_version: crate::stream::VERSION, wasm: wasm.into(), pipelines: Vec::new() }
    }

    /// Parses and validates a manifest; any other version is rejected.
    pub fn parse(json: &str) -> Result<Manifest, ManifestError> {
        let v: serde_json::Value = serde_json::from_str(json).map_err(|e| ManifestError(e.to_string()))?;
        let got = v.get("manifest_version").and_then(|x| x.as_u64());
        if got != Some(VERSION as u64) {
            return Err(ManifestError(format!("manifest version {got:?}, but this host reads version {VERSION}")));
        }
        let m: Manifest = serde_json::from_value(v).map_err(|e| ManifestError(e.to_string()))?;
        m.validate()?;
        Ok(m)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        let err = |s: String| Err(ManifestError(s));
        if self.manifest_version != VERSION {
            return err(format!("manifest version {}, expected {VERSION}", self.manifest_version));
        }
        if self.stream_version != crate::stream::VERSION {
            return err(format!("command stream version {}, but this host reads {}", self.stream_version, crate::stream::VERSION));
        }
        if self.wasm.is_empty() {
            return err("no WASM file".into());
        }
        for (i, p) in self.pipelines.iter().enumerate() {
            if p.shader.is_empty() {
                return err(format!("pipeline {i} has no shader"));
            }
            let mut bindings: Vec<u32> = p.buffers.iter().map(|b| b.binding).collect();
            if let Some(u) = &p.uniform {
                if u.size == 0 || u.size % 4 != 0 {
                    return err(format!("pipeline {i}'s uniform size {} isn't a positive multiple of 4", u.size));
                }
                if u.space == UniformSpace::Uniform && u.size % 16 != 0 {
                    return err(format!("pipeline {i}'s uniform block of {} bytes isn't a multiple of 16", u.size));
                }
                bindings.push(u.binding);
            }
            let n = bindings.len();
            bindings.sort_unstable();
            bindings.dedup();
            if bindings.len() != n {
                return err(format!("pipeline {i} uses a binding twice"));
            }
            let storage = p.buffers.len() + usize::from(p.uniform.as_ref().is_some_and(|u| u.space == UniformSpace::Storage));
            if storage > MAX_STORAGE_BUFFERS_PER_STAGE {
                return err(format!("pipeline {i} has {storage} storage buffers; WebGPU's default limit is {MAX_STORAGE_BUFFERS_PER_STAGE}"));
            }
            match &p.stage {
                Stage::Compute { entry, workgroup_size } => {
                    if entry.is_empty() {
                        return err(format!("pipeline {i} has no entry point"));
                    }
                    let total: u64 = workgroup_size.iter().map(|&x| x as u64).product();
                    let ok = workgroup_size.iter().zip(MAX_WORKGROUP_SIZE).all(|(&s, m)| s >= 1 && s <= m)
                        && total <= MAX_WORKGROUP_INVOCATIONS as u64;
                    if !ok {
                        return err(format!("pipeline {i}'s workgroup size {workgroup_size:?} is outside WebGPU's limits"));
                    }
                }
                Stage::Render { vertex_entry, fragment_entry } => {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        let mut m = Manifest::new("game.wasm");
        m.pipelines.push(Pipeline {
            name: "sample".into(),
            shader: "pipeline_0.wgsl".into(),
            stage: Stage::Compute { entry: "main".into(), workgroup_size: [64, 1, 1] },
            uniform: Some(UniformBlock { binding: 0, size: 32, space: UniformSpace::Uniform }),
            buffers: vec![BufferBinding { binding: 1, access: Access::ReadWrite }],
        });
        m.pipelines.push(Pipeline {
            name: "cover+shade".into(),
            shader: "pipeline_1.wgsl".into(),
            stage: Stage::Render { vertex_entry: "vs".into(), fragment_entry: "fs".into() },
            uniform: None,
            buffers: vec![],
        });
        m
    }

    /// Golden JSON: the manifest's format is part of the contract.
    #[test]
    fn golden_json() {
        let json = sample().to_json();
        let expected = r#"{
  "manifest_version": 1,
  "stream_version": 1,
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
      "buffers": [
        {
          "binding": 1,
          "access": "read_write"
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
      "buffers": []
    }
  ]
}
"#;
        assert_eq!(json, expected);
        assert_eq!(Manifest::parse(&json).expect("valid"), sample());
    }

    #[test]
    fn rejects_other_versions() {
        let json = sample().to_json().replace("\"manifest_version\": 1", "\"manifest_version\": 2");
        assert!(Manifest::parse(&json).is_err());
        let json = sample().to_json().replace("\"stream_version\": 1", "\"stream_version\": 9");
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
        m.pipelines[0].buffers[0].binding = 0;
        assert!(m.validate().is_err());
        let mut m = sample();
        m.pipelines[0].uniform = Some(UniformBlock { binding: 0, size: 20, space: UniformSpace::Uniform });
        assert!(m.validate().is_err());
    }
}
