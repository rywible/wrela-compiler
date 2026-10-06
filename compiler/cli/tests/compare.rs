//! `wrela studio <package> beside | variants | sweep`: a ball beside a photo of itself (a dark
//! disc drawn where the ball is, registered), two balls side by side, and one literal swept.
//! They need a GPU.

mod common;

use std::path::{Path, PathBuf};

const BALL: &str = "use std::field::{Lipschitz, sphere}

pub fn subject() -> Lipschitz {
    sphere(0.30).translate(vec3(0.0, 0.30, 0.0))
}
";

fn package(name: &str, subject: &str) -> PathBuf {
    let manifest = common::manifest(&name.replace('-', "_"));
    common::package(
        name,
        &[("wrela.toml", &manifest), ("main.wrela", common::FRAME), ("subject.wrela", subject)],
    )
}

/// The lens's answer: the last JSON line `wrela studio` printed.
fn studio(pkg: &Path, args: &[&str]) -> serde_json::Value {
    let text = common::stdout(common::wrela().arg("studio").arg(pkg).args(args));
    let line = text.lines().rev().find(|l| l.starts_with('{')).expect("an answer");
    serde_json::from_str(line).expect("JSON")
}

#[test]
#[ignore = "needs a GPU"]
fn a_ball_beside_its_own_photo_covers_it() {
    let pkg = package("compare-ball", BALL);
    // The photo: 400×300 pixels, 2 mm a pixel, the ground on row 280, z = 0 on column 200; a
    // dark disc where the ball is (0.30 m across its radius, its centre 0.30 m up).
    let (w, h) = (400u32, 300u32);
    let mut rgba = vec![240u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let (z, up) = ((x as f32 + 0.5 - 200.0) * 0.002, (280.0 - y as f32 - 0.5) * 0.002);
            if z * z + (up - 0.30) * (up - 0.30) < 0.30 * 0.30 {
                let i = ((y * w + x) * 4) as usize;
                rgba[i..i + 3].copy_from_slice(&[40, 40, 40]);
            }
        }
    }
    let photo = pkg.join("photo.png");
    wrela_host::image::write_png(&photo, w, h, &rgba).unwrap();
    std::fs::write(
        pkg.join("photo.toml"),
        "ground_px = 280\nmetres_per_px = 0.002\nz0_px = 200\nfaces = \"right\"\n",
    )
    .unwrap();
    let png = pkg.join("beside.png");
    let a = studio(
        &pkg,
        &["beside", pkg.join("photo.toml").to_str().unwrap(), "--png", png.to_str().unwrap()],
    );
    let share = a["silhouette_on_dark_photo_pixels"].as_f64().unwrap();
    // Pixels on the disc's edge blend it with the background.
    assert!(share > 0.95, "{a}");
    let (pw, ph, _) = wrela_host::image::read_png(&png).unwrap();
    assert_eq!((pw, ph), (3 * 448, 448), "three panels at half size");
    // Moved 10 cm, the ball covers less of its photo.
    let moved = package(
        "compare-ball-moved",
        &BALL.replace("vec3(0.0, 0.30, 0.0)", "vec3(0.0, 0.30, 0.10)"),
    );
    let b = studio(
        &moved,
        &["beside", pkg.join("photo.toml").to_str().unwrap(), "--png", png.to_str().unwrap()],
    );
    assert!(b["silhouette_on_dark_photo_pixels"].as_f64().unwrap() < 0.9, "{b}");
}

#[test]
#[ignore = "needs a GPU"]
fn variants_and_sweeps_are_numbered_panels() {
    let a = package("compare-variant-a", BALL);
    let b = package("compare-variant-b", &BALL.replace("sphere(0.30)", "sphere(0.20)"));
    let png = a.join("variants.png");
    let v = studio(&a, &["variants", b.to_str().unwrap(), "--png", png.to_str().unwrap()]);
    assert_eq!(v["panels"].as_array().unwrap().len(), 2, "{v}");
    assert_eq!(wrela_host::image::read_png(&png).unwrap().0, 2 * 448);
    let literals = studio(&a, &["literals"]);
    let radius = literals["literals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["text"] == "0.30" && l["column"] == 12)
        .expect("the radius")["index"]
        .as_u64()
        .unwrap();
    let swept = a.join("sweep.png");
    let s = studio(
        &a,
        &["sweep", &radius.to_string(), "0.1", "0.2", "0.3", "--png", swept.to_str().unwrap()],
    );
    let values: Vec<f32> = s["panels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["value"].as_f64().unwrap() as f32)
        .collect();
    assert_eq!(values, [0.1, 0.2, 0.3]);
}
