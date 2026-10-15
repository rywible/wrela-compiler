//! The floor's map and small growth fixtures. Full-scene bakes, tours and timing claims are
//! historical experiments; cache, shipping, rendering and browser behavior use small fixtures.

use crate::scratch;
use std::path::{Path, PathBuf};
use wrela_tests::repo_root;

/// The floor's files that make its map: copied into a package with `main` as its program.
const MAP_FILES: [&str; 4] = ["map.wrela", "ground.wrela", "species.wrela", "stones.wrela"];

/// A scratch package `name` holding the floor's map files and `main` as its `main.wrela`,
/// depending on the engine.
fn map_package(name: &str, main: &str) -> PathBuf {
    let dir = scratch(name);
    let floor = repo_root().join("games/last-green");
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

/// Map modules can carry their own focused regressions. They are copied into each scratch
/// package and must run too; the fixture's own expected count remains separate below.
fn copied_map_test_count(dir: &Path) -> usize {
    MAP_FILES.iter().map(|file| declared_tests(&dir.join(file))).sum()
}

fn declared_tests(file: &Path) -> usize {
    std::fs::read_to_string(file)
        .expect("read copied fixture module")
        .lines()
        .filter(|line| line.trim() == "@test")
        .count()
}

/// Terrain, trees and masonry use bounded fields and small seeded fixtures. Copy only their
/// modules, so discovering their tests cannot prepare the full game's history constants.
#[test]
fn the_environments_bounded_art_and_collision_regressions_pass() {
    let dir = map_package(
        "last-green/environment-art",
        "pub fn frame(time: f32, width: u32, height: u32) {}\n",
    );
    let floor = repo_root().join("games/last-green");
    let extra = ["trees.wrela", "blockouts.wrela"];
    for file in extra {
        std::fs::copy(floor.join(file), dir.join(file)).expect("copy environment fixture module");
    }
    let manifest = dir.join("wrela.toml");
    let mut text = std::fs::read_to_string(&manifest).expect("read map fixture manifest");
    let great_tree = repo_root().join("examples/great-tree").display().to_string();
    text.push_str(&format!("great_tree = {{ path = {great_tree:?} }}\n"));
    std::fs::write(manifest, text).expect("add great tree dependency");
    let declared = copied_map_test_count(&dir)
        + extra.iter().map(|file| declared_tests(&dir.join(file))).sum::<usize>();
    assert_eq!(super::tests_pass(&dir), declared);
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
    assert_eq!(super::tests_pass(&dir), 3 + copied_map_test_count(&dir));
}

/// A package that grows the floor's map changed by `edit` (its `map()` bound to `m`, mutable),
/// at the preview's grain, in a constant.
fn grown_from(name: &str, edit: &str) -> PathBuf {
    let main = format!(
        r#"
use engine::grow::{{History, PREVIEW, Start, simulate}}

const GROWN: u32 = grown()

fn grown() -> u32 {{
    var m = map::map()
    {edit}
    let h = simulate(ground::ground(), m, species::habits(), PREVIEW, map::LO, map::HI, stones::stones(m), |land, year| Start::wild(land))
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

/// The elves (peoples.wrela) on a small land of good soil by a river: the two tribes come to the
/// best places, far enough apart; each settlement tills the soil nearest it, grazes beyond, and
/// leaves the land as it last used it, with old trees standing only in the pasture, about one in
/// each `veteran` of it; land they left long before the plague has gone back to the wood.
#[test]
fn the_elves_settle_the_best_land_and_leave_it_as_they_used_it() {
    let tests = r#"
use engine::grow::{Grid, Land, Left, NEVER, NONE, UNBROWSED}
use engine::soil::soil
use peoples::{Culture, RECLAIMED, Use, left_by, settle}

/// A level land 1200 m by 800 m of deep loam, a river along its south edge.
fn land() -> Land {
    let grid = Grid::over(vec2(0.0), vec2(1200.0, 800.0), 4.0)
    let nn = grid.cells()
    var water: Vec<bool> = Vec::new()
    for c in 0..nn {
        water.push(grid.centre(c).y < 12.0)
    }
    Land {
        grid,
        heights: Vec::filled(nn, 0.0),
        habitable: Vec::filled(nn, value: true),
        wet: Vec::filled(nn, 0.3),
        thin: Vec::filled(nn, 0.0),
        soil: Vec::filled(nn, soil(lap: 0.002, slope: 0.01, rock: 0.0, wet: 0.3)),
        exposed: Vec::filled(nn, 0.0),
        cap: Vec::filled(nn, 1.0e9),
        capped_by: Vec::filled(nn, NONE),
        lawn: Vec::filled(nn, value: false),
        kept: Vec::filled(nn, NONE),
        opened: Vec::filled(nn, NEVER),
        browsed: Vec::filled(nn, UNBROWSED),
        water,
        region: Vec::filled(nn, 0),
    }
}

const ELVES = Culture { apart: 250.0, tribes_apart: 500.0, field_reach: 300.0, pasture_reach: 450.0, reach: 600.0 }

@test
fn two_tribes_far_apart() {
    let p = settle(land(), ELVES, 0, 300)
    var tribes = [0, 0]
    for s in p.settlements {
        tribes[s.tribe] += 1
    }
    assert(tribes[0] >= 1 && tribes[1] >= 1, f"both tribes settle: {tribes[0]} and {tribes[1]}")
    let a = p.grid.centre(p.settlements[0].at)
    let b = p.grid.centre(p.settlements[1].at)
    assert(length(a - b) >= 500.0, f"the tribes' first settlements stand apart: {length(a - b)} m")
    assert(a.y < 260.0 && b.y < 260.0, f"near the river: {a.y} and {b.y} m from it")
}

@test
fn fields_near_and_pasture_beyond() {
    let p = settle(land(), ELVES, 0, 300)
    var near = [0.0, 0.0]
    var count = [0.0, 0.0]
    for c in 0..p.grid.cells() {
        let u = Use::from_u32(u32(p.last[c])).unwrap()
        let k = if u == Use::Field { 0 } else if u == Use::Pasture { 1 } else { 2 }
        if k == 2 {
            continue
        }
        var d = 1.0e9
        for s in p.settlements {
            d = min(d, length(p.grid.centre(c) - p.grid.centre(s.at)))
        }
        near[k] += d
        count[k] += 1.0
    }
    assert(count[0] > 0.0 && count[1] > 0.0, f"fields {count[0]} and pasture {count[1]}")
    assert(near[0] / count[0] < near[1] / count[1], f"fields {near[0] / count[0]} m from home, pasture {near[1] / count[1]} m")
}

@test
fn left_as_last_used_with_old_trees_in_the_pasture() {
    let l = land()
    let p = settle(l, ELVES, 0, 300)
    let s = left_by(p, l, species::habits(), ELVES, 300, 0)
    var grazed = 0.0
    for c in 0..l.grid.cells() {
        let was = Left::from_u32(u32(s.left[c])).unwrap()
        let pc = p.grid.cell_at(l.grid.centre(c))
        let used = Use::from_u32(u32(p.last[pc])).unwrap()
        let want = if p.last_year[pc] < 300 - RECLAIMED { Left::Wild } else if used == Use::Field { Left::Tilled } else if used == Use::Pasture { Left::Grazed } else if used == Use::Wood { Left::Cut } else if used == Use::Settled { Left::Built } else { Left::Wild }
        assert(was == want, "each cell left as it was last used, if in the years before")
        if was == Left::Grazed {
            grazed += l.grid.cell * l.grid.cell
        }
    }
    let want = grazed / ELVES.veteran
    let n = f32(s.standing.len())
    assert(n > 0.7 * want && n < 1.3 * want, f"about one old tree to each {ELVES.veteran} m² of pasture: {n} for {want}")
    let habits = species::habits()
    for t in s.standing {
        let full = habits[t.kind].crown * t.tall
        assert(t.reach > 0.99 * full, f"open-grown: crown {t.reach} m, its kind's {full} m")
    }
}

@test
fn left_long_before_the_plague_is_wood_again() {
    let l = land()
    let p = settle(l, ELVES, 0, 200)
    let s = left_by(p, l, species::habits(), ELVES, 400, 0)
    for b in s.left {
        assert(Left::from_u32(u32(b)).unwrap() == Left::Wild, "the wood has it back")
    }
    assert(s.standing.len() == 0, "no tree kept")
}
"#;
    let dir = map_package("last-green/elves", tests);
    let floor = repo_root().join("games/last-green");
    std::fs::copy(floor.join("peoples.wrela"), dir.join("peoples.wrela")).expect("copy the elves");
    assert_eq!(super::tests_pass(&dir), 4 + copied_map_test_count(&dir));
}

/// A world's seed (`Map::seed`): the same map grown with another seed is another world, and with
/// the same seed, the same world. On a corner of the floor at the preview's grain.
#[test]
fn a_seed_grows_another_world_and_the_same_seed_the_same() {
    let tests = r#"
use engine::grow::{PREVIEW, Start, Tree, simulate}

fn grown(seed: u32) -> Vec<Tree> {
    var m = map::map()
    m.seed = seed
    let lo = vec2(500.0, -1100.0)
    var h = simulate(ground::ground(), m, species::habits(), PREVIEW, lo, lo + vec2(160.0), stones::stones(m), |land, year| Start::wild(land))
    take h.trees
}

fn same(a: [Tree], b: [Tree]) -> bool {
    if a.len() != b.len() {
        return false
    }
    for i in 0..a.len() {
        if length(a[i].at - b[i].at) > 0.0 || a[i].tall != b[i].tall {
            return false
        }
    }
    true
}

@test
fn seeds() {
    let a = grown(0)
    assert(same(a, grown(0)), "the same seed grows the same world")
    assert(!same(a, grown(7)), "another seed grows another world")
}
"#;
    let dir = map_package("last-green/seeds", tests);
    assert_eq!(super::tests_pass(&dir), 1 + copied_map_test_count(&dir));
}
