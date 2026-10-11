//! The Last Green's floor (M6: #54, #55): its map's anatomy, area and keepers, what the
//! interest check says of its bake, the preview's check beside the bake's, its build from the
//! constants' cache, and the floor's own tests (head room, ground cover).
//!
//! The map alone (map.wrela and what it reads) is tested in a scratch package of its own, so
//! those tests don't bake: they're the gate's. The bake's tests build the floor, which bakes
//! when the constants' cache is cold (minutes): they're long.

use crate::scratch;
use serde_json::Value;
use std::path::{Path, PathBuf};
use wrela_tests::repo_root;

/// The floor's files that make its map: copied into a package with `main` as its program.
const MAP_FILES: [&str; 4] = ["map.wrela", "ground.wrela", "species.wrela", "stones.wrela"];

/// A scratch package `name` holding the floor's map files and `main` as its `main.wrela`,
/// depending on the engine.
fn map_package(name: &str, main: &str) -> PathBuf {
    let dir = scratch(name);
    let floor = repo_root().join("examples/last-green");
    for f in MAP_FILES {
        std::fs::copy(floor.join(f), dir.join(f)).expect("copy the floor's map");
    }
    std::fs::write(dir.join("main.wrela"), main).expect("write main.wrela");
    let engine = repo_root().join("engine").display().to_string();
    let manifest = format!(
        "[package]\nname = \"floor_map\"\n\n[dependencies]\nengine = {{ path = {engine:?} }}\n"
    );
    std::fs::write(dir.join("wrela.toml"), manifest).expect("write wrela.toml");
    dir
}

/// AC1: the floor's map is wrela source written with the engine's types, and holds #44's
/// anatomy: a gate, a town's site, 3–5 anchors, a Warden's arena, rest points, waystones, 25–35
/// echoes' sites and views; its area is 3 km² within 10% (Q1); and the engine's own refusals
/// (`place::refusals`) find nothing to refuse in it.
#[test]
fn the_floors_map_has_its_anatomy_and_area() {
    let tests = r#"
use engine::interest::floor_area
use engine::place::{SiteKind, refusals}

@test
fn anatomy() {
    let m = map::map()
    assert(m.count(SiteKind::Gate) == 1, "one gate")
    assert(m.count(SiteKind::Town) == 1, "one town's site")
    let anchors = m.count(SiteKind::Anchor)
    assert(anchors >= 3 && anchors <= 5, f"3 to 5 anchors: {anchors}")
    assert(m.count(SiteKind::Arena) == 1, "one Warden's arena")
    assert(m.count(SiteKind::Rest) >= 1, "rest points")
    assert(m.count(SiteKind::Waystone) >= 1, "waystones")
    let echoes = m.count(SiteKind::Echo)
    assert(echoes >= 25 && echoes <= 35, f"25 to 35 echoes' sites: {echoes}")
    assert(m.count(SiteKind::View) >= 1, "views")
}

@test
fn area() {
    let a = floor_area(ground::ground(), map::map())
    assert(a > 2.7 && a < 3.3, f"about 3 km², within 10%: {a}")
}

@test
fn nothing_to_refuse() {
    assert(refusals(map::map()).len() == 0, "the floor's map has an opening or a route with no keeper at work")
}

pub fn frame(time: f32, width: u32, height: u32) {}
"#;
    let dir = map_package("last-green/anatomy", tests);
    assert_eq!(super::tests_pass(&dir), 3);
}

/// A package that grows the floor's map changed by `edit` (its `map()` bound to `m`, mutable),
/// at the preview's grain, in a constant.
fn grown_from(name: &str, edit: &str) -> PathBuf {
    let main = format!(
        r#"
use engine::grow::{{History, PREVIEW, simulate}}

const GROWN: u32 = grown()

fn grown() -> u32 {{
    var m = map::map()
    {edit}
    let h = simulate(ground::ground(), m, species::habits(), PREVIEW, map::LO, map::HI, stones::stones(m))
    h.trees.len()
}}

pub fn frame(time: f32, width: u32, height: u32) {{
    let _ = GROWN
}}
"#
    );
    map_package(name, &main)
}

/// The build of a map the history refuses fails with E0704, naming the opening, and before the
/// history runs: well inside the constant's fuel and in under a few seconds.
fn refused(dir: &Path, opening: &str, why: &str) {
    let t = std::time::Instant::now();
    let out = wrela_driver::build(dir);
    let took = t.elapsed().as_secs_f64();
    let text = wrela_diag::render::render_all(&out.sources, &out.diagnostics);
    let codes: Vec<&str> = out.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(codes, ["E0704"], "{text}");
    assert!(text.contains("the map can't be grown"), "{text}");
    assert!(text.contains(opening) && text.contains(why), "the refusal names `{opening}`: {text}");
    // The history at the preview's grain takes seconds of the build's time; the refusal, a
    // fraction of one (most of this is compiling).
    assert!(took < 20.0, "refused after {took:.1} s: did the history run?");
}

/// AC1, keepers: a map with an opening that has no keeper is refused before the history runs,
/// the opening named.
#[test]
fn an_opening_with_no_keeper_is_refused_by_name() {
    let edit = "m.openings[0].keeper = engine::place::Keeper::Nothing";
    let dir = grown_from("last-green/no-keeper", edit);
    refused(&dir, "the clearing's meadow", "no keeper");
}

/// AC1, keepers: a map with an opening whose keeper stops before the present (people who left)
/// is refused before the history runs, the opening named.
#[test]
fn an_opening_whose_keeper_stops_before_the_present_is_refused_by_name() {
    let edit = "m.openings[0].keeper = engine::place::Keeper::People { from: 300, to: map::PRESENT - 100 }";
    let dir = grown_from("last-green/keeper-gone", edit);
    refused(&dir, "the clearing's meadow", "left");
}

// ---- the bake --------------------------------------------------------------------------------

/// The floor built (its bake from the constants' cache when it's warm), once a test process:
/// its build's directory (under target/tmp: the package's own `build` is `wrela build`'s, which
/// keeps a list of what it wrote there and won't replace a file it didn't write).
fn floor() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let dir = repo_root().join("examples/last-green");
            let out = wrela_driver::build(&dir);
            assert!(
                !wrela_diag::has_errors(&out.diagnostics),
                "{}",
                wrela_diag::render::render_all(&out.sources, &out.diagnostics)
            );
            let build = crate::scratch_root().join("last-green-floor");
            let _ = std::fs::remove_dir_all(&build);
            for (path, bytes) in &out.files {
                let to = build.join(path);
                std::fs::create_dir_all(to.parent().expect("a parent")).expect("mkdir");
                std::fs::write(to, bytes).expect("write the build's file");
            }
            build
        })
        .clone()
}

/// The check's report on the bake (`files/check/report.json`).
fn report() -> Value {
    let text = std::fs::read_to_string(floor().join("files/check/report.json")).expect("report");
    serde_json::from_str(&text).expect("the report is JSON")
}

fn f(v: &Value) -> f64 {
    v.as_f64().expect("a number")
}

fn named(v: &Value) -> &str {
    v["name"].as_str().expect("a name")
}

/// AC1's reach and AC3's verdicts, from the check on the bake: every anchor's site, the town's
/// site, the Warden's arena and every relation's viewpoint reached on foot from the gate (on the
/// check's grid, and on the walkable mask a metre a cell, as the sim walks); the
/// dead share ≤ 15% of the walkable floor; every relation holds; every opening is open in the
/// present; walkers lost ≤ 25%; the critical path's pilgrims reach every anchor. And measured,
/// not gated: the share with no draw of the map's own, draws an hour and the median gap, and
/// keystones an hour, for the floor and each region; the critical path's median time.
#[test]
#[ignore = "long: bakes the floor (minutes when the constants' cache is cold)"]
fn what_the_check_says_of_the_floor() {
    let r = report();
    let area = f(&r["area_km2"]);
    assert!((2.7..3.3).contains(&area), "the floor's area: {area} km²");
    for s in r["sites"].as_array().expect("sites") {
        if s["critical"].as_bool() == Some(true) {
            assert_eq!(
                s["reached"].as_bool(),
                Some(true),
                "{} can't be reached from the gate",
                named(s)
            );
        }
    }
    let floor = &r["floor"];
    let dead = f(&floor["dead"]);
    let lost = f(&floor["pilgrims"]["lost"]);
    let magpies_lost = f(&floor["magpies"]["lost"]);
    eprintln!(
        "the floor: {:.2} km² walkable; dead {:.1}% (spike 17: 14.4%); no draw of the map's own {:.1}% (75.1%); lost {:.1}% (pilgrims; magpies {:.1}%; spike 17: 23%); draws {:.0} an hour, median gap {:.0} s (38, 62 s); keystones {:.1} an hour (2.7)",
        f(&floor["walkable_km2"]),
        100.0 * dead,
        100.0 * f(&floor["quiet"]),
        100.0 * lost,
        100.0 * magpies_lost,
        f(&floor["pilgrims"]["draws_per_hour"]),
        f(&floor["pilgrims"]["median_gap"]),
        f(&floor["pilgrims"]["keystones_per_hour"]),
    );
    for region in r["regions"].as_array().expect("regions") {
        eprintln!(
            "  {}: {:.2} km², dead {:.1}%, no draw of the map's own {:.1}%, draws {:.0} an hour, median gap {:.0} s, keystones {:.1} an hour",
            named(region),
            f(&region["walkable_km2"]),
            100.0 * f(&region["dead"]),
            100.0 * f(&region["quiet"]),
            f(&region["pilgrims"]["draws_per_hour"]),
            f(&region["pilgrims"]["median_gap"]),
            f(&region["pilgrims"]["keystones_per_hour"]),
        );
    }
    assert!(dead <= 0.15, "dead {:.1}% of the walkable floor (≤ 15%)", 100.0 * dead);
    assert!(lost <= 0.25, "walkers lost {:.1}% (≤ 25%)", 100.0 * lost);
    for rel in r["relations"].as_array().expect("relations") {
        eprintln!("  {}: draw {:.2}, before {:.2}", named(rel), f(&rel["draw"]), f(&rel["before"]));
        assert_eq!(rel["holds"].as_bool(), Some(true), "{} doesn't hold", named(rel));
        assert_eq!(
            rel["reached"].as_bool(),
            Some(true),
            "{}'s viewpoint can't be reached",
            named(rel)
        );
    }
    for o in r["openings"].as_array().expect("openings") {
        assert_eq!(
            o["open"].as_bool(),
            Some(true),
            "{} isn't open: {:.2} covered",
            named(o),
            f(&o["cover"])
        );
    }
    // On foot as the sim walks: the walkable mask a metre a cell, its logs, walls and thickets
    // closed (`footing::Walkable`; the check's own grid is 4 m a cell).
    let text = std::fs::read_to_string(super::last_green::floor().join("files/check/reach.json"))
        .expect("reach.json");
    let on_foot: Value = serde_json::from_str(&text).expect("reach.json is JSON");
    for part in ["sites", "relations"] {
        for s in on_foot[part].as_array().expect("its list") {
            assert_eq!(
                s["reached"].as_bool(),
                Some(true),
                "{} can't be walked to from the gate (the walkable mask)",
                named(s)
            );
        }
    }
    let c = &r["critical"];
    eprintln!(
        "the critical path: {} of {} pilgrims reached every anchor; the median {:.0} min from the gate at a run (#44: 2–4 hours for the whole path)",
        c["reached"],
        c["of"],
        f(&c["median_s"]) / 60.0
    );
    assert_eq!(c["reached"], c["of"], "pilgrims on the trails reach every anchor: {c}");
}

/// AC2: a second build of the unchanged floor computes no constant.
#[test]
#[ignore = "long: bakes the floor (minutes when the constants' cache is cold)"]
fn the_floor_builds_again_from_its_cache() {
    let _ = floor();
    let t = std::time::Instant::now();
    let out = wrela_driver::build(&repo_root().join("examples/last-green"));
    assert!(!wrela_diag::has_errors(&out.diagnostics));
    eprintln!(
        "the floor built again in {:.2} s: {} computed, {} from the cache",
        t.elapsed().as_secs_f64(),
        out.consts.computed,
        out.consts.cached
    );
    assert_eq!(out.consts.computed, 0, "the second build computed constants");
    assert!(out.consts.cached > 0);
}

/// §9: each lookbook shot has the check's numbers for its view (`shots.json`: its strongest draw,
/// how many draw, and each draw), in the lookbook's order.
#[test]
#[ignore = "long: bakes the floor (minutes when the constants' cache is cold)"]
fn each_lookbook_shot_has_the_checks_numbers() {
    let text = std::fs::read_to_string(floor().join("files/check/shots.json")).expect("shots.json");
    let views: Value = serde_json::from_str(&text).expect("shots.json is JSON");
    let views = views.as_array().expect("an array");
    let lookbook = std::fs::read_to_string(repo_root().join("examples/last-green/lookbook.wrela"))
        .expect("lookbook.wrela");
    let names: Vec<&str> = lookbook
        .split("name: \"")
        .skip(1)
        .map(|rest| rest.split('"').next().expect("a name"))
        .collect();
    assert_eq!(views.len(), names.len(), "{} shots, {} with numbers", names.len(), views.len());
    for (v, name) in views.iter().zip(&names) {
        assert_eq!(v["shot"].as_str(), Some(*name));
        let best = v["view"]["best"].as_f64().expect("its strongest draw");
        assert!((0.0..=1.0).contains(&best), "{name}: {best}");
        assert!(v["view"]["draws"].is_array(), "{name}: no draws");
    }
}

