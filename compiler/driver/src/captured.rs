//! A dispatch's or a draw's inputs, captured from the native host for the interpreter of GPU
//! code (`wrela_ir::interp`): its uniform bytes from the recorded command, each buffer's and
//! texture's contents as the GPU holds them now, and each sampler's settings from the command
//! that made it. Read from a host stopped just before the command (`wrela_host::Probe`), they're
//! what the command saw. What `wrela pixel` and the interpreter's tests run on.

use std::collections::HashMap;
use wrela_abi::manifest::{BindingKind, Pipeline};
use wrela_abi::stream::{Binding, Command, Compare};
use wrela_ir::interp::concrete::Given;
use wrela_ir::interp::{SamplerDesc, Texels};

/// Each sampler the program made, by its handle, from every batch recorded so far.
pub fn samplers(batches: &[Vec<u8>]) -> HashMap<u32, SamplerDesc> {
    let mut out = HashMap::new();
    for batch in batches {
        for cmd in wrela_abi::stream::decode(batch).unwrap_or_default() {
            if let Command::CreateSampler {
                handle,
                linear,
                repeat,
                compare,
                mip_linear,
                anisotropy,
            } = cmd
            {
                let compare = compare.map(|c| match c {
                    Compare::Less => wrela_ir::BinOp::Lt,
                    Compare::LessEqual => wrela_ir::BinOp::Le,
                    Compare::Greater => wrela_ir::BinOp::Gt,
                    Compare::GreaterEqual => wrela_ir::BinOp::Ge,
                    Compare::Equal => wrela_ir::BinOp::Eq,
                    Compare::NotEqual => wrela_ir::BinOp::Ne,
                    // Always and never: as `And` and `Or` stand-ins the interpreter reads as
                    // passing.
                    Compare::Always | Compare::Never => wrela_ir::BinOp::And,
                });
                out.insert(
                    handle,
                    SamplerDesc {
                        linear,
                        mip_linear,
                        repeat: [repeat; 3],
                        anisotropy: anisotropy.max(1) as u16,
                        compare,
                    },
                );
            }
        }
    }
    out
}

/// A texture's every level and layer, as the host holds it.
pub fn texels(host: &mut wrela_host::Host, handle: u32) -> Result<Texels, String> {
    let mut levels = Vec::new();
    let (mut size, mut format) = ([0; 3], None);
    for level in 0.. {
        let mut layers = Vec::new();
        for layer in 0.. {
            match host.read_texture(handle, level, layer) {
                Ok((s, f, bytes)) => {
                    if level == 0 && layer == 0 {
                        size = s;
                        format = Some(f);
                    }
                    layers.push(bytes);
                    // A 3D texture's level is one read of every slice.
                    if s[2] > 1 {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        if layers.is_empty() {
            break;
        }
        levels.push(layers);
    }
    let format = format.ok_or_else(|| format!("texture {handle} can't be read back"))?;
    let depth = if size[2] > 1 { size[2] } else { levels[0].len() as u32 };
    Ok(Texels { width: size[0], height: size[1], depth, format, levels })
}

/// The inputs of a command of `pipeline` with `bindings` and `uniforms`, read from `host`.
pub fn given(
    pipeline: &Pipeline,
    bindings: &[Binding],
    uniforms: &[u8],
    samplers: &HashMap<u32, SamplerDesc>,
    host: &mut wrela_host::Host,
) -> Result<Given, String> {
    let mut g = Given::default();
    if let Some(u) = &pipeline.uniform {
        g.bytes.insert(u.binding, uniforms.to_vec());
    }
    for (rb, b) in pipeline.bindings.iter().zip(bindings) {
        match rb.kind {
            BindingKind::Read | BindingKind::ReadWrite => {
                let bytes = host.read_buffer(b.handle).map_err(|e| e.to_string())?;
                let start = (b.offset as usize).min(bytes.len());
                let end = if b.size == 0 {
                    bytes.len()
                } else {
                    (start + b.size as usize).min(bytes.len())
                };
                g.bytes.insert(rb.binding, bytes[start..end].to_vec());
            }
            BindingKind::Sampler
            | BindingKind::ComparisonSampler
            | BindingKind::NonFilteringSampler => {
                if let Some(s) = samplers.get(&b.handle) {
                    g.samplers.insert(rb.binding, *s);
                }
            }
            BindingKind::Texture
            | BindingKind::DepthTexture
            | BindingKind::Texture3d
            | BindingKind::TextureArray
            | BindingKind::TextureCube
            | BindingKind::DepthTextureArray => {
                g.textures.insert(rb.binding, texels(host, b.handle)?);
            }
            // Written, not read.
            BindingKind::StorageTexture
            | BindingKind::StorageTexture3d
            | BindingKind::StorageTextureArray => {}
        }
    }
    Ok(g)
}
