//! Comparisons for authoring by eye: `wrela studio <package> beside <photo manifest>` (the side
//! view beside a registered reference photo, at the photo's own scale, and the two overlaid),
//! `wrela studio <package> variants <package>...` (several versions of a subject side by side,
//! at one framing) and `wrela studio <package> sweep <literal> <value>...` (one literal's values
//! side by side). Each writes one PNG and prints a JSON answer naming what's in each panel.
//!
//! A photo's registration is in its manifest (`references/`): `ground_px` (the ground's row),
//! `metres_per_px`, `z0_px` (the column at z = 0) and `faces` (`right` or `left`, the way the
//! animal faces in the photo). The photo itself is read as PNG: `<manifest stem>.png`.

use std::path::{Path, PathBuf};
use wrela_host::Value;

use crate::reference::Value as Toml;
use crate::studio::{Lens, build, round3, save_rgba};

/// A photo, registered: how its pixels map to the creature's side view (z, y), in metres.
struct Registered {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    ground_px: f32,
    metres_per_px: f32,
    z0_px: f32,
    faces_right: bool,
}

fn registered(manifest: &Path) -> Result<Registered, String> {
    let text = std::fs::read_to_string(manifest)
        .map_err(|e| format!("can't read {}: {e}", manifest.display()))?;
    let tables =
        crate::reference::parse_toml(&text).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let value = |key: &str| tables.iter().find_map(|t| t.get(key));
    let number = |key: &str| -> Result<f32, String> {
        match value(key) {
            Some(Toml::Num(x)) => Ok(*x as f32),
            _ => Err(format!(
                "{} has no `{key}` (a photo's registration: ground_px, metres_per_px, z0_px, faces)",
                manifest.display()
            )),
        }
    };
    let png = manifest.with_extension("png");
    let (width, height, rgba) = wrela_host::image::read_png(&png).map_err(|e| {
        format!(
            "can't read the photo {} ({e}): a reference is read as PNG beside its manifest (`sips -s format png photo.jpg --out {}`)",
            png.display(),
            png.display()
        )
    })?;
    Ok(Registered {
        width,
        height,
        rgba,
        ground_px: number("ground_px")?,
        metres_per_px: number("metres_per_px")?,
        z0_px: number("z0_px")?,
        faces_right: !matches!(value("faces"), Some(Toml::Str(f)) if f == "left"),
    })
}

impl Registered {
    /// The photo's colour at side-view point (z, y), bilinear; white off the photo.
    fn at(&self, z: f32, y: f32) -> [f32; 3] {
        let dz = z / self.metres_per_px;
        let x = if self.faces_right { self.z0_px + dz } else { self.z0_px - dz };
        let yp = self.ground_px - y / self.metres_per_px;
        let (x0, y0) = (x.floor(), yp.floor());
        let (fx, fy) = (x - x0, yp - y0);
        let px = |xi: f32, yi: f32| -> [f32; 3] {
            if xi < 0.0 || yi < 0.0 || xi >= self.width as f32 || yi >= self.height as f32 {
                return [255.0; 3];
            }
            let i = 4 * (yi as usize * self.width as usize + xi as usize);
            [f32::from(self.rgba[i]), f32::from(self.rgba[i + 1]), f32::from(self.rgba[i + 2])]
        };
        let (a, b, c, d) = (px(x0, y0), px(x0 + 1.0, y0), px(x0, y0 + 1.0), px(x0 + 1.0, y0 + 1.0));
        let mut out = [0.0; 3];
        for k in 0..3 {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bottom = c[k] + (d[k] - c[k]) * fx;
            out[k] = top + (bottom - top) * fy;
        }
        out
    }
}

/// A square framing of the side view: centred at (z, y), `half` metres to each edge.
#[derive(Clone, Copy)]
struct Framing {
    z: f32,
    y: f32,
    half: f32,
}

