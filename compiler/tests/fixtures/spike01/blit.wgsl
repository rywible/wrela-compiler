// Copies the 1920×1080 frame to the canvas, encoding to sRGB. Not measured.

@group(0) @binding(0) var frame: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct BOut {
  @builtin(position) clip: vec4f,
  @location(0) uv: vec2f,
}

@vertex
fn blit_vs(@builtin(vertex_index) vi: u32) -> BOut {
  let p = vec2f(f32((vi << 1u) & 2u), f32(vi & 2u));
  var o: BOut;
  o.clip = vec4f(p * 2.0 - 1.0, 0.0, 1.0);
  o.uv = vec2f(p.x, 1.0 - p.y);
  return o;
}

@fragment
fn blit_fs(i: BOut) -> @location(0) vec4f {
  return vec4f(pow(textureSample(frame, samp, i.uv).rgb, vec3f(1.0 / 2.2)), 1.0);
}
