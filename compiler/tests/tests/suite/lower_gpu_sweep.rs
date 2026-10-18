//! GPU interface regressions from the compiler audit.

#[test]
fn generic_position_fields_emit_valid_shader_interfaces() {
    let dir = super::package(
        "lower-gpu-sweep/generic-position",
        r#"
use std::gpu::{ClipPosition, FragCoord, begin_screen_pass, draw}

struct Output<P> {
    position: P,
    colour: vec3,
}

@vertex
fn vertex() -> Output<ClipPosition> {
    Output { position: ClipPosition { position: vec4(0.0, 0.0, 0.5, 1.0) }, colour: vec3(1.0) }
}

@fragment
fn fragment(input: Output<ClipPosition>) -> vec4 {
    vec4(input.colour, 1.0)
}

@fragment
fn positioned(input: Output<ClipPosition>, pixel: FragCoord) -> vec4 {
    vec4(input.colour * input.position.position.w + vec3(pixel.position.x * 0.001), 1.0)
}

pub fn frame(time: f32, width: u32, height: u32) {
    let pass = begin_screen_pass(clear: vec4(0.0))
    draw(pass, vertex, fragment, vertices: 3)
    draw(pass, vertex, positioned, vertices: 3)
    pass.present()
}
"#,
    );
    let built = wrela_driver::build(&dir);
    assert!(!built.has_errors(), "{:?}", built.diagnostics);
    assert_eq!(built.files.iter().filter(|(path, _)| path.ends_with(".wgsl")).count(), 2);
}