impl Framing {
    /// The side-view point under pixel (`sx`, `sy`) of an `n`-pixel square: the side view looks
    /// along −x, so the screen's right is −z (the front is on the left).
    fn point(self, sx: f32, sy: f32, n: f32) -> (f32, f32) {
        (self.z - (sx / n - 0.5) * 2.0 * self.half, self.y + (0.5 - sy / n) * 2.0 * self.half)
    }
}

/// Runs `calls` (an export and its arguments, each) in the lens and returns the screen.
fn screen(lens: &mut Lens, calls: &[(&str, Vec<Value>)]) -> Result<Vec<u8>, String> {
    for (name, args) in calls {
        lens.call(name, args)?;
    }
    lens.screen()
}

fn zoom_call(f: Framing) -> (&'static str, Vec<Value>) {
    ("zoom", vec![Value::F32(0.0), Value::F32(f.y), Value::F32(f.z), Value::F32(f.half)])
}

fn view_call(facing: u32) -> (&'static str, Vec<Value>) {
    ("view", vec![Value::I32(facing as i32)])
}

fn mode_call(mode: u32) -> (&'static str, Vec<Value>) {
    ("mode", vec![Value::I32(mode as i32), Value::I32(0)])
}

/// The side framing the lens gives the subject (its `view` answer's first framing).
fn own_framing(lens: &mut Lens) -> Result<Framing, String> {
    let a = lens.answer("view", &[Value::I32(0)])?.map_err(|e| e.to_string())?;
    let side = &a["framings"][0];
    let c = |k: usize| side["centre"][k].as_f64().unwrap_or(0.0) as f32;
    Ok(Framing { z: c(2), y: c(1), half: side["half"].as_f64().unwrap_or(1.0) as f32 })
}

/// Panels in a grid, each `n` pixels square shrunk to half, with a number in the top-left
/// corner of each (three or fewer in a row, more in two rows), saved as a PNG at `png`.
fn sheet(panels: &[Vec<u8>], n: u32, png: &Path) -> Result<(), String> {
    const SHRINK: u32 = 2;
    let columns = if panels.len() <= 3 { panels.len() } else { panels.len().div_ceil(2) }.max(1);
    let cell = n / SHRINK;
    let rows = panels.len().div_ceil(columns);
    let (w, h) = (cell * columns as u32, cell * rows as u32);
    let mut out = vec![255u8; (w * h * 4) as usize];
    for (k, p) in panels.iter().enumerate() {
        let (cx, cy) = ((k % columns) as u32 * cell, (k / columns) as u32 * cell);
        for y in 0..cell {
            for x in 0..cell {
                // A box filter over the shrunk pixels.
                let mut acc = [0u32; 3];
                for dy in 0..SHRINK {
                    for dx in 0..SHRINK {
                        let i = (((y * SHRINK + dy) * n + x * SHRINK + dx) * 4) as usize;
                        for c in 0..3 {
                            acc[c] += u32::from(p[i + c]);
                        }
                    }
                }
                let o = (((cy + y) * w + cx + x) * 4) as usize;
                for c in 0..3 {
                    out[o + c] = (acc[c] / (SHRINK * SHRINK)) as u8;
                }
                out[o + 3] = 255;
            }
        }
        // A thin border, and the panel's number.
        let border = [150, 150, 160];
        fill(&mut out, w, cx, cy, cell, 1, border);
        fill(&mut out, w, cx, cy + cell - 1, cell, 1, border);
        fill(&mut out, w, cx, cy, 1, cell, border);
        fill(&mut out, w, cx + cell - 1, cy, 1, cell, border);
        number(&mut out, w, cx + 6, cy + 6, k + 1);
    }
    save_rgba(png, w, h, &out)
}

