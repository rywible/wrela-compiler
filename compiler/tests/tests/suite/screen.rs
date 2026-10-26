//! The screen's colour (language.md §18.8): a program asks for an HDR screen in Display P3
//! (`std::gpu::screen`), and once every pipeline that draws the screen has a variant for it the
//! browser shows the canvas in `rgba16float`, tone mapped `extended`, in `display-p3`; a white of
//! 2.0 stays 2.0 in the screen's texels, and `screen_is` says so. Test mode does this only when
//! the test asks (`hdr=1`); otherwise its screen, so its goldens, stay as they were. The native
//! host, drawing offscreen, keeps SDR in sRGB.

use wrela_host::{Host, Value};

/// A program that asks for an HDR screen in Display P3 at its first frame, draws it white at
/// 2.0, and tells what `screen_is` says.
const HDR: &str =
    "use std::gpu::{ClipPosition, Gamut, VertexIndex, begin_screen_pass, draw, screen, screen_is}

pub struct S {
    frames: u32,
}

pub fn init() -> S {
    S { frames: 0 }
}

@vertex
fn cover(v: VertexIndex) -> ClipPosition {
    let x = f32((v.index << 1) & 2) * 2.0 - 1.0
    let y = f32(v.index & 2) * 2.0 - 1.0
    ClipPosition { position: vec4(x, y, 0.5, 1.0) }
}

@fragment
fn bright() -> vec4 {
    vec4(2.0, 2.0, 2.0, 1.0)
}

pub fn frame(s: mut S, time: f32, width: u32, height: u32) {
    if s.frames == 0 {
        screen(hdr: true, gamut: Gamut::DisplayP3)
    }
    s.frames += 1
    let p = begin_screen_pass(clear: vec4(0.0))
    draw(p, cover, bright, vertices: 3)
    p.present()
}

/// Bit 0: the screen is HDR; bit 1: Display P3.
pub fn shown(s: S) -> u32 {
    let is = screen_is()
    u32(is.hdr) + 2 * u32(is.gamut == Gamut::DisplayP3)
}
";

#[test]
#[ignore = "needs a GPU and Chrome"]
fn an_hdr_screen_shows_its_white_past_one() {
    let pkg = crate::package("screen-hdr", HDR);
    let built = wrela_tests::build(&pkg).unwrap_or_else(|e| panic!("doesn't build:\n{e}"));
    let dir = crate::scratch("screen-hdr-page");
    built.write_to(&dir).expect("the build");
    wrela_host::pages::run(wrela_host::pages::Page {
        root: &dir,
        page: "",
        fragment: "#test&frames=30&width=64&height=64&hdr=1",
        timeout: std::time::Duration::from_secs(120),
        script: None,
    })
    .expect("the page runs");
    let text = std::fs::read_to_string(dir.join("results/screen.json")).expect("screen.json");
    let v: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(v["configuration"]["format"], "rgba16float", "{v}");
    assert_eq!(v["configuration"]["colorSpace"], "display-p3", "{v}");
    assert_eq!(v["configuration"]["toneMapping"], "extended", "{v}");
    let middle: Vec<f64> =
        v["middle"].as_array().unwrap().iter().filter_map(|x| x.as_f64()).collect();
    assert_eq!(middle, [2.0, 2.0, 2.0, 1.0], "{v}");
    assert_eq!(v["state"], 3, "screen_is: HDR, Display P3");
}

/// Without `hdr=1`, test mode keeps its screen: the command changes nothing, the white clamps,
/// and `screen_is` says SDR in sRGB; the native host does the same.
#[test]
#[ignore = "needs a GPU and Chrome"]
fn a_screen_stays_sdr_where_it_cant_change() {
    let pkg = crate::package("screen-sdr", HDR);
    let built = wrela_tests::build(&pkg).unwrap_or_else(|e| panic!("doesn't build:\n{e}"));
    let dir = crate::scratch("screen-sdr-page");
    built.write_to(&dir).expect("the build");
    wrela_host::pages::run(wrela_host::pages::Page {
        root: &dir,
        page: "",
        fragment: "#test&frames=10&width=64&height=64",
        timeout: std::time::Duration::from_secs(120),
        script: None,
    })
    .expect("the page runs");
    assert!(!dir.join("results/screen.json").exists(), "no screen.json without hdr=1");
    let mut host = Host::load(&dir).expect("load");
    host.init().expect("init");
    for k in 0..3 {
        host.lockstep_frame(k, 60.0, 64, 64, &[]).expect("a frame");
    }
    let shown = host.call_export("shown", &[]).expect("shown");
    assert_eq!(shown, [Value::I32(0)], "the native host keeps SDR in sRGB");
}
