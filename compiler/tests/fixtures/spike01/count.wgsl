// Measurement only: counts covered texels in the coverage target.

@group(0) @binding(0) var cov: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> total: atomic<u32>;

var<workgroup> local_count: atomic<u32>;

@compute @workgroup_size(16, 16)
fn count(@builtin(global_invocation_id) id: vec3u, @builtin(local_invocation_index) li: u32) {
  let d = textureDimensions(cov);
  if (all(id.xy < d)) {
    if (textureLoad(cov, id.xy, 0).r > 0.5) { atomicAdd(&local_count, 1u); }
  }
  workgroupBarrier();
  if (li == 0u) { atomicAdd(&total, atomicLoad(&local_count)); }
}
