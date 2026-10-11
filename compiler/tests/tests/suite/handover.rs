//! One build hands its place to another (`std::reload::prepare`, `unprepare` and `hand_over`),
//! in the native host: compiler/tests/handover's three packages, built side by side. `first`
//! (red) gets `second` (red, its right half green, with a pipeline of its own) ready and hands
//! over to it; `second` hands back; a hand-over to a build that isn't there fails and `first`
//! goes on; one to `broken`, whose `init` panics, gives the place back to `first`. `tour` does
//! all of that by itself, in both hosts.

use std::path::PathBuf;
use wrela_host::{Host, Value};

const SIZE: u32 = 64;

/// The three builds, side by side; `first`'s directory.
fn builds() -> PathBuf {
    for p in ["second", "broken", "tour"] {
        super::built(&format!("handover/{p}"));
    }
    super::built("handover/first")
}

fn frames(host: &mut Host, n: u32) {
    for i in 0..n {
        host.frame(i as f32 / 60.0, SIZE, SIZE).unwrap_or_else(|e| panic!("frame {i}: {e}"));
    }
}

/// Frames until `first` hears how getting a build ready went: it compiles and builds on a thread
/// meanwhile, so a busy machine takes more frames (bounded by time, not frames).
fn frames_until_ready(host: &mut Host) {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while report(host)[2] == 0.0 && std::time::Instant::now() < end {
        frames(host, 1);
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

/// The screen's pixel at (x, y), RGB.
fn pixel(host: &mut Host, x: u32, y: u32) -> [u8; 3] {
    let s = host.read_screen().expect("the screen");
    let i = ((y * SIZE + x) * 4) as usize;
    [s[i], s[i + 1], s[i + 2]]
}

fn running(host: &Host) -> String {
    host.build_dir().file_name().expect("a name").to_string_lossy().into_owned()
}

#[test]
#[ignore = "needs a GPU"]
fn a_build_hands_over_to_one_it_got_ready_and_back() {
    let mut host = Host::load(builds()).expect("load");
    frames(&mut host, 2);
    host.call_export("get_ready", &[Value::I32(1)]).expect("get_ready");
    // Ready in the background: frames go on meanwhile.
    frames_until_ready(&mut host);
    assert_eq!(report(&mut host)[2], 1.0, "`second` got ready");
    let built = host.pipelines_built();
    host.call_export("go", &[Value::I32(1)]).expect("go");
    frames(&mut host, 1);
    assert_eq!(running(&host), "handover-second");
    assert_eq!(host.pipelines_built(), built, "the hand-over built no pipeline: all were ready");
    frames(&mut host, 1);
    assert_eq!(report(&mut host)[..3], [2.0, 1.0, 2.0], "starts, restored, last");
    assert_eq!(pixel(&mut host, 8, 32), [255, 0, 0], "the kept pipeline");
    assert_eq!(pixel(&mut host, 56, 32), [0, 255, 0], "`second`'s own");
    host.call_export("back", &[]).expect("back");
    frames(&mut host, 2);
    assert_eq!(running(&host), "handover-first");
    assert_eq!(report(&mut host)[..2], [3.0, 1.0], "three starts, the last given what was kept");
    assert_eq!(host.pipelines_built(), built, "going back built none: the last build's are kept");
    assert_eq!(pixel(&mut host, 56, 32), [255, 0, 0]);
}

#[test]
#[ignore = "needs a GPU"]
fn a_hand_over_to_a_build_not_got_ready_gets_it_ready_first() {
    let mut host = Host::load(builds()).expect("load");
    frames(&mut host, 1);
    let built = host.pipelines_built();
    host.call_export("go", &[Value::I32(1)]).expect("go");
    frames(&mut host, 2);
    assert_eq!(running(&host), "handover-second");
    assert_eq!(host.pipelines_built(), built + 1, "only `second`'s own pipeline was built");
    assert_eq!(pixel(&mut host, 56, 32), [0, 255, 0]);
}

#[test]
#[ignore = "needs a GPU"]
fn a_hand_over_to_a_missing_build_fails_and_the_program_goes_on() {
    let mut host = Host::load(builds()).expect("load");
    frames(&mut host, 1);
    host.call_export("get_ready", &[Value::I32(3)]).expect("get_ready");
    host.call_export("go", &[Value::I32(3)]).expect("go");
    frames(&mut host, 2);
    assert_eq!(running(&host), "handover-first");
    let r = report(&mut host);
    assert_eq!(r[2], 2.0, "getting it ready failed");
    assert_eq!(r[3], 1.0, "the hand-over failed");
    assert_eq!(r[0], 1.0, "the same program goes on");
    assert_eq!(pixel(&mut host, 32, 32), [255, 0, 0], "and draws");
}

#[test]
#[ignore = "needs a GPU"]
fn a_build_whose_init_fails_gives_the_place_back() {
    let mut host = Host::load(builds()).expect("load");
    frames(&mut host, 1);
    host.call_export("go", &[Value::I32(2)]).expect("go");
    frames(&mut host, 1);
    assert_eq!(running(&host), "handover-first", "`first` again");
    let logs = host.take_logs().join("\n");
    assert!(logs.contains("the hand-over to") && logs.contains("handover-broken"), "{logs}");
    frames(&mut host, 1);
    assert_eq!(report(&mut host)[..2], [2.0, 1.0], "started again with what it had kept");
    assert_eq!(pixel(&mut host, 32, 32), [255, 0, 0], "the screen isn't black");
}

#[test]
#[ignore = "needs a GPU"]
fn letting_a_build_go_drops_its_pipelines() {
    let mut host = Host::load(builds()).expect("load");
    frames(&mut host, 1);
    host.call_export("get_ready", &[Value::I32(1)]).expect("get_ready");
    frames_until_ready(&mut host);
    host.call_export("let_go", &[Value::I32(1)]).expect("let_go");
    frames(&mut host, 1);
    let built = host.pipelines_built();
    host.call_export("go", &[Value::I32(1)]).expect("go");
    frames(&mut host, 1);
    assert_eq!(running(&host), "handover-second");
    assert_eq!(host.pipelines_built(), built + 1, "its own pipeline, let go, was built again");
}

#[test]
#[ignore = "needs a GPU"]
fn a_build_let_go_before_it_is_ready_fails_the_request() {
    let mut host = Host::load(builds()).expect("load");
    frames(&mut host, 1);
    // Both are carried out after the next frame, in order: the build is still on its way.
    host.call_export("get_ready", &[Value::I32(1)]).expect("get_ready");
    host.call_export("let_go", &[Value::I32(1)]).expect("let_go");
    frames(&mut host, 2);
    assert_eq!(report(&mut host)[2], 2.0, "getting it ready failed");
}

/// What `tour` prints on its tour (compiler/tests/handover/tour), with the host's line about the
/// failed hand-over left out (it names a path, which the hosts write their own ways).
const TOUR: [&str; 8] = [
    "tour: start 1, restored 0",
    "tour: the missing build isn't ready",
    "tour: second is ready",
    "second: start 2, restored 1",
    "tour: start 3, restored 1",
    "tour: start 4, restored 1",
    "tour: the hand-over failed; it goes on",
    "tour: done",
];

/// The tour's lines, and whether the host said a hand-over failed and who started again.
fn tour_lines(lines: impl IntoIterator<Item = String>) -> (Vec<String>, bool) {
    let (host, tour): (Vec<String>, Vec<String>) =
        lines.into_iter().partition(|l| l.starts_with("the hand-over to"));
    let said =
        host.len() == 1 && host[0].contains("handover-broken") && host[0].contains("starts again");
    (tour, said)
}

#[test]
#[ignore = "needs a GPU"]
fn a_tour_of_hand_overs_runs_by_itself() {
    let mut host = Host::load(builds().with_file_name("handover-tour")).expect("load");
    let mut lines = Vec::new();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !lines.iter().any(|l: &String| l == "tour: done") && std::time::Instant::now() < end {
        frames(&mut host, 1);
        lines.extend(host.take_logs());
    }
    let (tour, said) = tour_lines(lines.clone());
    assert_eq!(tour, TOUR, "{lines:#?}");
    assert!(said, "the host says `broken` failed and `tour` starts again: {lines:#?}");
    assert_eq!(running(&host), "handover-tour");
    assert_eq!(pixel(&mut host, 32, 32), [255, 0, 0]);
}

#[test]
#[ignore = "needs the runtime"]
fn chrome_runs_the_tour_of_hand_overs_alike() {
    // The builds side by side, as pages beside each other.
    let mut rel = String::new();
    for p in ["second", "broken", "tour"] {
        rel = wrela_tests::page(
            &format!("compiler/tests/handover/{p}"),
            &format!("chrome-handover/handover-{p}"),
        )
        .1;
    }
    let run = wrela_tests::run_in_chrome(&rel, 90, SIZE, SIZE, 60.0);
    let (tour, said) = tour_lines(run.printed.into_iter().map(|(_, l)| l));
    assert_eq!(tour, TOUR);
    assert!(said, "the page says `broken` failed and `tour` starts again");
    assert_eq!(
        &run.frame[(32 * SIZE as usize + 32) * 4..][..3],
        [255, 0, 0],
        "`tour` draws at the end"
    );
}
