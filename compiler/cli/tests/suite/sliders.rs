//! `wrela sliders` (language.md §23.10): two balls whose radii are separate literals have two
//! sliders, one a ball; a third literal that scales both is a direction of neither, so it adds
//! almost nothing; `--move 1=1 --write` changes only the literals the slider names; and the sheet
//! is a picture a slider row, three across.

use crate::common;

const PAIR: &str = "use std::field::{Surface, sphere}

pub fn pair() -> Surface {
    let ra = 0.30
    let rb = 0.26
    let k = 1.0
    let a = sphere(ra * k).translate(vec3(-0.5, 0.0, 0.0)).named(\"left ball\")
    let b = sphere(rb * k).translate(vec3(0.5, 0.0, 0.0)).named(\"right ball\")
    a.union(b)
}
";

fn pair(name: &str) -> std::path::PathBuf {
    common::package(
        name,
        &[
            ("wrela.toml", &common::manifest("pair")),
            ("pair.wrela", PAIR),
            ("main.wrela", common::FRAME),
        ],
    )
}

fn sliders(dir: &std::path::Path, free: &str, extra: &[&str]) -> serde_json::Value {
    common::answer(
        common::wrela()
            .arg("sliders")
            .arg(dir)
            .args([
                "pair::pair",
                "--free",
                free,
                "--region",
                " -0.9..0.9,-0.5..0.5,-0.5..0.5",
                "--depth",
                "5",
            ])
            .args(extra)
            .arg("--json"),
    )
}

#[test]
fn two_radii_are_two_sliders_and_a_scale_of_both_adds_almost_nothing() {
    let dir = pair("sliders-pair");
    let r = sliders(&dir, "pair.wrela:ra,rb", &[]);
    let s = r["sliders"].as_array().unwrap();
    let names: Vec<&str> = s.iter().take(2).map(|x| x["name"].as_str().unwrap()).collect();
    assert!(names.iter().any(|n| n.starts_with("left ball")), "{names:?}");
    assert!(names.iter().any(|n| n.starts_with("right ball")), "{names:?}");
    let (s1, s2) =
        (s[0]["singular_value"].as_f64().unwrap(), s[1]["singular_value"].as_f64().unwrap());
    assert!(s2 > 0.7 * s1, "nearly equal: {s1} and {s2}");
    let r = sliders(&dir, "pair.wrela:ra,rb,k", &[]);
    let s = r["sliders"].as_array().unwrap();
    let (s1, s3) =
        (s[0]["singular_value"].as_f64().unwrap(), s[2]["singular_value"].as_f64().unwrap());
    assert!(s3 < 0.01 * s1, "the scale adds {s3} against {s1}: {r:#}");
    assert_eq!(r["redundant"], serde_json::json!([]), "{r:#}");
}

#[test]
fn moving_along_a_slider_writes_only_its_literals() {
    let dir = pair("sliders-move");
    let r = sliders(&dir, "pair.wrela:ra,rb", &["--move", "1=1", "--write"]);
    assert_eq!(r["written"], true, "{r:#}");
    let listed: Vec<u64> = r["sliders"][0]["literals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["literal"].as_u64().unwrap())
        .collect();
    for m in r["moves"].as_array().unwrap() {
        assert!(listed.contains(&m["literal"].as_u64().unwrap()), "{r:#}");
    }
    let text = std::fs::read_to_string(dir.join("pair.wrela")).unwrap();
    assert!(text.contains("let k = 1.0"), "an unfreed literal is untouched:\n{text}");
    assert_ne!(text, PAIR, "the move was written");
}

#[test]
fn the_sheet_draws_each_slider_three_ways() {
    let dir = pair("sliders-sheet");
    let sheet = dir.join("sheet.png");
    sliders(&dir, "pair.wrela:ra,rb", &["--top", "2", "--sheet", sheet.to_str().unwrap()]);
    let bytes = std::fs::read(&sheet).unwrap();
    let (w, h, buf) = wrela_host::image::png_of(&bytes, &sheet).expect("a PNG");
    assert_eq!((w, h), (288, 192));
    let picture = |row: usize, k: usize| -> Vec<u8> {
        (0..96)
            .flat_map(|y| {
                let at = ((row * 96 + y) * 288 + k * 96) * 4;
                buf[at..at + 96 * 4].to_vec()
            })
            .collect()
    };
    for row in 0..2 {
        assert_ne!(picture(row, 0), picture(row, 1), "slider {row}: -1 is the middle");
        assert_ne!(picture(row, 2), picture(row, 1), "slider {row}: +1 is the middle");
    }
}
