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

/// The door's time, from the frame the player goes through to the next build's first frame
/// drawn: the stair's last frame, the hand-over and the new build's first frame. `ready`: the
/// build was got ready on the stair first, as the stair does while the player climbs; else the
/// hand-over gets it ready itself (the player went through before it was).
fn door_time(stair: &std::path::Path, world: u32, ready: bool) -> Duration {
    // A new tower each time: no save from the last.
    let _ = std::fs::remove_dir_all(stair.join("storage"));
    let mut host = Host::load(stair).expect("load");
    if world == 1 {
        host.call_export("test_decide", &[Value::I32(0), Value::I32(1)]).expect("decide");
    }
    host.call_export("climb", &[Value::F32(16.0 * (world + 1) as f32)]).expect("climb");
    if ready {
        let end = Instant::now() + Duration::from_secs(600);
        while report(&mut host)[3] != 1.0 && Instant::now() < end {
            frames(&mut host, 1);
        }
        assert_eq!(report(&mut host)[3], 1.0, "the build got ready");
    }
    host.call_export("go_in", &[]).expect("go_in");
    let start = Instant::now();
    frames(&mut host, 2);
    let took = start.elapsed();
    assert_ne!(running(&host), "stair", "the hand-over was made");
    took
}

/// The door's time to each world on the native host, got ready first and not: the Last Green
/// (behind the first door) and the test floor (behind the second). Printed, not gated.
#[test]
#[ignore = "measure: needs a GPU, and builds the Last Green (its bake, when its cache is cold)"]
fn the_doors_time_with_and_without_getting_ready() {
    let name = "tower-door";
    wrela_tests::page("examples/last-green", &format!("{name}/last-green"));
    let stair = tower(name);
    for (world, what) in [(1, "the test floor"), (0, "the Last Green")] {
        for ready in [true, false] {
            let took = door_time(&stair, world, ready);
            let how = if ready { "got ready first" } else { "not got ready" };
            println!("door to {what}, {how}: {:.0} ms", took.as_secs_f64() * 1000.0);
        }
    }
}