/// The pixels from (`x`, `y`), `dw` across and `dh` down, of an image `w` pixels wide, set to
/// `rgb` (those in the image).
fn fill(out: &mut [u8], w: u32, x: u32, y: u32, dw: u32, dh: u32, rgb: [u8; 3]) {
    for py in y..y + dh {
        for px in x..x + dw {
            let o = ((py * w + px) * 4) as usize;
            if o + 3 < out.len() {
                out[o..o + 3].copy_from_slice(&rgb);
            }
        }
    }
}

/// `n` in a 5×7 bitmap font, 3 pixels a dot, dark on a light box, at (`x`, `y`).
fn number(out: &mut [u8], w: u32, x: u32, y: u32, n: usize) {
    const DIGITS: [[u8; 7]; 10] = [
        [14, 17, 19, 21, 25, 17, 14],
        [4, 12, 4, 4, 4, 4, 14],
        [14, 17, 1, 2, 4, 8, 31],
        [31, 2, 4, 2, 1, 17, 14],
        [2, 6, 10, 18, 31, 2, 2],
        [31, 16, 30, 1, 1, 17, 14],
        [6, 8, 16, 30, 17, 17, 14],
        [31, 1, 2, 4, 8, 8, 8],
        [14, 17, 17, 14, 17, 17, 14],
        [14, 17, 17, 15, 1, 2, 12],
    ];
    let text = n.to_string();
    let scale = 3;
    let (bw, bh) = (text.len() as u32 * 6 * scale + 2 * scale, 9 * scale);
    fill(out, w, x, y, bw, bh, [250, 250, 235]);
    for (k, ch) in text.bytes().enumerate() {
        let glyph = DIGITS[(ch - b'0') as usize];
        for (r, bits) in glyph.iter().enumerate() {
            for c in (0..5).filter(|c| bits & (16 >> c) != 0) {
                let (px, py) =
                    (x + scale + (k as u32 * 6 + c) * scale, y + scale + r as u32 * scale);
                fill(out, w, px, py, scale, scale, [20, 20, 30]);
            }
        }
    }
}

/// `beside`: the photo (turned to face as the side view does), the subject's side view at the
/// photo's scale, and the photo with the subject's silhouette laid over it (inside tinted, its
/// edge red), in a row, each panel `n` pixels square before it's shrunk.
pub fn beside(pkg: &Path, page: &Path, n: u32, manifest: &Path, png: &Path) -> Result<(), String> {
    let photo = registered(manifest)?;
    let mut lens = Lens::start(pkg, page, (n, n), None)?;
    // Frame the subject and the photo's animal both: the subject's own framing's size, or
    // most of the photo's shorter side, whichever is larger.
    let own = own_framing(&mut lens)?;
    let photo_m = photo.width.min(photo.height) as f32 * photo.metres_per_px;
    let half = own.half.max(0.45 * photo_m);
    let f = Framing { z: 0.0, y: half * 0.88, half };
    let shaded = screen(&mut lens, &[zoom_call(f), mode_call(0), view_call(0)])?;
    let silhouette = screen(&mut lens, &[mode_call(4)])?;
    let mut left = vec![0u8; (n * n * 4) as usize];
    let mut over = vec![0u8; (n * n * 4) as usize];
    let inside = |i: usize| silhouette[i] < 100;
    let mut covered = 0usize;
    let mut matched = 0usize;
    for sy in 0..n {
        for sx in 0..n {
            let (z, y) = f.point(sx as f32 + 0.5, sy as f32 + 0.5, n as f32);
            let c = photo.at(z, y);
            let i = ((sy * n + sx) * 4) as usize;
            for k in 0..3 {
                left[i + k] = c[k] as u8;
            }
            left[i + 3] = 255;
            let edge = inside(i)
                && [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)].iter().any(|(dx, dy)| {
                    let (x2, y2) = (sx as i32 + dx, sy as i32 + dy);
                    x2 < 0
                        || y2 < 0
                        || x2 >= n as i32
                        || y2 >= n as i32
                        || !inside(((y2 as u32 * n + x2 as u32) * 4) as usize)
                });
            let tinted = if edge {
                [230.0, 30.0, 30.0]
            } else if inside(i) {
                [c[0] * 0.6 + 60.0, c[1] * 0.6 + 100.0, c[2] * 0.6 + 40.0]
            } else {
                c
            };
            for k in 0..3 {
                over[i + k] = tinted[k].clamp(0.0, 255.0) as u8;
            }
            over[i + 3] = 255;
            if inside(i) {
                covered += 1;
                if (c[0] + c[1] + c[2]) / 3.0 < 175.0 {
                    matched += 1;
                }
            }
        }
    }
    sheet(&[left, shaded, over], n, png)?;
    let answer = serde_json::json!({
        "action": "beside",
        "png": png.display().to_string(),
        "panels": ["1: the photo, facing as the side view does", "2: your side view at the photo's scale", "3: the photo with your silhouette over it (tinted inside, red edge)"],
        "framing": { "z": f.z, "y": f.y, "half": f.half },
        "metres_per_px_in_panels": 2.0 * f.half / (n as f32 / 2.0),
        "silhouette_on_dark_photo_pixels": if covered > 0 { round3(matched as f64 / covered as f64) } else { 0.0 },
    });
    println!("{answer}");
    Ok(())
}