/// The lookdev reviews (2026-10-10): every lookbook shot is a valid view through its move. At its
/// start, middle and end, its eye stands at least 1 m clear of a trunk, and at least 35% of the
/// light from its subject reaches the eye through the crowns and trunks as drawn
/// (`history::shot_valid`: the town's eye had stood inside a trunk, and near trees and logs hid
/// the ring, the arena, the burn and the windthrow).
#[test]
#[ignore = "long: bakes the floor (minutes when the constants' cache is cold)"]
fn every_lookbook_shot_sees_its_subject() {
    let text = std::fs::read_to_string(floor().join("files/check/shots.json")).expect("shots.json");
    let views: Value = serde_json::from_str(&text).expect("shots.json is JSON");
    let mut bad = Vec::new();
    for v in views.as_array().expect("an array") {
        let name = v["shot"].as_str().expect("a name");
        let seen = v["seen"].as_f64().expect("seen");
        let clear = v["clear"].as_f64().expect("clear");
        eprintln!("{name}: its subject {seen:.2} seen, the eye {clear:.1} m from a trunk");
        if seen < 0.35 || clear < 1.0 {
            bad.push(format!("{name}: {seen:.2} seen, {clear:.1} m clear"));
        }
    }
    assert!(bad.is_empty(), "shots that don't see their subjects: {bad:?}");
}

/// AC2: the bake is ≤ 0.7 MB a km² compressed (each file gzipped, over the floor's area), each
/// tile a file read alone (`bake::decode_tile` reads one with no other).
#[test]
#[ignore = "long: bakes the floor (minutes when the constants' cache is cold)"]
fn the_bake_is_small_and_a_file_a_tile() {
    let dir = floor().join("files/floor");
    let mut raw = 0usize;
    let mut packed = 0usize;
    let mut tiles = 0;
    for e in std::fs::read_dir(&dir).expect("the bake's files") {
        let path = e.expect("entry").path();
        raw += std::fs::metadata(&path).expect("size").len() as usize;
        let z = std::process::Command::new("gzip")
            .args(["-9", "-c"])
            .arg(&path)
            .output()
            .expect("gzip");
        packed += z.stdout.len();
        if path.file_name().is_some_and(|n| n.to_string_lossy().starts_with('t')) {
            tiles += 1;
        }
    }
    let area = f(&report()["area_km2"]);
    let per = packed as f64 / 1.0e6 / area;
    eprintln!(
        "the bake: {tiles} tiles and the far layer, {raw} bytes, {packed} gzipped: {per:.2} MB a km² of {area:.2} km² (#44's estimate for the floor: 16 MB)"
    );
    assert!(per <= 0.7, "{per:.2} MB a km² (≤ 0.7)");
    assert!(tiles >= 48, "a file a tile: {tiles}");
}

/// AC2: the preview's check is within 5 points of the bake's dead share, with the same verdict
/// on every relation. The preview's history grows in a package of its own (the map's files, and
/// the trees' for the crowns as they're drawn), checked with the same settings.
#[test]
#[ignore = "long: bakes the floor and grows its preview (minutes when the constants' cache is cold)"]
fn the_previews_check_agrees_with_the_bakes() {
    let main = r#"
use engine::footing::obstacles_of
use engine::grow::{PREVIEW, simulate}
use engine::interest::{Evidence, FULL, check, report}
use std::io::{Shipped, ship}

@fuel(2 ** 44)
const PREVIEWED: Shipped = ship("preview", previewed())

fn previewed() -> Vec<(String, Vec<u8>)> {
    let m = map::map()
    let lo = map::LO - vec2(64.0)
    let hi = map::HI + vec2(64.0)
    let st = stones::stones(m)
    let h = simulate(ground::ground(), m, species::habits(), PREVIEW, lo, hi, st)
    let o = obstacles_of(h.trees, species::habits(), h.streams, st, stones::walls(m), lo, hi)
    var e = Evidence::of(h)
    e.crowns = trees::drawn_crowns(e.trees)
    e.solids = blockouts::solids(m)
    let c = check(ground::ground(), m, species::habits(), e, o, st, FULL)
    let text = report(ground::ground(), m, c)
    Vec::from([(String::from("report.json"), Vec::from(text.as_bytes()))])
}

pub fn frame(time: f32, width: u32, height: u32) {
    let _ = PREVIEWED.len()
}
"#;
    let dir = map_package("last-green/preview", main);
    // The trees' kinds, for their crowns as they're drawn: trees.wrela, and the great oak's
    // package it reads.
    let floor = repo_root().join("examples/last-green");
    for f in ["trees.wrela", "blockouts.wrela"] {
        std::fs::copy(floor.join(f), dir.join(f))
            .expect("copy the trees' and the built things' files");
    }
    let manifest = std::fs::read_to_string(dir.join("wrela.toml")).expect("the manifest");
    let great = repo_root().join("examples/great-tree").display().to_string();
    std::fs::write(
        dir.join("wrela.toml"),
        format!("{manifest}great_tree = {{ path = {great:?} }}\n"),
    )
    .expect("write wrela.toml");
    let out = wrela_driver::build(&dir);
    assert!(
        !wrela_diag::has_errors(&out.diagnostics),
        "{}",
        wrela_diag::render::render_all(&out.sources, &out.diagnostics)
    );
    let (_, text) = out
        .files
        .iter()
        .find(|(p, _)| p == "files/preview/report.json")
        .expect("the preview's report");
    let preview: Value = serde_json::from_slice(text).expect("JSON");
    let bake = report();
    let (a, b) = (f(&preview["floor"]["dead"]), f(&bake["floor"]["dead"]));
    eprintln!("dead: the preview's {:.1}%, the bake's {:.1}%", 100.0 * a, 100.0 * b);
    assert!(
        (a - b).abs() <= 0.05,
        "the preview's dead share {:.1}%, the bake's {:.1}%",
        100.0 * a,
        100.0 * b
    );
    let rels = |r: &Value| -> Vec<(String, bool)> {
        r["relations"]
            .as_array()
            .expect("relations")
            .iter()
            .map(|x| (named(x).to_string(), x["holds"].as_bool().expect("holds")))
            .collect()
    };
    assert_eq!(rels(&preview), rels(&bake), "a relation's verdict differs");
}

/// The floor's own tests (`wrela test examples/last-green`, tests.wrela and main.wrela): AC2's
/// head room over the baked trees; AC11's ground cover against the history's light; AC9's water,
/// one surface and where the map and the flow put it; AC4's player walking into each thing, its
/// trunks as drawn, and its camera over the floor's walk; the floor's walk walked to its end;
/// AC7's wildwood tiles matching the wider wood; AC12's trunks that differ (a fork per tree);
/// its sound (each place its own, changing as smoothly as the place: the outside audit's point
/// 6); and its first frames on the GPU.
#[test]
#[ignore = "long: bakes the floor (minutes when the constants' cache is cold)"]
fn the_floors_own_tests_pass() {
    let _ = floor();
    assert_eq!(super::tests_pass(&repo_root().join("examples/last-green")), 20);
}

/// The body of WGSL function `name` in `text` (to its closing brace at the line's start).
fn wgsl_body<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let start = text.find(&format!("fn {name}("))?;
    let end = text[start..].find("\n}\n").map_or(text.len(), |e| start + e + 3);
    Some(&text[start..end])
}

/// AC9's bounds (spike 08's): the water's pass traces no ray through the scene. Its pipeline
/// binds no buffer of the scene's geometry: its storage is the water's mesh and the probes'
/// columns alone, so what it reflects comes from the screen (the scene under it, `below`), the
/// probes and the sky. And AC12's cooked materials: the fragments of bark (the trees', the
/// snags' and the logs' limbs), rock and masonry (the stones, the tower, the walls) evaluate no
/// field: none holds the value noise's lattice hash (`engine::noise`'s, which their fields'
/// noise is made of). Bark reads its ridges from a cooked tile, stone its cooked colours.
#[test]
#[ignore = "long: bakes the floor (minutes when the constants' cache is cold)"]
fn the_floors_pipelines_keep_their_bounds() {
    let build = floor();
    let (mut water, mut cooked) = (0, 0);
    for entry in std::fs::read_dir(&build).expect("the build") {
        let path = entry.expect("an entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !(name.starts_with("pipeline_") && name.ends_with(".wgsl")) {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("the pipeline's WGSL");
        if text.contains("fn water_shade(") {
            water += 1;
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if line.starts_with("var<storage") {
                    let bound = line.split_whitespace().nth(1).unwrap_or("").trim_end_matches(':');
                    assert!(
                        ["verts", "tris", "lt_grounds"].contains(&bound),
                        "{name}: the water's pass binds `{bound}` ({}), not its mesh or the probes'",
                        lines[i]
                    );
                }
            }
        }
        for f in ["bark_shade", "stone_shade"] {
            if let Some(body) = wgsl_body(&text, f) {
                cooked += 1;
                for hash in ["1597334677u", "3812015801u", "2798796415u"] {
                    assert!(!body.contains(hash), "{name}: `{f}` evaluates value noise ({hash})");
                }
            }
        }
    }
    assert_eq!(water, 1, "the water's pipelines");
    assert!(cooked >= 2, "only {cooked} of bark's and stone's pipelines");
}

// ---- the floor, frame by frame ---------------------------------------------------------------

/// The floor's test build (its `@testing` exports), loaded on the native host and driven a frame
/// at a time at 60 frames a second.
struct Floor {
    host: wrela_host::Host,
    frame: u32,
    /// The output's size (the scene is half of it each way).
    size: (u32, u32),
    /// Input for the frames, by frame (`wrela_host::parse_script`'s).
    script: Vec<wrela_host::Scripted>,
}

/// What `test_capture` copies (testing.wrela's `Capture`, by discriminant).
const CAPTURE_DEPTH: i32 = 0;
const CAPTURE_TAGS: i32 = 1;
const CAPTURE_EXPOSURE: i32 = 2;

/// Tag classes (engine::view): a tag is its class times 64, plus which one.
fn class(tag: f32) -> u32 {
    (tag.max(0.0) / 64.0 + 0.001) as u32
}
const CLASS_GROUND: u32 = 1;
const CLASS_GRASS: u32 = 2;
const CLASS_ROCK: u32 = 6;
const CLASS_FLOWER: u32 = 7;
const CLASS_WATER: u32 = 8;

impl Floor {
    fn load(name: &str) -> Floor {
        Floor::load_sized(name, (320, 180))
    }

    /// The floor drawn `size` wide and high.
    fn load_sized(name: &str, size: (u32, u32)) -> Floor {
        Floor::load_with(name, size, &wrela_host::Options::default())
    }

    /// The floor drawn `size` wide and high, on a host with `options`.
    fn load_with(name: &str, size: (u32, u32), options: &wrela_host::Options) -> Floor {
        let _ = floor();
        let (dir, _) = wrela_tests::page("examples/last-green", name);
        let host = wrela_host::Host::load_with(&dir, options).expect("load the floor");
        Floor { host, frame: 0, size, script: Vec::new() }
    }

    fn step(&mut self) {
        self.host
            .lockstep_frame(self.frame, 60.0, self.size.0, self.size.1, &self.script)
            .unwrap_or_else(|e| panic!("frame {}: {e}", self.frame));
        self.frame += 1;
    }

