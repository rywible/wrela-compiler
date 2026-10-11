//! The stair and its doors (examples/stair, examples/test-floor, engine::tower), in the native
//! host: the builds side by side, as a game ships them. The player climbs to the second door,
//! which opens once the first world is decided, walks through to the test floor, frees it, and
//! walks back out; the freed world's door is gone, from then on and over a reload.

use std::path::PathBuf;
use std::time::{Duration, Instant};
use wrela_host::{Host, Value};

const SIZE: u32 = 64;

/// The stair's build, with the test floor's beside it, under `target/tmp/<name>/`: fresh, with
/// no save.
fn tower(name: &str) -> PathBuf {
    wrela_tests::page("examples/test-floor", &format!("{name}/test-floor"));
    wrela_tests::page("examples/stair", &format!("{name}/stair")).0
}

fn frames(host: &mut Host, n: u32) {
    for _ in 0..n {
        host.frame(0.0, SIZE, SIZE).expect("a frame");
    }
}

fn report(host: &mut Host) -> [f32; 4] {
    let v = host.call_export("report", &[]).expect("report");
    let f = |i: usize| match v[i] {
        Value::F32(x) => x,
        ref other => panic!("{other:?}"),
    };
    [f(0), f(1), f(2), f(3)]
}

fn running(host: &Host) -> String {
    host.build_dir().file_name().expect("a name").to_string_lossy().into_owned()
}

/// Frames until `done` holds of the report, for at most a minute.
fn frames_until(host: &mut Host, done: impl Fn([f32; 4]) -> bool) {
    let end = Instant::now() + Duration::from_secs(60);
    while !done(report(host)) && Instant::now() < end {
        frames(host, 1);
    }
}

/// The stair's report's doors: the first door's state times 4, plus the second's (0 shut, 1
/// open, 2 gone).
const SHUT: f32 = 4.0;
const BOTH_OPEN: f32 = 5.0;
const SECOND_GONE: f32 = 6.0;

/// Up to the second door, through it, the floor freed, and back out.
fn walk_through_and_back(host: &mut Host) {
    frames(host, 2);
    assert_eq!(report(host)[2], SHUT, "a new tower: the first door opens, the second doesn't");
    host.call_export("climb", &[Value::F32(32.0)]).expect("climb");
    host.call_export("go_in", &[]).expect("go_in");
    frames(host, 2);
    assert_eq!(running(host), "stair", "the second door is shut");
    host.call_export("test_decide", &[Value::I32(0), Value::I32(1)]).expect("decide");
    frames(host, 1);
    assert_eq!(report(host)[2], BOTH_OPEN, "the first world preserved: the second door opens");
    frames_until(host, |r| r[3] == 1.0);
    assert_eq!(report(host)[3], 1.0, "the test floor's build is ready");
    host.call_export("go_in", &[]).expect("go_in");
    frames(host, 2);
    assert_eq!(running(host), "test-floor");
    let r = report(host);
    assert_eq!([r[2], r[3]], [1.0, 0.0], "arrived, the floor undecided");
    host.call_export("test_decide", &[Value::I32(2)]).expect("decide");
    host.call_export("go_out", &[]).expect("go_out");
    frames(host, 2);
    assert_eq!(running(host), "stair");
    let r = report(host);
    assert_eq!(r[..3], [32.0, 1.0, SECOND_GONE], "back at the second door, which is gone");
    host.call_export("go_in", &[]).expect("go_in");
    frames(host, 2);
    assert_eq!(running(host), "stair", "a freed world can't be gone back to");
}

#[test]
#[ignore = "needs a GPU"]
fn the_player_walks_through_the_second_door_and_back() {
    let mut host = Host::load(tower("tower-walk")).expect("load");
    walk_through_and_back(&mut host);
}

#[test]
#[ignore = "needs a GPU"]
fn the_tower_is_saved_over_a_reload() {
    let dir = tower("tower-reload");
    {
        let mut host = Host::load(&dir).expect("load");
        walk_through_and_back(&mut host);
        // The save is written in the background.
        frames(&mut host, 4);
    }
    let mut host = Host::load(&dir).expect("load again");
    frames_until(&mut host, |r| r[2] != SHUT);
    let r = report(&mut host);
    assert_eq!([r[0], r[2]], [32.0, SECOND_GONE], "at the last door, the world freed");
}
/// The first floor's real gate/door declarations and exit operation, on the small test floor.
/// This exercises the stair's first landing and round trip without baking the floor's world.
#[test]
#[ignore = "needs the runtime"]
fn the_first_floor_door_returns_to_its_stair_landing() {
    let root = wrela_tests::repo_root();
    let floor = std::fs::read_to_string(root.join("examples/last-green/main.wrela")).unwrap();
    let world = std::fs::read_to_string(root.join("examples/last-green/world.wrela")).unwrap();
    let gate = world.lines().find(|l| l.starts_with("pub const GATE =")).unwrap();
    let door = floor.lines().find(|l| l.starts_with("const DOOR =")).unwrap();
    let leave = floor.split("pub fn test_leave(").nth(1).unwrap().split_once("\n}").unwrap().0;
    let source = std::fs::read_to_string(root.join("examples/test-floor/main.wrela"))
        .unwrap()
        .replace("const WORLD: u32 = 1", "const WORLD: u32 = 0")
        .replace("const DOOR = Doorway { at: vec2(0.0, 0.6), reach: 1.2 }", door);
    let source = format!("{gate}\n{source}\n@testing\npub fn test_leave({leave}\n}}\n");
    let package = super::engine_package("first-floor-source", "first_floor", &source);
    let output = wrela_tests::page_scratch_root().join("first-floor-tour/last-green");
    wrela_tests::must_build(&package, &output);
    let dir = wrela_tests::page("examples/stair", "first-floor-tour/stair").0;
    let mut host = Host::load(dir).expect("load");
    frames(&mut host, 2);
    host.call_export("climb", &[Value::F32(16.0)]).expect("climb");
    host.call_export("go_in", &[]).expect("go_in");
    frames(&mut host, 2);
    assert_eq!(running(&host), "last-green");
    let leave = |host: &mut Host, x: f32, z: f32| match host
        .call_export("test_leave", &[Value::F32(x), Value::F32(z)])
        .expect("test_leave")[0]
    {
        Value::F32(v) => v,
        ref other => panic!("{other:?}"),
    };
    // The gate is at (-1.5, -2.0); the door 5 m behind it.
    assert_eq!(leave(&mut host, -1.5, -2.0), 0.0, "the gate isn't the door");
    assert_eq!(leave(&mut host, -1.5, -7.0), 1.0, "the door goes back down");
    frames(&mut host, 2);
    assert_eq!(running(&host), "stair");
    let r = report(&mut host);
    assert_eq!(r[..2], [16.0, 1.0], "arrived back at the first door");
}