/// `variants`: the side view (or `facing`) of `pkg` (its lens built at `page`) and of each of
/// `others`, at `pkg`'s framing, in a grid numbered in order.
pub fn variants(
    pkg: &Path,
    page: &Path,
    others: &[PathBuf],
    n: u32,
    facing: u32,
    png: &Path,
) -> Result<(), String> {
    let mut panels = Vec::new();
    let mut legend = Vec::new();
    let mut framing: Option<Framing> = None;
    let dirs = std::iter::once(pkg).chain(others.iter().map(PathBuf::as_path));
    for (k, dir) in dirs.enumerate() {
        // The first package's lens is built already.
        let built = if k == 0 {
            page.to_path_buf()
        } else {
            build(dir, false).map_err(|_| format!("{} doesn't build", dir.display()))?
        };
        let mut lens = Lens::start(dir, &built, (n, n), None)?;
        let f = match framing {
            Some(f) => f,
            None => {
                let f = own_framing(&mut lens)?;
                framing = Some(f);
                f
            }
        };
        let calls =
            if facing == 0 { vec![zoom_call(f), view_call(0)] } else { vec![view_call(facing)] };
        panels.push(screen(&mut lens, &calls)?);
        legend.push(serde_json::json!({ "panel": k + 1, "package": dir.display().to_string() }));
    }
    sheet(&panels, n, png)?;
    let answer = serde_json::json!({ "action": "variants", "png": png.display().to_string(), "panels": legend });
    println!("{answer}");
    Ok(())
}

/// `sweep`: literal `literal` set to each value in turn, in one lens session, its side view (or
/// `facing`) at the subject's framing, numbered in order.
pub fn sweep(
    pkg: &Path,
    page: &Path,
    n: u32,
    literal: u32,
    values: &[f32],
    facing: u32,
    png: &Path,
) -> Result<(), String> {
    let mut lens = Lens::start(pkg, page, (n, n), None)?;
    let f = own_framing(&mut lens)?;
    let mut panels = Vec::new();
    let mut legend = Vec::new();
    for (k, v) in values.iter().enumerate() {
        let mut calls = vec![("set", vec![Value::I32(literal as i32), Value::F32(*v)])];
        if facing == 0 {
            calls.push(zoom_call(f));
        }
        calls.push(view_call(facing));
        panels.push(screen(&mut lens, &calls)?);
        legend.push(serde_json::json!({ "panel": k + 1, "value": v }));
    }
    sheet(&panels, n, png)?;
    let answer = serde_json::json!({ "action": "sweep", "literal": literal, "png": png.display().to_string(), "panels": legend });
    println!("{answer}");
    Ok(())
}