    /// Keys pressed at frame `at` (each down and up in it: `KeyL`, `Digit7`, `Enter`).
    fn keys(&mut self, at: u32, keys: &[&str]) {
        let events: Vec<String> = keys
            .iter()
            .map(|k| format!(r#"{{"frame": {at}, "type": "key", "key": "{k}"}}"#))
            .collect();
        let mut more = wrela_host::parse_script(&format!("[{}]", events.join(", "))).expect("keys");
        self.script.append(&mut more);
    }

    /// The lines the floor printed since the last call, each with the frame it was printed in.
    fn printed(&mut self) -> Vec<String> {
        self.host.take_logs()
    }

    fn steps(&mut self, n: u32) {
        for _ in 0..n {
            self.step();
        }
    }

    fn call(&mut self, name: &str, args: &[wrela_host::Value]) -> Vec<f64> {
        let v = self.host.call_export(name, args).unwrap_or_else(|e| panic!("{name}: {e}"));
        v.iter()
            .map(|x| match x {
                wrela_host::Value::F32(f) => f64::from(*f),
                wrela_host::Value::I32(i) => f64::from(*i),
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    /// The camera the last frame was drawn by.
    fn camera(&mut self) -> wrela_tests::camera::Cam {
        wrela_tests::camera::Cam::of(&self.call("test_camera", &[]))
    }

    /// What the last frame drew (`test_capture`'s `what`), read back as floats.
    fn capture(&mut self, what: i32) -> Vec<f32> {
        let n = self.call("test_capture", &[wrela_host::Value::I32(what)]);
        assert!(n[0] > 0.0, "nothing to capture");
        let newest = *self.host.buffers().last().expect("a buffer");
        wrela_tests::f32s(&self.host.read_buffer(newest).expect("read the capture"))
    }

    /// The ground's height at `p`, as the floor draws it.
    fn ground(&mut self, p: [f32; 2]) -> f32 {
        self.call("test_ground", &[wrela_host::Value::F32(p[0]), wrela_host::Value::F32(p[1])])[0]
            as f32
    }

    /// An eye's height (1.7 m) over the ground at `p`.
    fn eye(&mut self, p: [f32; 2]) -> [f32; 3] {
        [p[0], self.ground(p) + 1.7, p[1]]
    }

    /// The camera held at `eye` looking at `at` until the floor round it has settled
    /// (`test_settled`: its tiles in and placed, its shadows drawn, its probes baked there), and
    /// `settle` frames more (temporal AA, the exposure).
    fn look(&mut self, eye: [f32; 3], at: [f32; 3], settle: u32) {
        self.hold(eye, at);
        self.steps(3);
        let from = self.frame;
        while self.call("test_settled", &[])[0] < 0.5 {
            assert!(self.frame < from + 1800, "the floor round {eye:?} isn't settled after 30 s");
            self.step();
        }
        self.steps(settle);
    }

    /// The screen, saved as a PNG at `path` (the tests' evidence).
    fn save(&mut self, path: &Path) {
        let rgba = self.host.read_screen().expect("the screen");
        wrela_host::image::write_png(path, self.size.0, self.size.1, &rgba)
            .expect("save the screen");
    }

    /// Frames until the kinds are cooked and the tiles round the player placed.
    fn until_ready(&mut self) {
        let from = self.frame;
        while self.call("test_ready", &[])[0] < 0.5 {
            assert!(
                self.frame < from + 900,
                "the floor isn't ready after {} frames",
                self.frame - from
            );
            self.step();
        }
    }

    fn hold(&mut self, eye: [f32; 3], at: [f32; 3]) {
        let args: Vec<wrela_host::Value> =
            eye.iter().chain(&at).map(|&x| wrela_host::Value::F32(x)).collect();
        self.call("test_hold", &args);
    }
}

/// AC4's no trap: the camera held inside a tree's crown, exactly at its middle (where its
/// levels' patterns are anchored, `lod::anchor`) and a step either side, looking each way, and
/// inside its trunk: every frame draws, with the floor's streamed wood (`engine::wood`'s marks).
/// Playing spike 17, a camera there trapped (`lod.wrela:125`).
#[test]
#[ignore = "long: bakes the floor, needs a GPU"]
fn the_camera_inside_a_crown_of_the_floor_draws_its_frames() {
    let mut f = Floor::load("last-green-inside-a-crown");
    f.until_ready();
    for i in [0, 3, 17, 60] {
        let m = f.call("test_crown", &[wrela_host::Value::I32(i)]);
        assert!(m[3] > 0.0, "no tree {i} near the player");
        let middle = [m[0] as f32, m[1] as f32, m[2] as f32];
        for (dx, dy) in [(0.0, 0.0), (0.05, 0.0), (0.0, -0.05)] {
            let eye = [middle[0] + dx, middle[1] + dy, middle[2]];
            for (x, z) in [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0)] {
                f.hold(eye, [eye[0] + x, eye[1], eye[2] + z]);
                f.steps(2);
            }
        }
        // Inside its trunk, a metre up, looking along the ground.
        let foot = [m[0] as f32, m[1] as f32 - m[3] as f32 * 0.5 + 1.0, m[2] as f32];
        f.hold(foot, [foot[0] + 3.0, foot[1], foot[2] + 1.0]);
        f.steps(2);
    }
}

/// The calibration file the floor's check ships (history.wrela's `calibration`).
fn calibration() -> Value {
    let text = std::fs::read_to_string(floor().join("files/check/calibration.json"))
        .expect("calibration.json");
    serde_json::from_str(&text).expect("calibration.json is JSON")
}

/// The share of a level view's middle rows (the eye's level, two rows either side), within
/// `half` radians of its middle column, whose depth (`depth`, the scene's) reaches `dist`
/// metres along its ray, or the sky.
fn reaching(cam: &wrela_tests::camera::Cam, depth: &[f32], dist: f64, half: f64) -> f64 {
    let (w, h) = (cam.screen[0] as usize, cam.screen[1] as usize);
    let (mut seen, mut all) = (0usize, 0usize);
    for y in (h / 2 - 2)..(h / 2 + 3) {
        for x in 0..w {
            let px = [x as f64 + 0.5, y as f64 + 0.5];
            let ray = cam.ray(px);
            let side = wrela_tests::camera::dot(ray, cam.right);
            let ahead = wrela_tests::camera::dot(ray, cam.forward);
            if side.atan2(ahead).abs() > half {
                continue;
            }
            let z = f64::from(depth[y * w + x]);
            all += 1;
            if z <= 0.0 || cam.distance_at(px, z) >= dist {
                seen += 1;
            }
        }
    }
    seen as f64 / all.max(1) as f64
}

/// AC3's calibration: at 24 places spread over the floor (`calibration.json`), the camera held at
/// the check's eye above the ground, looking level all round (`views` views, each its sixth of
/// the circle), the renderer's share of the views' level middles that reaches `dist` metres (its
/// depth) against the model's (`interest::seen_beyond`): within 0.1 at each place.
#[test]
#[ignore = "long: bakes the floor, needs a GPU"]
fn the_checks_sight_matches_the_renderers_depth() {
    let cal = calibration();
    let (eye_h, dist, views) = (
        cal["eye"].as_f64().unwrap(),
        cal["dist"].as_f64().unwrap(),
        cal["views"].as_u64().unwrap(),
    );
    let half = std::f64::consts::PI / views as f64;
    let mut f = Floor::load_sized("last-green-calibration", (960, 540));
    f.until_ready();
    let shots = crate::scratch_root().join("last-green-calibration-views");
    std::fs::create_dir_all(&shots).expect("a directory for the views");
    let mut off = Vec::new();
    let mut sum = 0.0;
    let places = cal["places"].as_array().expect("places");
    for (i, p) in places.iter().enumerate() {
        let at = [p["at"][0].as_f64().unwrap(), p["at"][1].as_f64().unwrap()];
        let (yaw, y) = (p["yaw"].as_f64().unwrap(), p["ground"].as_f64().unwrap() + eye_h);
        let eye = [at[0] as f32, y as f32, at[1] as f32];
        let mut drawn = 0.0;
        for v in 0..views {
            let a = yaw + 2.0 * half * v as f64;
            let ahead =
                [(at[0] + 10.0 * a.cos()) as f32, y as f32, (at[1] + 10.0 * a.sin()) as f32];
            f.look(eye, ahead, 20);
            if v == 0 {
                f.save(&shots.join(format!("place-{i:02}.png")));
            }
            let cam = f.camera();
            let depth = f.capture(CAPTURE_DEPTH);
            drawn += reaching(&cam, &depth, dist, half) / views as f64;
        }
        let model = p["seen"].as_f64().unwrap();
        eprintln!(
            "place {i} at ({:.0}, {:.0}): {:.2} of the view reaches {dist} m drawn, {:.2} in the model",
            at[0], at[1], drawn, model
        );
        sum += (drawn - model).abs();
        if (drawn - model).abs() > 0.1 {
            off.push(format!(
                "place {i} at ({:.0}, {:.0}): drawn {drawn:.2}, model {model:.2}",
                at[0], at[1]
            ));
        }
    }
    eprintln!("the model against the renderer: {:.3} apart on average", sum / places.len() as f64);
    assert_eq!(places.len(), 24, "the calibration's places");
    assert!(off.is_empty(), "the model's sight is more than 0.1 from the renderer's: {off:?}");
}

/// AC3's landmarks can be made out: from where each landmark relation is judged
/// (`calibration.json`: `interest::relation_view`), looking at its site (a crag's top, another's
/// middle), the part of it that
/// shows (pixels of the ground or of rock, from the renderer's tags, whose depth puts them
/// within the site's reach of it) is at least 1° tall in some column.
#[test]
#[ignore = "long: bakes the floor, needs a GPU"]
fn each_landmark_can_be_made_out_from_where_it_is_seen() {
    let cal = calibration();
    let (w, h) = (960usize, 540usize);
    let mut f = Floor::load_sized("last-green-landmarks", (w as u32, h as u32));
    f.until_ready();
    let shots = crate::scratch_root().join("last-green-landmark-views");
    std::fs::create_dir_all(&shots).expect("a directory for the views");
    let mut small = Vec::new();
    for l in cal["landmarks"].as_array().expect("landmarks") {
        let name = l["name"].as_str().unwrap_or("?");
        let e: Vec<f32> =
            l["eye"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as f32).collect();
        let site = [l["site"][0].as_f64().unwrap(), l["site"][1].as_f64().unwrap()];
        let (aim, reach) = (l["aim"].as_f64().unwrap(), l["reach"].as_f64().unwrap());
        let target = [site[0] as f32, aim as f32, site[1] as f32];
        f.look([e[0], e[1], e[2]], target, 20);
        f.save(&shots.join(format!("{}.png", name.replace(' ', "-").replace('\'', ""))));
        let cam = f.camera();
        let depth = f.capture(CAPTURE_DEPTH);
        let tags = f.capture(CAPTURE_TAGS);
        let (sw, sh) = (cam.screen[0] as usize, cam.screen[1] as usize);
        // Each column's count of the landmark's pixels (the output's, each read at its scene
        // pixel's depth).
        let mut tallest = 0usize;
        for x in 0..w {
            let mut n = 0;
            for y in 0..h {
                let k = class(tags[y * w + x]);
                if k != CLASS_GROUND && k != CLASS_ROCK {
                    continue;
                }
                let (qx, qy) = ((x * sw / w).min(sw - 1), (y * sh / h).min(sh - 1));
                let z = f64::from(depth[qy * sw + qx]);
                if z <= 0.0 {
                    continue;
                }
                let p = cam.world_at([qx as f64 + 0.5, qy as f64 + 0.5], z);
                if ((p[0] - site[0]).powi(2) + (p[2] - site[1]).powi(2)).sqrt() < reach {
                    n += 1;
                }
            }
            tallest = tallest.max(n);
        }
        let degrees = tallest as f64 * (2.0 * cam.tan_half.atan()).to_degrees() / h as f64;
        eprintln!(
            "{name}: {degrees:.2}° of it shows (its draw in the check {:.2})",
            l["draw"].as_f64().unwrap_or(0.0)
        );
        if degrees < 1.0 {
            small.push(format!("{name}: {degrees:.2}°"));
        }
    }
    assert!(small.is_empty(), "landmarks under 1°: {small:?}");
}

/// AC10's trails read as paths: at each place along the critical path's routes (the road and
/// the trails: `calibration.json`'s `trails`), the camera at an eye's height on the route looking
/// at the trodden line 20 m on, the trail's mean display colour there (a 5 × 5 window of the
/// screen) against the ground's 3 m either side, as it shows with its cover (their two windows'
/// mean): at least 8/255 apart in some channel, at each place where the three windows show the
/// ground or its cover (grass, flowers: by the tags), not a tree, a stone or the sky.
#[test]
#[ignore = "long: bakes the floor, needs a GPU"]
fn the_trails_read_as_paths() {
    let cal = calibration();
    let (w, h) = (960usize, 540usize);
    let mut f = Floor::load_sized("last-green-trails", (w as u32, h as u32));
    f.until_ready();
    let shots = crate::scratch_root().join("last-green-trail-views");
    std::fs::create_dir_all(&shots).expect("a directory for the views");
    let places = cal["trails"].as_array().expect("trails");
    let (mut weak, mut measured) = (Vec::new(), 0usize);
    let mut all = Vec::new();
    for (i, t) in places.iter().enumerate() {
        let xz = |v: &Value| [v[0].as_f64().unwrap() as f32, v[1].as_f64().unwrap() as f32];
        let (eye, centre, across) = (xz(&t["eye"]), xz(&t["centre"]), xz(&t["across"]));
        let e = f.eye(eye);
        let c = [centre[0], f.ground(centre), centre[1]];
        f.look(e, c, 30);
        f.save(&shots.join(format!("trail-{i:02}.png")));
        let cam = f.camera();
        let screen = f.host.read_screen().expect("the screen");
        let tags = f.capture(CAPTURE_TAGS);
        let mut window = |p: [f32; 2]| -> Option<[f64; 3]> {
            let y = f64::from(f.ground(p)) + 0.02;
            let (px, _) =
                cam.project([f64::from(p[0]), y, f64::from(p[1])], [w as f64, h as f64])?;
            let (cx, cy) = (px[0] as isize, px[1] as isize);
            let (mut sum, mut n, mut ground) = ([0.0f64; 3], 0, 0);
            for dy in -2..=2isize {
                for dx in -2..=2isize {
                    let (x, y) = (cx + dx, cy + dy);
                    if x < 0 || y < 0 || x >= w as isize || y >= h as isize {
                        return None;
                    }
                    let o = y as usize * w + x as usize;
                    if matches!(class(tags[o]), CLASS_GROUND | CLASS_GRASS | CLASS_FLOWER) {
                        ground += 1;
                    }
                    for (k, v) in sum.iter_mut().enumerate() {
                        *v += f64::from(screen[4 * o + k]);
                    }
                    n += 1;
                }
            }
            (ground >= 20).then(|| sum.map(|v| v / f64::from(n)))
        };
        let side = |k: f32| [centre[0] + across[0] * k, centre[1] + across[1] * k];
        let (Some(on), Some(l), Some(r)) = (window(centre), window(side(-3.0)), window(side(3.0)))
        else {
            continue;
        };
        measured += 1;
        let apart = (0..3).map(|k| (on[k] - 0.5 * (l[k] + r[k])).abs()).fold(0.0, f64::max);
        all.push(apart);
        let name = t["route"].as_str().unwrap_or("?");
        eprintln!(
            "{name} at ({:.0}, {:.0}): the trail {apart:.1}/255 from the ground beside it",
            eye[0], eye[1]
        );
        if apart < 8.0 {
            weak.push(format!("{name} at ({:.0}, {:.0}): {apart:.1}/255", eye[0], eye[1]));
        }
    }
    eprintln!(
        "{measured} of {} places measured; the trail against the ground beside it: median {:.1}/255",
        places.len(),
        wrela_tests::median(&all)
    );
    assert!(
        measured * 2 >= places.len(),
        "only {measured} of {} places show the ground",
        places.len()
    );
    assert!(weak.is_empty(), "trails under 8/255 from the ground beside them: {weak:?}");
}

/// How much a run of frames flickers (`floor_flicker`): the mean warp error (/255), the share of
/// pixels over 8/255, and each class's mean; and the largest step in the screen's mean
/// brightness from a frame to the next, as a share of it.
struct Flicker {
    mean: f64,
    share: f64,
    by: [f64; 9],
    step: f64,
}

/// The look's flicker on the floor in motion, with the wind blowing: over the floor's walk's first
/// 10 s at a run, and over the first second shown after a jump to the beech wood's lookbook shot
/// (arriving somewhere: the frames are black until the light there is drawn). Each frame warped into the one before it, by spike 12's measure
/// (`camera::warp_error`: the earlier frame sampled bilinearly where it sees the same surface,
/// or the same sky); nothing left out, so the leaves in the wind, the water, the sky and the
/// stand-in are in it, as they are in plain rendering's. `plain`: the same content drawn as plain
/// rendering, at the output's size, with no temporal AA, no jitter and no look. On the walk, a
/// frame and the next every 4 frames (every frame at full size); after the jump, every frame.
fn floor_flicker(plain: bool) -> (Flicker, Flicker) {
    use wrela_tests::camera::{Seen, warp_error};
    let (w, h) = (960usize, 540usize);
    let name = if plain { "last-green-flicker-plain" } else { "last-green-flicker" };
    let mut f = Floor::load_sized(name, (w as u32, h as u32));
    if plain {
        f.call("test_plain", &[wrela_host::Value::I32(1)]);
    }
    f.until_ready();
    f.keys(f.frame + 1, &["KeyP"]);
    // The walk under way and temporal AA settled.
    f.steps(60);
    let grab = |f: &mut Floor| {
        let cam = f.camera();
        let depth = f.capture(CAPTURE_DEPTH);
        let tags = f.capture(CAPTURE_TAGS);
        (f.host.read_screen().expect("the screen"), depth, cam, tags)
    };
    // `frames` frames from now, a pair every `every`.
    let measure = |f: &mut Floor, frames: u32, every: u32, what: &str| {
        let mut last = grab(f);
        let (mut sum, mut over, mut motion, mut pairs) = (0.0, 0.0, 0.0, 0usize);
        let mut step = 0.0f64;
        let mut worst = (0.0f64, 0u32);
        let mut by = [(0.0f64, 0usize); 9];
        let end = f.frame + frames;
        while f.frame < end {
            f.step();
            if !f.frame.is_multiple_of(every) {
                // The first frame of the next pair.
                if (f.frame + 1).is_multiple_of(every) {
                    last = grab(f);
                }
                continue;
            }
            let now = grab(f);
            let (a, b) = (brightness(&last.0), brightness(&now.0));
            step = step.max((b - a).abs() / a.max(1.0));
            let warp = warp_error(&Seen::of(&last), &Seen::of(&now), (w, h), &[], &mut by);
            if warp.n > 10000 {
                sum += warp.mean;
                over += warp.over;
                motion += warp.motion;
                pairs += 1;
                if warp.mean > worst.0 {
                    worst = (warp.mean, f.frame);
                }
            }
            last = now;
        }
        let p = pairs.max(1) as f64;
        let (mean, share) = (sum / p, over / p);
        println!(
            "{}, {what}: flicker over {pairs} frames: mean warp error {mean:.3}/255, {:.2}% over 8/255, at {:.2} px a frame; worst {:.3}/255 at frame {}; brightness's largest step {:.1}%",
            if plain { "plain" } else { "the look" },
            share * 100.0,
            motion / p,
            worst.0,
            worst.1,
            step * 100.0
        );
        assert!(pairs * every as usize * 3 > frames as usize * 2, "only {pairs} frames compared");
        Flicker { mean, share, by: by.map(|(s, n)| s / n.max(1) as f64), step }
    };
    let walk = measure(&mut f, 600, wrela_tests::sized(4, 1), "the walk");
    // The walk stopped, and a jump to the beech wood's shot, dark after the open: its first
    // second shown.
    f.keys(f.frame + 1, &["KeyP", "KeyL", "Digit2", "Enter"]);
    f.steps(2);
    let from = f.frame;
    while brightness(&f.host.read_screen().expect("the screen")) < 0.5 {
        assert!(f.frame < from + 240, "the beech wood not shown 4 s after the jump");
        f.step();
    }
    println!("arriving: black for {} frames", f.frame - from);
    let arrival = measure(&mut f, 60, 1, "arriving");
    (walk, arrival)
}

/// A screen's mean brightness (of 255: its RGB's mean).
fn brightness(rgba: &[u8]) -> f64 {
    let sum: u64 =
        rgba.chunks_exact(4).map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2])).sum();
    sum as f64 / (rgba.len() / 4 * 3) as f64
}

/// AC10's flicker on the floor in motion, by M5's criterion (#51 Q8): on the walk and after
/// arriving somewhere, the look flickers no more than the same content drawn as plain
/// rendering, by at most 0.5/255 of mean warp error and 0.5 points of pixels over 8/255; and
/// arriving, the light comes with the first frame shown: the screen's brightness changes by at
/// most 3% from a frame to the next (the near shadows and the probes, drawn after the jump, were
/// steps of 50% and 80%). Each class's means are printed.
#[test]
#[ignore = "long: bakes the floor and walks it twice, needs a GPU"]
fn the_floor_doesnt_flicker_on_its_walk() {
    let (look_walk, look_arrival) = floor_flicker(false);
    let (plain_walk, plain_arrival) = floor_flicker(true);
    let arrival_step = look_arrival.step;
    let names =
        ["sky", "ground", "grass", "leaves", "bark", "stand-in", "rock", "flowers", "water"];
    for (what, look, plain) in
        [("the walk", look_walk, plain_walk), ("arriving", look_arrival, plain_arrival)]
    {
        for (k, name) in names.iter().enumerate() {
            println!(
                "  {what}, {name}: the look {:.3}/255, plain {:.3}/255",
                look.by[k], plain.by[k]
            );
        }
        println!(
            "{what}, the look against plain: {:.3} against {:.3}/255, {:.2}% against {:.2}% over 8/255",
            look.mean,
            plain.mean,
            look.share * 100.0,
            plain.share * 100.0
        );
        assert!(
            look.mean <= plain.mean + 0.5,
            "{what}: the look's warp error is {}/255, plain's {}/255",
            look.mean,
            plain.mean
        );
        assert!(
            look.share <= plain.share + 0.005,
            "{what}: {} of the look's pixels over 8/255, {} of plain's",
            look.share,
            plain.share
        );
    }
    assert!(
        arrival_step <= 0.03,
        "arriving, the look's brightness steps {:.1}% from a frame to the next",
        arrival_step * 100.0
    );
}

/// AC11's exposure: walking from the gate's clearing into the wood and back at a run, the camera
/// at an eye's height looking the way it goes, the metered exposure (one rule for the whole
/// floor, `METERING`) settles within 2 s of each stop (within 0.1 of a stop of where it ends 3 s
/// after it), and never flashes: what's shown (the scene's mean times the exposure) never comes
/// to 1.5 times its brightest settled level. The wood: the darkest of eight ways out of the
/// clearing, 60 m out.
#[test]
#[ignore = "long: bakes the floor, needs a GPU"]
fn the_exposure_settles_in_two_seconds_with_no_flash() {
    let mut f = Floor::load("last-green-exposure");
    f.until_ready();
    let clearing = [-1.5f32, 0.0];
    // The darkest way out: each held 60 m out, looking on, its scene's mean once settled.
    let (mut wood, mut darkest) = ([0.0f32; 2], f64::MAX);
    for k in 0..8 {
        let a = std::f32::consts::TAU * k as f32 / 8.0;
        let p = [clearing[0] + 60.0 * a.cos(), clearing[1] + 60.0 * a.sin()];
        let e = f.eye(p);
        f.look(e, [e[0] + a.cos(), e[1], e[2] + a.sin()], 90);
        let mean = f64::from(f.capture(CAPTURE_EXPOSURE)[1]);
        if mean < darkest {
            darkest = mean;
            wood = p;
        }
    }
    // Settled in the clearing, then the walk in, three seconds still, the walk out, three
    // seconds still: each frame's exposure and shown brightness.
    let mut frames: Vec<(f64, f64)> = Vec::new();
    let mut stops = Vec::new();
    let e = f.eye(clearing);
    f.look(e, [e[0] + (wood[0] - clearing[0]), e[1], e[2] + (wood[1] - clearing[1])], 180);
    for (from, to) in [(clearing, wood), (wood, clearing)] {
        let d = [to[0] - from[0], to[1] - from[1]];
        let len = (d[0] * d[0] + d[1] * d[1]).sqrt();
        let steps = (len / 5.0 * 60.0) as u32;
        for i in 0..=steps + 180 {
            let t = (i.min(steps) as f32) / steps as f32;
            let p = [from[0] + d[0] * t, from[1] + d[1] * t];
            let e = f.eye(p);
            f.hold(e, [e[0] + d[0] / len, e[1], e[2] + d[1] / len]);
            f.step();
            let x = f.capture(CAPTURE_EXPOSURE);
            frames.push((f64::from(x[0]), f64::from(x[0]) * f64::from(x[1])));
            if i == steps {
                stops.push(frames.len() - 1);
            }
        }
    }
    let mut brightest_settled = 0.0f64;
    for &s in &stops {
        let settled = frames[s + 180].0;
        let at_two = frames[s + 120].0;
        let off = (at_two / settled).log2().abs();
        eprintln!(
            "after the stop at frame {s}: the exposure {at_two:.3} at 2 s, {settled:.3} at 3 s ({off:.3} stops apart); shown {:.3}",
            frames[s + 180].1
        );
        assert!(off <= 0.1, "the exposure is {off:.3} stops from settled 2 s after a stop");
        brightest_settled = brightest_settled.max(frames[s + 180].1);
    }
    let brightest = frames.iter().map(|x| x.1).fold(0.0, f64::max);
    eprintln!("shown brightness: at most {brightest:.3}, settled at most {brightest_settled:.3}");
    assert!(
        brightest <= 1.5 * brightest_settled,
        "a flash: {brightest:.3} shown, {brightest_settled:.3} settled"
    );
}

/// The floor's systems (#54 §12), their slices (ms), and the labels their work is timed under (a
/// label names everything up to the next, `std::gpu::label`). A label no system claims fails the
/// test, so a system's work can't drop out of its slice unseen. Cooking is streaming's and the
/// caches' that follow the player: the clipmap's texels, the shadow cascades' strips, the
/// probes baked again where the eye has gone.
const SYSTEMS: [(&str, f64, &[&str]); 9] = [
    ("terrain", 1.5, &["terrain"]),
    (
        "vegetation",
        4.0,
        &[
            "grass",
            "vegetation choose",
            "vegetation occlusion",
            "vegetation prepass",
            "vegetation",
            "vegetation far",
        ],
    ),
    ("characters", 1.5, &["creature", "motion"]),
    ("light", 2.0, &["shadow moving", "probes again", "probes open sky", "shafts"]),
    ("water", 1.5, &["water"]),
    ("sky", 1.0, &["sky", "clouds", "air", "clouds noise", "clouds weather", "clouds shadow"]),
    (
        "cooking",
        1.0,
        &[
            "terrain cook",
            "shadows near",
            "shadows far",
            "probes occluders",
            "probes bake",
            "wood shift",
            "built things",
        ],
    ),
    ("temporal AA", 1.0, &["temporal"]),
    ("look", 2.0, &["look"]),
];

/// Each system's time (ms) in each of `frames` (its labels' times summed), from a run's timings.
fn system_times(
    timings: &[wrela_host::GpuTiming],
    frames: std::ops::Range<usize>,
) -> Vec<(&'static str, f64, Vec<f64>)> {
    let unclaimed: std::collections::BTreeSet<&str> = timings
        .iter()
        .filter(|t| {
            frames.contains(&t.frame)
                && !SYSTEMS.iter().any(|(_, _, ls)| ls.contains(&t.label.as_str()))
        })
        .map(|t| t.label.as_str())
        .collect();
    assert!(unclaimed.is_empty(), "timed under labels no system claims: {unclaimed:?}");
    SYSTEMS
        .iter()
        .map(|(name, slice, labels)| {
            let mut per: std::collections::BTreeMap<usize, f64> =
                frames.clone().map(|f| (f, 0.0)).collect();
            for t in timings {
                if let Some(ms) = per.get_mut(&t.frame)
                    && labels.contains(&t.label.as_str())
                {
                    *ms += t.nanos / 1e6;
                }
            }
            (*name, *slice, per.into_values().collect())
        })
        .collect()
}

/// A lookbook shot held (L, its number, Enter at the next frame) and settled, then `frames`
/// frames timed: each system's median, and the share of the last frame's pixels that are water.
fn shot_times(f: &mut Floor, shot: u32, frames: u32) -> (Vec<(&'static str, f64, f64)>, f64) {
    let mut keys = vec!["KeyL".to_string()];
    keys.extend(shot.to_string().chars().map(|d| format!("Digit{d}")));
    keys.push("Enter".into());
    let at = f.frame;
    f.keys(at, &keys.iter().map(String::as_str).collect::<Vec<_>>());
    f.step();
    f.steps(3);
    f.until_ready();
    // The shot's settle (lookbook::SETTLE, 5 s): it holds still while its caches settle.
    f.steps(240);
    timed_view(f, frames)
}

/// Each system's median time over the next `frames` frames of the view the floor holds, and the
/// share of the screen that's water.
fn timed_view(f: &mut Floor, frames: u32) -> (Vec<(&'static str, f64, f64)>, f64) {
    let _ = f.host.take_timings();
    let from = f.frame as usize;
    f.steps(frames);
    let times = system_times(&f.host.take_timings().expect("timings"), from..f.frame as usize);
    let tags = f.capture(CAPTURE_TAGS);
    let water =
        tags.iter().filter(|&&t| class(t) == CLASS_WATER).count() as f64 / tags.len() as f64;
    (times.into_iter().map(|(n, s, v)| (n, s, wrela_tests::median(&v))).collect(), water)
}

/// AC8's slices, each system timed alone (the native host's serial mode) at 1080p, #55's
/// Method: over the floor's walk, streaming as it goes (a minute and a half from its first full
/// frame; the whole walk with `WRELA_FULL`), each system's median within its slice or over it
/// by less than a quarter (the slack lends it), and cooking (streaming, and the caches that
/// follow the player) at most 1.0 ms a frame on average; in the beech high forest's shot,
/// vegetation at most 4.0 ms (spike 17: 10.2); and looking across the mere from its shore, with
/// water over 30% of the screen, water at most 1.5 ms (each may borrow under a quarter more).
#[test]
#[ignore = "measure: the floor's walk and two shots in serial timing, needs a GPU"]
fn the_floors_systems_keep_their_slices() {
    let options = wrela_host::Options {
        timing: wrela_host::Timing::Serial,
        ..wrela_host::Options::default()
    };
    let mut f = Floor::load_with("last-green-systems", (1920, 1080), &options);
    f.keys(1, &["KeyP"]);
    let frames = if std::env::var("WRELA_FULL").is_ok() { 24 * 60 * 60 } else { WALK_FRAMES };
    // The walk's first full frame (`playable`), then its frames.
    let mut start = None;
    while start.is_none() {
        assert!(f.frame < 1200, "the floor wasn't playable after {} frames", f.frame);
        f.step();
        if f.printed().iter().any(|l| l.contains("playable at frame")) {
            start = Some(f.frame as usize);
        }
    }
    let start = start.unwrap_or(0);
    let _ = f.host.take_timings();
    f.steps(frames);
    let walk = system_times(&f.host.take_timings().expect("timings"), start..f.frame as usize);
    let mut over = Vec::new();
    for (name, slice, v) in &walk {
        let (m, mean, worst) = (
            wrela_tests::median(v),
            v.iter().sum::<f64>() / v.len().max(1) as f64,
            v.iter().copied().fold(0.0, f64::max),
        );
        let borrows = if m > *slice { " (borrows from the slack)" } else { "" };
        eprintln!(
            "{name:12} slice {slice:4.1} ms: median {m:6.3} ms, mean {mean:6.3}, worst {worst:6.3}{borrows}"
        );
        if m > slice * 1.25 {
            over.push(format!("{name}: {m:.2} ms over the walk (slice {slice})"));
        }
        if *name == "cooking" && mean > *slice {
            over.push(format!("cooking: {mean:.2} ms a frame on average over the walk"));
        }
    }
    let (beech, _) = shot_times(&mut f, 2, 120);
    // The mere looked across from over its water, a little out from the north-west shore (the
    // lookbook's shot of it shows 16% water, the shore's shrubs hide half of it from the bank:
    // AC9 times water over 30% of the screen).
    let shore = f.ground([207.0, -337.0]);
    f.look([235.0, shore + 1.5, -365.0], [270.0, shore - 3.0, -400.0], 60);
    let (mere, water) = timed_view(&mut f, 120);
    f.save(&crate::scratch_root().join("last-green-mere-view.png"));
    let of = |t: &[(&str, f64, f64)], n: &str| t.iter().find(|x| x.0 == n).map_or(0.0, |x| x.2);
    eprintln!("beech high forest: vegetation {:.3} ms (slice 4.0)", of(&beech, "vegetation"));
    eprintln!(
        "the mere: water {:.3} ms (slice 1.5) with {:.0}% of the screen water",
        of(&mere, "water"),
        water * 100.0
    );
    // (A system over its slice by less than 25% borrows from the slack, if the frame passes:
    // the walk's frames, in Chrome.)
    for (what, t, system, slice) in
        [("the beech", &beech, "vegetation", 4.0), ("the mere", &mere, "water", 1.5)]
    {
        let ms = of(t, system);
        if ms > slice * 1.25 {
            over.push(format!("{system} in {what}: {ms:.2} ms"));
        } else if ms > slice {
            eprintln!("  {system} in {what} borrows {:.2} ms from the slack", ms - slice);
        }
    }
    assert!(water >= 0.3, "the view of the mere shows {:.0}% water, not 30%", water * 100.0);
    assert!(over.is_empty(), "over their slices: {over:?}");
}

/// std::hash's `Hasher` (compiler/std/hash.wrela), as the floor's `hash_bake` uses it.
struct Hasher(u64);

impl Hasher {
    fn new() -> Hasher {
        Hasher(0x243f_6a88_85a3_08d3)
    }

    fn write_u64(&mut self, x: u64) {
        let s = (self.0 ^ x).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        self.0 = s ^ (s >> 29);
    }

    /// A length, then bytes eight to a word (`write_str`'s way).
    fn write_bytes(&mut self, b: &[u8]) {
        self.write_u64(b.len() as u64);
        for chunk in b.chunks(8) {
            let mut w = [0u8; 8];
            w[..chunk.len()].copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(w));
        }
    }

    fn finish(&self) -> u64 {
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

/// The floor's `hash_bake` line, of the bake's files as the build wrote them.
fn bake_hash_on_disk() -> String {
    let dir = floor().join("files/floor");
    let (mut sum, mut files, mut bytes) = (0u64, 0, 0);
    for entry in std::fs::read_dir(&dir).expect("the bake's files") {
        let path = entry.expect("an entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).expect("a name").to_string();
        let b = std::fs::read(&path).expect("a file of the bake");
        let mut h = Hasher::new();
        h.write_bytes(name.as_bytes());
        h.write_bytes(&b);
        sum = sum.wrapping_add(h.finish());
        files += 1;
        bytes += b.len();
    }
    format!("the bake's bytes: {files} files, {bytes} bytes, hash {sum}, 0 missing")
}

/// AC2's same bytes in every host: the bake as the floor reads it (F9: each of its files
/// fetched and hashed, `hash_bake`) in the native host and in Chrome, and as the build wrote it
/// to disk: the same files, bytes and hash.
#[test]
#[ignore = "long: bakes the floor, needs Chrome and a GPU"]
fn the_bake_is_the_same_bytes_in_both_hosts() {
    let disk = bake_hash_on_disk();
    let mut f = Floor::load("last-green-bake-hash");
    f.keys(1, &["F9"]);
    let mut native = None;
    while native.is_none() && f.frame < 900 {
        f.step();
        native = f
            .printed()
            .into_iter()
            .find_map(|l| l.find("the bake's bytes").map(|i| l[i..].to_string()));
    }
    // The native host holds the GPU's lock while it's loaded: Chrome's run waits for it.
    drop(f);
    let (_, chrome, _, _) = floor_in_chrome(
        "last-green-bake-hash-chrome",
        600,
        r#"[{"frame": 1, "type": "key", "key": "F9"}]"#,
        false,
    );
    let in_chrome = chrome
        .printed
        .iter()
        .find_map(|(_, l)| l.find("the bake's bytes").map(|i| l[i..].to_string()));
    eprintln!("on disk: {disk}\nnative: {native:?}\nChrome: {in_chrome:?}");
    assert_eq!(native.as_deref(), Some(disk.as_str()), "the native host reads other bytes");
    assert_eq!(in_chrome.as_deref(), Some(disk.as_str()), "Chrome reads other bytes");
}

/// AC4's replays and AC5's harness: the floor's walk (P pressed at the first frame) in Chrome's
/// test mode and in the native host, frame for frame at 60 a second: the play harness's reports,
/// each 2 s, the same lines in both hosts; and Chrome's tick log replays natively, every tick to
/// the same state hash.
#[test]
#[ignore = "long: bakes the floor, needs Chrome, python3 and a GPU"]
fn the_floors_walk_replays_and_reports_the_same_in_both_hosts() {
    let natively = same_in_both_hosts(
        "last-green-hosts",
        r#"[{"frame": 1, "type": "key", "key": "KeyP"}]"#,
        720,
    );
    assert!(natively.len() >= 5, "only {} reports natively", natively.len());
}

/// The fawn's encounter (behaviour.wrela) in both hosts: the player travels to 46 m in front of
/// the fawn and walks up, stands, and walks on. Its reports (the fawn's state among them) are the
/// same in Chrome and natively, and Chrome's tick log replays natively, every tick to the same
/// state hash; and the fawn watched, then fled.
#[test]
#[ignore = "long: bakes the floor, needs Chrome, python3 and a GPU"]
fn the_fawns_encounter_replays_the_same_in_both_hosts() {
    let natively = same_in_both_hosts("last-green-fawn", FAWN_WALK_UP, 2160);
    let doing: Vec<String> = natively
        .iter()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|r| r["fawn"][0].as_str().unwrap_or("").to_string())
        .collect();
    println!("the fawn: {doing:?}");
    let watched = doing.iter().position(|d| d == "watch").expect("it never watched");
    assert!(doing[watched..].iter().any(|d| d == "flee"), "it watched but never fled");
}

/// The player's walk up to the fawn: a travel to it (G 61 Enter: the sites are 0 to 58, the egg
/// 59, its road 60, the fawn 61), then walk 10 s, stand 8 s, and walk on 9.5 s.
const FAWN_WALK_UP: &str = r#"[
    {"frame": 1, "type": "key", "key": "KeyG"},
    {"frame": 1, "type": "key", "key": "Digit6"},
    {"frame": 1, "type": "key", "key": "Digit1"},
    {"frame": 1, "type": "key", "key": "Enter"},
    {"frame": 150, "type": "keydown", "key": "KeyW"},
    {"frame": 750, "type": "keyup", "key": "KeyW"},
    {"frame": 1230, "type": "keydown", "key": "KeyW"},
    {"frame": 1800, "type": "keyup", "key": "KeyW"}
]"#;

/// The floor run with `script` for `frames` frames at 60 a second in Chrome's test mode and in
/// the native host, frame for frame: the play harness's reports the same in both, and Chrome's
/// tick log replayed natively, every tick to the same state hash (a hash that differs fails the
/// replay at its tick). The native host's reports.
fn same_in_both_hosts(name: &str, text: &str, frames: u32) -> Vec<String> {
    let _ = floor();
    let (dir, rel) = wrela_tests::page("examples/last-green", name);
    let script = wrela_host::parse_script(text).expect("a script");
    let run = wrela_tests::ChromeRun {
        script: Some(text.into()),
        workers: 4,
        ..wrela_tests::ChromeRun::new(frames, 320, 180, 60.0)
    };
    let chrome = wrela_tests::run_in_chrome_with(&rel, run);
    let reports = |lines: Vec<&str>| -> Vec<String> {
        lines.into_iter().filter(|l| l.contains("\"report\"")).map(String::from).collect()
    };
    let in_chrome = reports(chrome.printed.iter().map(|(_, l)| l.as_str()).collect());
    let mut native = wrela_host::Host::load(&dir).expect("load the floor");
    for i in 0..frames {
        native
            .lockstep_frame(i, 60.0, 320, 180, &script)
            .unwrap_or_else(|e| panic!("frame {i}: {e}"));
    }
    let logs = native.take_logs();
    let natively = reports(logs.iter().map(String::as_str).collect());
    println!(
        "{} reports in Chrome, {} natively; the last: {:?}",
        in_chrome.len(),
        natively.len(),
        natively.last()
    );
    assert_eq!(in_chrome, natively, "the hosts' reports differ");
    let ticks = chrome.ticks.expect("the ticker's ticks");
    let log = ticks.log.expect("a tick log");
    assert!(log.ticks.len() as u32 > frames * 5 / 6, "{} ticks", log.ticks.len());
    let built = wrela_host::CpuBuild::load(&dir).expect("load on the CPU");
    assert_eq!(built.replay(&log, 2).expect("replays") as usize, log.ticks.len());
    natively
}

// ---- in Chrome ------------------------------------------------------------------------------

/// A run of the floor in Chrome at 1080p, `frames` frames, seven helpers, with `script`'s keys:
/// back to back, two frames in flight (`saturate`: the GPU kept busy, its clock high, so a
/// frame's span is its work: #55's Method for budgets), or paced at 60 Hz as in play. The run,
/// each frame's GPU span (ms), and each frame's CPU time (ms: `results/frames.json`).
fn floor_in_chrome(
    name: &str,
    frames: u32,
    script: &str,
    saturate: bool,
) -> (PathBuf, wrela_tests::BrowserRun, Vec<f64>, Vec<f64>) {
    floor_in_chrome_throttled(name, frames, script, saturate, 0)
}

/// [`floor_in_chrome`], the network slowed to `throttle` bits a second (0: full speed).
fn floor_in_chrome_throttled(
    name: &str,
    frames: u32,
    script: &str,
    saturate: bool,
    throttle: u64,
) -> (PathBuf, wrela_tests::BrowserRun, Vec<f64>, Vec<f64>) {
    let _ = floor();
    let (dir, rel) = wrela_tests::page("examples/last-green", name);
    let run = wrela_tests::ChromeRun {
        script: Some(script.into()),
        workers: 8,
        // (`WRELA_SERIAL`: each pass and dispatch timed alone, to see what a frame holds.)
        timing: if std::env::var_os("WRELA_SERIAL").is_some() {
            wrela_host::Timing::Serial
        } else {
            wrela_host::Timing::Span
        },
        saturate,
        paced: !saturate,
        nohash: true,
        throttle,
        ..wrela_tests::ChromeRun::new(frames, 1920, 1080, 60.0)
    };
    let chrome = wrela_tests::run_in_chrome_with(&rel, run);
    let spans: Vec<f64> = chrome.spans.iter().map(|s| s.1).collect();
    let frames_json = wrela_tests::result_json(&dir.join("results"), "frames.json");
    let cpu: Vec<f64> = frames_json["cpu_ms"]
        .as_array()
        .expect("cpu_ms")
        .iter()
        .map(|v| v.as_f64().expect("ms"))
        .collect();
    (dir, chrome, spans, cpu)
}

/// The frame a line starting `prefix` was printed in, if one was.
fn printed_at(chrome: &wrela_tests::BrowserRun, prefix: &str) -> Option<usize> {
    chrome.printed.iter().find(|(_, l)| l.starts_with(prefix)).map(|(f, _)| *f)
}

/// AC8: the floor's walk (P) in Chrome at 1080p, its frames back to back (#55's Method): from
/// its first full frame, no frame's GPU span over 16.7 ms, and no frame's CPU time (the frame's
/// call: its streaming, cooking and the caches moving) over 33 ms; `WALK_FRAMES` of it (the
/// whole walk with `WRELA_FULL`), three runs.
#[test]
#[ignore = "measure: needs Chrome, python3 and a GPU"]
fn the_floors_walk_keeps_its_frames_in_chrome() {
    let frames = if std::env::var("WRELA_FULL").is_ok() { 24 * 60 * 60 + 600 } else { WALK_FRAMES };
    // (`WRELA_WALK_FRAMES`: fewer, to look into a frame.)
    let frames =
        std::env::var("WRELA_WALK_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(frames);
    let mut failed = Vec::new();
    let runs = std::env::var("WRELA_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    for k in 0..runs {
        let (_, chrome, spans, cpu) = floor_in_chrome(
            &format!("last-green-walk-{k}"),
            frames,
            r#"[{"frame": 1, "type": "key", "key": "KeyP"}]"#,
            true,
        );
        let start = printed_at(&chrome, "playable").unwrap_or(0);
        let spans: Vec<f64> = spans.iter().skip(start).copied().collect();
        let cpu: Vec<f64> = cpu.iter().skip(start).copied().collect();
        let over_gpu = spans.iter().filter(|&&m| m > 16.7).count();
        let over_cpu = cpu.iter().filter(|&&m| m > 33.0).count();
        let worst_gpu = spans.iter().copied().fold(0.0, f64::max);
        let worst_cpu = cpu.iter().copied().fold(0.0, f64::max);
        eprintln!(
            "run {k}: {} frames from the first full one (frame {start}): GPU span median {:.2} ms, 99th percentile {:.2}, worst {worst_gpu:.2}, {over_gpu} over 16.7; CPU median {:.2} ms, worst {worst_cpu:.2}, {over_cpu} over 33",
            spans.len(),
            wrela_tests::median(&spans),
            wrela_tests::percentile(&spans, 0.99),
            wrela_tests::median(&cpu),
        );
        // What the frames over 16.7 ms held more of than the median frame: each label's mean
        // time in them, against its median in all.
        let mut over: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
        for (f, m) in &chrome.spans {
            if *f >= start && *m > 16.7 {
                over.insert(*f);
            }
        }
        let mut in_over: std::collections::BTreeMap<String, f64> =
            std::collections::BTreeMap::new();
        for t in &chrome.timings {
            if over.contains(&t.frame) {
                *in_over.entry(t.label.clone()).or_insert(0.0) += t.nanos / 1e6;
            }
        }
        let listed: Vec<String> = chrome
            .spans
            .iter()
            .filter(|(f, m)| over.contains(f) && *m > 16.7)
            .take(12)
            .map(|(f, m)| format!("{f} ({m:.1} ms)"))
            .collect();
        if !listed.is_empty() {
            eprintln!("  the frames over: {}", listed.join(", "));
        }
        // The worst frame's own passes, against the median frame's.
        if let Some(&(worst_frame, _)) =
            chrome.spans.iter().filter(|(f, _)| *f >= start).max_by(|a, b| a.1.total_cmp(&b.1))
        {
            let mut mine: std::collections::BTreeMap<&str, f64> = Default::default();
            for t in chrome.timings.iter().filter(|t| t.frame == worst_frame) {
                *mine.entry(t.label.as_str()).or_insert(0.0) += t.nanos / 1e6;
            }
            let mut top: Vec<(&str, f64)> = mine.into_iter().collect();
            top.sort_by(|a, b| b.1.total_cmp(&a.1));
            let parts: Vec<String> =
                top.iter().take(8).map(|(l, ms)| format!("{l} {ms:.2}")).collect();
            eprintln!("  the worst frame ({worst_frame}): {}", parts.join(", "));
            if let Ok(f) = std::env::var("WRELA_FRAME") {
                let f: usize = f.parse().expect("a frame");
                let mut each: Vec<(String, f64)> = chrome
                    .timings
                    .iter()
                    .filter(|t| t.frame == f)
                    .map(|t| (t.label.clone(), t.nanos / 1e6))
                    .collect();
                each.sort_by(|a, b| b.1.total_cmp(&a.1));
                eprintln!("  frame {f}'s passes: {each:?}");
                let mut each: Vec<(String, f64)> = chrome
                    .timings
                    .iter()
                    .filter(|t| t.frame == f + 10)
                    .map(|t| (t.label.clone(), t.nanos / 1e6))
                    .collect();
                each.sort_by(|a, b| b.1.total_cmp(&a.1));
                eprintln!("  frame {}'s passes: {each:?}", f + 10);
            }
        }
        let mut heavy: Vec<(f64, String)> =
            in_over.into_iter().map(|(l, ms)| (ms / over.len().max(1) as f64, l)).collect();
        heavy.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (ms, l) in heavy.iter().take(10) {
            eprintln!("  in the frames over: {l} {ms:.2} ms a frame on average");
        }
        if over_gpu > 0 || over_cpu > 0 {
            failed.push(format!("run {k}: {over_gpu} frames over 16.7 ms of GPU (worst {worst_gpu:.2}), {over_cpu} over 33 ms of CPU (worst {worst_cpu:.2})"));
        }
    }
    assert!(failed.is_empty(), "{failed:?}");
}

/// How many frames of the floor's walk the measure runs at its smaller size (a minute and a half).
const WALK_FRAMES: u32 = 5400;

/// The script that travels to the egg's road in the wildwood (one past the sites and the egg's
/// place), walks it (Y), and glides `glides` steps faster (H each: 2, 4, 8, 16 times a run).
fn road_script(glides: u32) -> String {
    let sites = std::fs::read_to_string(repo_root().join("examples/last-green/map.wrela"))
        .expect("map.wrela")
        .matches("sites.push")
        .count()
        + 1;
    let mut keys = vec![r#"{"frame": 1, "type": "key", "key": "KeyG"}"#.to_string()];
    for d in sites.to_string().chars() {
        keys.push(format!(r#"{{"frame": 1, "type": "key", "key": "Digit{d}"}}"#));
    }
    keys.push(r#"{"frame": 1, "type": "key", "key": "Enter"}"#.into());
    keys.push(r#"{"frame": 120, "type": "key", "key": "KeyY"}"#.into());
    for i in 0..glides {
        keys.push(format!(r#"{{"frame": {}, "type": "key", "key": "KeyH"}}"#, 121 + i));
    }
    format!("[{}]", keys.join(", "))
}

/// The tiles a run's floor printed late (`late tile ...`): a tile within the cards' reach of the
/// eye whose own trees weren't placed.
fn late_tiles(chrome: &wrela_tests::BrowserRun) -> Vec<String> {
    chrome
        .printed
        .iter()
        .filter(|(_, l)| l.starts_with("late tile"))
        .map(|(_, l)| l.clone())
        .collect()
}

/// AC7's time to play: the floor opened cold in Chrome (a new profile, nothing cached), paced
/// as in play: its first full frame (`playable`: the wood cooked, the tiles round the eye placed,
/// the near shadows drawn, the probes baked) within 5 s of opening the page, with at most 6 MB
/// downloaded before it.
#[test]
#[ignore = "measure: needs Chrome, python3 and a GPU"]
fn the_floor_is_playable_cold_within_its_budget() {
    let (dir, chrome, _, _) = floor_in_chrome("last-green-cold", 600, "[]", false);
    let load = wrela_tests::result_json(&dir.join("results"), "load.json");
    let opened = load["opened_ms"].as_f64().expect("opened_ms");
    let f = printed_at(&chrome, "playable").expect("the floor got playable");
    let at = chrome.began_ms[f];
    let bytes: f64 = load["resources"]
        .as_array()
        .expect("resources")
        .iter()
        .filter(|r| r["end_ms"].as_f64().expect("end_ms") <= at)
        .map(|r| r["bytes"].as_f64().expect("bytes"))
        .sum();
    eprintln!(
        "the floor cold: playable {:.2} s after the page opened (frame {f}), {:.2} MB downloaded before",
        (at - opened) / 1000.0,
        bytes / 1e6
    );
    assert!(at - opened <= 5000.0, "playable {:.2} s after opening (≤ 5)", (at - opened) / 1000.0);
    assert!(bytes <= 6.0e6, "{:.2} MB before the first full frame (≤ 6)", bytes / 1e6);
}

/// AC7's tiles on time: the floor's walk at a run, paced in Chrome, the network slowed to
/// 5 Mbit/s (the test server's `WRELA_THROTTLE`): from the first full frame, no tile within the
/// cards' reach of the eye without its own trees placed (`late tile`), `WALK_FRAMES` of the walk
/// (the whole of it with `WRELA_FULL`).
#[test]
#[ignore = "measure: needs Chrome, python3 and a GPU"]
fn the_floors_tiles_come_on_time_over_a_slow_network() {
    let frames = if std::env::var("WRELA_FULL").is_ok() { 24 * 60 * 60 + 600 } else { WALK_FRAMES };
    // (`WRELA_WALK_FRAMES`: fewer, to look into a frame.)
    let frames =
        std::env::var("WRELA_WALK_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(frames);
    let (_, chrome, _, _) = floor_in_chrome_throttled(
        "last-green-throttled",
        frames,
        r#"[{"frame": 1, "type": "key", "key": "KeyP"}]"#,
        false,
        5_000_000,
    );
    let late = late_tiles(&chrome);
    eprintln!("at 5 Mbit/s, the walk's tiles: {} late {late:?}", late.len());
    assert!(late.is_empty(), "tiles late at 5 Mbit/s: {late:?}");
}

/// The pace a run's player kept (m/s): the way its reports' places (`world::report`, each 2 s
/// of the sim's time) went, over that time, from the first report after `from` frames.
fn kept_pace(chrome: &wrela_tests::BrowserRun, from: usize) -> f64 {
    let places: Vec<(f64, [f64; 2])> = chrome
        .printed
        .iter()
        .filter(|(f, l)| *f >= from && l.starts_with("{\"report\""))
        .filter_map(|(_, l)| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            Some((v["report"].as_f64()?, [v["at"][0].as_f64()?, v["at"][1].as_f64()?]))
        })
        .collect();
    let way: f64 = places
        .windows(2)
        .map(|w| ((w[1].1[0] - w[0].1[0]).powi(2) + (w[1].1[1] - w[0].1[1]).powi(2)).sqrt())
        .sum();
    match (places.first(), places.last()) {
        (Some(a), Some(b)) if b.0 > a.0 => way / (b.0 - a.0),
        _ => 0.0,
    }
}

/// AC7's fastest pace (measured, for the game's gliding): along the egg's road in the wildwood,
/// a minute at each glide (1, 2, 4, 8 and 16 times a run; a glide passes over what's on the
/// ground, as the game's will): the pace each kept, the tiles late at each, and the fastest
/// with none.
#[test]
#[ignore = "measure: needs Chrome, python3 and a GPU"]
fn the_fastest_pace_the_streaming_holds() {
    let mut fastest = 0.0f64;
    for g in 0..5u32 {
        let (_, chrome, _, _) =
            floor_in_chrome(&format!("last-green-pace-{g}"), 3720, &road_script(g), false);
        let late = late_tiles(&chrome);
        let asked = 5.0 * f64::from(1u32 << g);
        // From a second after the glide's set: its pace reached.
        let pace = kept_pace(&chrome, 200);
        eprintln!("gliding at {asked} m/s: kept {pace:.1} m/s, {} tiles late {late:?}", late.len());
        assert!(pace > 0.9 * asked, "the glide at {asked} m/s kept {pace:.1} m/s");
        if late.is_empty() {
            fastest = fastest.max(pace);
        }
    }
    eprintln!("the fastest pace the streaming holds with no tile late: {fastest:.1} m/s");
    assert!(fastest >= 5.0, "tiles come late even at a run");
}

/// AC7's memory: GPU buffers and textures, and the WASM heap, after the floor's walk's first
/// minute, and after 10 km of the egg's road through the wildwood gliding at 20 m/s: within 10%
/// of the first, and within 1.5 GB a tab.
#[test]
#[ignore = "measure: needs Chrome, python3 and a GPU"]
fn the_floors_memory_stays_flat_into_the_wildwood() {
    let mib =
        |m: &serde_json::Value, k: &str, w: &str| m[k][w].as_f64().expect("bytes") / 1048576.0;
    let (first, _, _, _) = floor_in_chrome(
        "last-green-memory-minute",
        3720,
        r#"[{"frame": 1, "type": "key", "key": "KeyP"}]"#,
        false,
    );
    let a = wrela_tests::result_json(&first.join("results"), "memory.json");
    // 10 km at 20 m/s: 500 s.
    let (far, chrome, _, _) =
        floor_in_chrome("last-green-memory-far", 30_600, &road_script(2), false);
    let b = wrela_tests::result_json(&far.join("results"), "memory.json");
    let last =
        chrome.printed.iter().rev().find(|(_, l)| l.contains("\"report\"")).map(|(_, l)| l.clone());
    eprintln!(
        "memory after the walk's first minute: GPU {:.1} MiB (at most {:.1}), WASM {:.1} MiB; after 10 km of the wildwood: GPU {:.1} MiB (at most {:.1}), WASM {:.1} MiB; the last report {last:?}",
        mib(&a, "gpu", "end"),
        mib(&a, "gpu", "peak"),
        mib(&a, "wasm", "used"),
        mib(&b, "gpu", "end"),
        mib(&b, "gpu", "peak"),
        mib(&b, "wasm", "used"),
    );
    let within = |x: f64, y: f64| (x - y).abs() <= 0.1 * y;
    assert!(within(mib(&b, "gpu", "end"), mib(&a, "gpu", "end")), "GPU memory grew");
    assert!(within(mib(&b, "wasm", "used"), mib(&a, "wasm", "used")), "the WASM heap grew");
    assert!(mib(&b, "gpu", "peak") + mib(&b, "wasm", "used") <= 1536.0, "over 1.5 GB");
}

// ---- the map lens ---------------------------------------------------------------------------

/// The floor's sources copied to scratch directory `tag` (its manifest's paths made absolute),
/// with the map lens's constants' cache, so its build doesn't bake again: the copy.
fn floor_copy(tag: &str) -> PathBuf {
    let dir = scratch(&format!("last-green/{tag}"));
    let floor = repo_root().join("examples/last-green");
    for e in std::fs::read_dir(&floor).expect("the floor's files") {
        let path = e.expect("entry").path();
        if path.extension().is_some_and(|x| x == "wrela") {
            std::fs::copy(&path, dir.join(path.file_name().expect("a name"))).expect("copy");
        }
    }
    let manifest = std::fs::read_to_string(floor.join("wrela.toml")).expect("wrela.toml");
    let manifest = manifest.replace("path = \"../", &format!("path = \"{}/../", floor.display()));
    std::fs::write(dir.join("wrela.toml"), manifest).expect("write wrela.toml");
    let cache = floor.join("build/studio/map/build/consts");
    if cache.is_dir() {
        let to = dir.join("build/studio/map/build/consts");
        std::fs::create_dir_all(&to).expect("mkdir");
        for e in std::fs::read_dir(&cache).expect("the cache") {
            let path = e.expect("entry").path();
            std::fs::copy(&path, to.join(path.file_name().expect("a name"))).expect("copy");
        }
    }
    dir
}

/// A script (runtime/abi `input`) typing each command and Enter, a frame apart: the lens runs
/// each when it's done with the one before.
fn typed(commands: &[&str]) -> String {
    let mut events = Vec::new();
    for (i, c) in commands.iter().enumerate() {
        let frame = 2 + 2 * i;
        events.push(serde_json::json!({ "frame": frame, "type": "text", "text": c }));
        events.push(serde_json::json!({ "frame": frame, "type": "key", "key": "Enter" }));
    }
    Value::Array(events).to_string()
}

/// The lens's answers among lines printed.
fn lens_answers<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<Value> {
    lines
        .filter(|l| l.starts_with('{'))
        .map(|l| serde_json::from_str(l).expect("an answer is JSON"))
        .collect()
}

/// Whether two answers agree: the same keys and texts, numbers within 10⁻⁴ (relative) or 10⁻⁵;
/// the times (keys ending `ms`) and the write's hashes' file paths left out.
fn answers_agree(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
            (x - y).abs() <= 1e-5_f64.max(1e-4 * x.abs().max(y.abs()))
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| answers_agree(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            let keys = |o: &serde_json::Map<String, Value>| -> Vec<String> {
                o.keys().filter(|k| !k.ends_with("ms")).cloned().collect()
            };
            keys(x) == keys(y)
                && keys(x).iter().all(|k| answers_agree(&x[k.as_str()], &y[k.as_str()]))
        }
        _ => a == b,
    }
}

/// The session the map lens test types: the check, the items, a zoom onto the tower, a click on
/// it, a drag of it 20 px east (9 m), the write, and a click where it now stands.
const MAP_SESSION: [&str; 10] = [
    "check",
    "items",
    "zoom 560 -470 200",
    "click 448 448",
    "move 448 448 468 448",
    "write",
    "click 468 448",
    "walk 500 -420 520 -440 540 -455",
    "point 552 -462",
    "walk",
];

/// The screen the map lens's session runs at.
const MAP_SIZE: (u32, u32) = (896, 896);

/// AC13: the map lens on the floor answers a session (the check, a click, a drag and its write)
/// the same headless and in Chrome, and writes the same edit to map.wrela; a click names the
/// line of source that places the tower; the drag's literals are written back with exact
/// readback, a line changed; the map with the check shows again ≤ 5 s after the drag, in both
/// hosts.
#[test]
#[ignore = "measure: the map lens's session, needs Chrome, python3 and a GPU"]
fn the_map_lens_answers_alike_in_both_hosts_and_in_time() {
    let script = typed(&MAP_SESSION);
    let expected = MAP_SESSION.len();
    // Natively, frame by frame until every answer is in.
    let native = floor_copy("lens-native");
    let (out, page) = wrela_driver::studio::build(&native, false).expect("the map lens builds");
    assert!(
        !out.has_errors(),
        "{}",
        wrela_diag::render::render_all(&out.sources, &out.diagnostics)
    );
    let a = {
        let post = wrela_driver::studio::post_handler(&native, &page, None);
        let options = wrela_host::Options { post: Some(post), quiet: true, ..Default::default() };
        let mut host = wrela_host::Host::load_with(&page, &options).expect("the lens loads");
        let events = wrela_abi::input::parse_script(&script).expect("the script");
        let (mut lines, began) = (Vec::new(), std::time::Instant::now());
        let mut frame = 0;
        while lens_answers(lines.iter().map(String::as_str)).len() < expected {
            assert!(began.elapsed().as_secs() < 120, "the session didn't finish: {lines:?}");
            for e in wrela_abi::input::events_at(&events, frame) {
                host.push_input(e);
            }
            host.frame(frame as f32 / 60.0, MAP_SIZE.0, MAP_SIZE.1).expect("a frame");
            lines.extend(host.take_logs());
            frame += 1;
        }
        lens_answers(lines.iter().map(String::as_str))
    };
    // In Chrome, served by the studio server (which writes the edit through `wrela edit`).
    let chrome = floor_copy("lens-chrome");
    let (out, page) = wrela_driver::studio::build(&chrome, std::env::var("LENS_DEBUG").is_ok())
        .expect("the map lens builds");
    assert!(!out.has_errors());
    std::fs::write(page.join("session.json"), &script).expect("the script");
    let b = {
        let server = wrela_driver::serve::start(&chrome, &page, 0, false, None).expect("serve");
        let url = format!(
            "http://127.0.0.1:{}/#test&frames=2400&width={}&height={}&fps=60&workers=8&input=session.json&nohash=1",
            server.port, MAP_SIZE.0, MAP_SIZE.1
        );
        wrela_tests::run_url_in_chrome(&url, &page, 600);
        let log = std::fs::read_to_string(page.join("results/log.txt")).expect("log.txt");
        lens_answers(log.lines())
    };
    let actions = |s: &[Value]| -> Vec<String> {
        s.iter()
            .map(|x| {
                x["action"].as_str().or(x["answer"]["action"].as_str()).unwrap_or("?").to_string()
            })
            .collect()
    };
    eprintln!("native: {:?}\nchrome: {:?}", actions(&a), actions(&b));
    assert_eq!(actions(&a).len(), expected, "every answer natively");
    assert_eq!(actions(&a), actions(&b), "the same answers, in order");
    for (x, y) in a.iter().zip(&b) {
        assert!(answers_agree(x, y), "the hosts disagree:\n  native {x}\n  chrome {y}");
    }
    // The click names the line that places the tower; the drag writes it back, exactly.
    let tower = &a[3]["item"];
    assert_eq!(tower["name"], "the tower", "{tower}");
    let lit = &tower["literals"][0]["literal"];
    assert_eq!(lit["file"], "[last_green] map.wrela", "{lit}");
    let line = lit["line"].as_u64().expect("a line") as usize;
    let original = std::fs::read_to_string(repo_root().join("examples/last-green/map.wrela"))
        .expect("map.wrela");
    assert!(original.lines().nth(line - 1).is_some_and(|l| l.contains("560.0")), "line {line}");
    let written = &a[5]["answer"];
    assert_eq!(written["written"], true, "{written}");
    for e in written["files"][0]["edits"].as_array().expect("edits") {
        assert_eq!(e["readback"].as_f64(), e["value"].as_f64(), "exact readback: {e}");
    }
    let (na, nb) = (
        std::fs::read_to_string(native.join("map.wrela")).expect("map.wrela"),
        std::fs::read_to_string(chrome.join("map.wrela")).expect("map.wrela"),
    );
    assert_eq!(na, nb, "the hosts wrote map.wrela alike");
    let changed: Vec<(&str, &str)> =
        original.lines().zip(na.lines()).filter(|(x, y)| x != y).collect();
    assert_eq!(changed.len(), 1, "a minimal diff: one line changed: {changed:?}");
    // A walk drawn: three points, a fourth, then the next walk begun.
    let walked = |i: usize| (a[i]["walks"].as_f64(), a[i]["points"].as_f64());
    assert_eq!(walked(7), (Some(1.0), Some(3.0)), "{}", a[7]);
    assert_eq!(walked(8), (Some(1.0), Some(4.0)), "{}", a[8]);
    assert_eq!(walked(9), (Some(2.0), Some(0.0)), "{}", a[9]);
    // The time from the drag to the map with the check shown again (the preview's history and
    // its check, on the lens's helpers).
    for (host, s) in [("native", &a), ("chrome", &b)] {
        let total = f(&s[4]["ms"]);
        eprintln!("{host}: the map with the check again {total:.0} ms after the drag (≤ 5000)");
        assert!(total <= 5000.0, "{host}: the map showed again {total:.0} ms after the drag");
    }
}

/// AC2: the preview, the floor's history at the preview's grain (4 m cells, 10-year steps, over
/// the history's domain), runs in ≤ 3 s on the native host, all cores; the check the map lens
/// runs on it (`interest::QUICK`) is reported beside it. The median of three.
#[test]
#[ignore = "measure: the preview's history, three times on all cores"]
fn the_preview_runs_in_three_seconds() {
    let main = r#"
use engine::footing::obstacles_of
use engine::grow::{PREVIEW, simulate}
use engine::interest::{Evidence, QUICK, check}
use std::io::print
use std::time::now

pub fn frame(time: f32, width: u32, height: u32) {
    let m = map::map()
    let lo = map::LO - vec2(64.0)
    let hi = map::HI + vec2(64.0)
    let st = stones::stones(m)
    let began = now()
    let h = simulate(ground::ground(), m, species::habits(), PREVIEW, lo, hi, st)
    let grown = now()
    let o = obstacles_of(h.trees, species::habits(), h.streams, st, stones::walls(m), lo, hi)
    var e = Evidence::of(h)
    e.crowns = trees::drawn_crowns(e.trees)
    e.solids = blockouts::solids(m)
    let c = check(ground::ground(), m, species::habits(), e, o, st, QUICK)
    print(f"timed {grown - began} {now() - grown} {c.coverage.dead}")
}
"#;
    let dir = map_package("last-green/preview-time", main);
    let floor = repo_root().join("examples/last-green");
    for f in ["trees.wrela", "blockouts.wrela"] {
        std::fs::copy(floor.join(f), dir.join(f)).expect("copy");
    }
    let manifest = std::fs::read_to_string(dir.join("wrela.toml")).expect("the manifest");
    let great = repo_root().join("examples/great-tree").display().to_string();
    std::fs::write(
        dir.join("wrela.toml"),
        format!("{manifest}great_tree = {{ path = {great:?} }}\n"),
    )
    .expect("write wrela.toml");
    let build = scratch("last-green/preview-time-build");
    wrela_tests::must_build(&dir, &build);
    let mut runs = Vec::new();
    for _ in 0..3 {
        let mut host = wrela_host::Host::load(&build).expect("load");
        host.frame(0.0, 64, 64).expect("the frame");
        let line = host.take_logs().into_iter().find(|l| l.starts_with("timed")).expect("timed");
        let n: Vec<f64> = line.split(' ').skip(1).map(|x| x.parse().expect("a number")).collect();
        runs.push((n[0], n[1], n[2]));
    }
    runs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (history, check, dead) = runs[1];
    eprintln!(
        "the preview: the history {history:.2} s (≤ 3 s; runs {:?}), the lens's check on it {check:.2} s, dead {:.1}%",
        runs.iter().map(|r| (r.0 * 100.0).round() / 100.0).collect::<Vec<_>>(),
        100.0 * dead
    );
    assert!(history <= 3.0, "the preview's history took {history:.2} s");
}

/// AC13's provenance in the running floor: a click on a tree names its species and the line that
/// says how it lives, the kind it's drawn as and the line that builds it, and the history's
/// record of it (born, how it died, what shaped it); a click on the ground names its region, its
/// cover and the history's fields there. In Chrome within 50 ms (the click's frame, CPU), and the
/// same answers natively. From the gate at 800×600: the great oak, then the meadow.
#[test]
#[ignore = "long: bakes the floor, needs Chrome, python3 and a GPU"]
fn a_click_names_a_trees_kind_and_history_and_the_grounds_cover() {
    let _ = floor();
    let (dir, rel) = wrela_tests::page("examples/last-green", "last-green-provenance");
    let text = r#"[
        {"frame": 150, "type": "down", "x": 203, "y": 285, "button": "primary"},
        {"frame": 151, "type": "up", "x": 203, "y": 285, "button": "primary"},
        {"frame": 170, "type": "down", "x": 250, "y": 450, "button": "primary"},
        {"frame": 171, "type": "up", "x": 250, "y": 450, "button": "primary"}
    ]"#;
    let frames = 200;
    let run = wrela_tests::ChromeRun {
        script: Some(text.into()),
        workers: 8,
        nohash: true,
        ..wrela_tests::ChromeRun::new(frames, 800, 600, 60.0)
    };
    let chrome = wrela_tests::run_in_chrome_with(&rel, run);
    let answers = |lines: Vec<&str>| -> Vec<Value> {
        lines
            .into_iter()
            .filter(|l| l.starts_with("{\"provenance\""))
            .map(|l| serde_json::from_str(l).expect("JSON"))
            .collect()
    };
    let in_chrome = answers(chrome.printed.iter().map(|(_, l)| l.as_str()).collect());
    let script = wrela_host::parse_script(text).expect("a script");
    let mut native = wrela_host::Host::load(&dir).expect("load the floor");
    for i in 0..frames {
        native.lockstep_frame(i, 60.0, 800, 600, &script).unwrap_or_else(|e| panic!("{i}: {e}"));
    }
    let natively = answers(native.take_logs().iter().map(String::as_str).collect());
    drop(native);
    eprintln!("chrome: {in_chrome:?}\nnative: {natively:?}");
    assert_eq!(in_chrome.len(), 2, "two answers in Chrome");
    for (a, b) in in_chrome.iter().zip(&natively) {
        assert!(answers_agree(a, b), "the hosts disagree:\n  {a}\n  {b}");
    }
    let tree = &in_chrome[0];
    assert_eq!(tree["provenance"], "tree", "{tree}");
    assert_eq!(tree["species"], "oak");
    assert_eq!(tree["drawn_as"], "GREAT");
    assert_eq!(tree["shaped"]["name"], "the great oak");
    let trees = std::fs::read_to_string(repo_root().join("examples/last-green/trees.wrela"))
        .expect("trees.wrela");
    let line = trees.lines().position(|l| l.contains("k == GREAT {")).expect("the kind") + 1;
    assert_eq!(tree["kind"], format!("trees.wrela:{line}"), "the kind's line");
    let species = std::fs::read_to_string(repo_root().join("examples/last-green/species.wrela"))
        .expect("species.wrela");
    let line = species.lines().position(|l| l.contains("// Oak:")).expect("the habit") + 1;
    assert_eq!(tree["habit"], format!("species.wrela:{line}"), "the habit's line");
    assert!(tree["born"].as_i64().is_some() && tree["state"] == "alive", "{tree}");
    let ground = &in_chrome[1];
    assert_eq!(ground["provenance"], "ground", "{ground}");
    assert_eq!(ground["region"], "the wood pasture");
    assert!(ground["cover"].as_str().is_some_and(|c| !c.is_empty()), "{ground}");
    assert!(ground["fields"]["light"].as_f64().is_some(), "the history's fields: {ground}");
    // The clicks' frames in Chrome: each pointer-up's frame, its CPU time.
    let frames_json = wrela_tests::result_json(&dir.join("results"), "frames.json");
    let cpu = frames_json["cpu_ms"].as_array().expect("cpu_ms");
    for f in [151, 171] {
        let ms = cpu[f].as_f64().expect("ms");
        eprintln!("the click at frame {f}: {ms:.1} ms of CPU");
        assert!(ms <= 50.0, "the click's frame took {ms:.1} ms");
    }
}

/// AC4's camera-relative rendering: the floor drawn from one camera (held in the world, by the
/// mere looking at the ring: trees, grass, stones, water, the ground), once with the render
/// origin at the world's and once six tiles east and twelve north of it, the scene moved in
/// render space by whole tiles, gives the same frame: a mean within 0.5/255, and no more than
/// 0.5% of the pixels off by more than 8/255 (edges, where a position rounds the other way by a
/// tenth of a millimetre). Both runs draw the same frames at the same times.
#[test]
#[ignore = "long: bakes the floor, needs a GPU"]
fn a_scene_moved_by_whole_tiles_draws_the_same_frame() {
    let eye_at = |f: &mut Floor| {
        let eye = f.eye([232.0, -452.0]);
        let at = [40.0, f.ground([40.0, -455.0]) + 1.5, -455.0];
        (eye, at)
    };
    let out = scratch("last-green/origin");
    let mut a = Floor::load_sized("last-green-origin-a", (480, 270));
    let (eye, at) = eye_at(&mut a);
    a.look(eye, at, 180);
    let frames = a.frame;
    let first = a.host.read_screen().expect("the screen");
    let cam_a = a.camera();
    a.save(&out.join("origin-0.png"));
    drop(a);
    let mut b = Floor::load_sized("last-green-origin-b", (480, 270));
    b.call("test_origin", &[wrela_host::Value::I32(6), wrela_host::Value::I32(-12)]);
    b.hold(eye, at);
    b.steps(frames);
    assert!(b.call("test_settled", &[])[0] > 0.5, "the second run settled by frame {frames}");
    let second = b.host.read_screen().expect("the screen");
    let cam_b = b.camera();
    b.save(&out.join("origin-6-12.png"));
    let d = wrela_host::image::compare(&first, &second).expect("the same size");
    let off = first
        .chunks_exact(4)
        .zip(second.chunks_exact(4))
        .filter(|(p, q)| (0..3).any(|c| p[c].abs_diff(q[c]) > 8))
        .count();
    let share = 100.0 * off as f64 / (first.len() / 4) as f64;
    eprintln!(
        "the same camera, the origin moved by whole tiles: mean {:.3}/255, {share:.2}% of pixels off by more than 8/255, most {}; the eyes {:?} and {:?}",
        d.mean, d.max, cam_a.eye, cam_b.eye
    );
    for k in 0..3 {
        assert!((cam_a.eye[k] - cam_b.eye[k]).abs() < 1.0e-3, "the camera's eye in the world");
    }
    assert!(d.mean <= 0.5, "the frames differ by a mean of {}/255", d.mean);
    assert!(share <= 0.5, "{share:.2}% of the pixels differ by more than 8/255");
}

/// AC11's sun shafts (Q6, a candidate): their pass's GPU time at 1080p output (the scene at
/// 960×540, the shafts at a quarter of its pixels), each pass alone (serial), ≤ 0.5 ms; and the
/// frames with and without them, from inside the wood pasture looking toward the sun, saved for
/// the owner's blind comparison (`target/tmp/last-green/shafts`), with how much they brighten.
#[test]
#[ignore = "measure: the shafts' pass in serial timing, needs a GPU"]
fn the_sun_shafts_cost_half_a_millisecond_or_less() {
    let options = wrela_host::Options {
        timing: wrela_host::Timing::Serial,
        ..wrela_host::Options::default()
    };
    let mut f = Floor::load_with("last-green-shafts", (1920, 1080), &options);
    let out = scratch("last-green/shafts");
    let eye = f.eye([-12.0, -470.0]);
    let at = [eye[0] + 20.0, eye[1] + 5.0, eye[2] - 3.0];
    f.look(eye, at, 120);
    let off = f.host.read_screen().expect("the screen");
    f.save(&out.join("without.png"));
    f.call("test_shafts", &[wrela_host::Value::I32(1)]);
    f.steps(64);
    let _ = f.host.take_timings();
    let from = f.frame as usize;
    f.steps(60);
    let timings = f.host.take_timings().expect("timings");
    let mut per: std::collections::BTreeMap<usize, f64> =
        (from..f.frame as usize).map(|k| (k, 0.0)).collect();
    for t in &timings {
        if t.label == "shafts"
            && let Some(ms) = per.get_mut(&t.frame)
        {
            *ms += t.nanos / 1e6;
        }
    }
    let ms = wrela_tests::median(&per.into_values().collect::<Vec<_>>());
    let on = f.host.read_screen().expect("the screen");
    f.save(&out.join("with.png"));
    let luma = |rgba: &[u8]| -> f64 {
        rgba.chunks_exact(4)
            .map(|p| 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]))
            .sum::<f64>()
            / (rgba.len() / 4) as f64
    };
    eprintln!(
        "sun shafts: {ms:.3} ms a frame (≤ 0.5); the frame's mean display luminance {:.1} without, {:.1} with",
        luma(&off),
        luma(&on)
    );
    assert!(ms <= 0.5, "the shafts took {ms:.3} ms");
}
