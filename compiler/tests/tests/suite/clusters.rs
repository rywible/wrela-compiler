//! The virtual geometry spike (examples/clusters, `engine::clusters`): a smooth field cooked as
//! clusters at six levels and drawn, each place at the coarsest level whose error looks under
//! half a pixel. A cut of several levels holds every quad it wants, opens no crack where its
//! levels meet (each cluster's skirt covers what a coarser neighbour leaves), and keeps every
//! silhouette within a pixel of the finest level's.
//! `cargo test -p wrela-tests --test suite clusters:: -- --ignored`.

use wrela_host::{Host, Value};
use wrela_tests::page;

const W: u32 = 640;
const H: u32 = 360;

/// The eye low over the field of copies, looking along a row: near copies a few metres off, far
/// ones twenty, so the cut takes several levels.
const EYE: [f32; 3] = [-2.0, 0.9, -1.5];
const AT: [f32; 3] = [6.0, 0.3, 9.0];

/// The example built as page `name` (each test its own: two tests building one page race).
fn host(name: &str) -> Host {
    let (dir, _) = page("examples/clusters", name);
    let mut host = Host::load(&dir).expect("load");
    host.init().expect("init");
    host
}

/// The frame with the eye at `EYE` looking at `AT`, `pixels` of error, `side` × `side` copies.
fn frame(host: &mut Host, pixels: f32, side: i32) -> Vec<u8> {
    let mut args: Vec<Value> = EYE.iter().chain(&AT).map(|&x| Value::F32(x)).collect();
    args.push(Value::F32(pixels));
    args.push(Value::I32(side));
    host.call_export("view", &args).expect("view");
    for i in 0..3 {
        host.frame(i as f32 / 60.0, W, H).expect("frame");
    }
    host.read_screen().expect("read the screen")
}

fn int(v: &[Value]) -> u32 {
    match v.first() {
        Some(Value::I32(i)) => *i as u32,
        other => panic!("not an integer: {other:?}"),
    }
}

/// Which pixels of `frame` show a copy: not the sky's colour (its top left pixel's).
fn covered(frame: &[u8]) -> Vec<bool> {
    let sky = &frame[0..3];
    frame.chunks(4).map(|p| p[..3] != *sky).collect()
}

/// The last frame's counts (`Clusters::counts`), checked: every level's clusters' quads fit its
/// room, and so do the quads its frame listed. How many levels listed some.
fn levels_listed(host: &mut Host) -> u32 {
    host.call_export("ask_counts", &[]).expect("ask");
    let mut n = 0;
    for i in 0..30 {
        host.frame(1.0 + i as f32 / 60.0, W, H).expect("frame");
        n = int(&host.call_export("counts", &[]).expect("counts"));
        if n > 0 {
            break;
        }
    }
    assert!(n > 0, "the counts never arrived");
    let at = |host: &mut Host, i: u32| {
        int(&host.call_export("count", &[Value::I32(i as i32)]).expect("count"))
    };
    let mut levels = 0;
    for level in 0..n / 4 {
        let (wanted, room) = (at(host, 4 * level), at(host, 4 * level + 1));
        let (listed, list_room) = (at(host, 4 * level + 2), at(host, 4 * level + 3));
        eprintln!("level {level}: {wanted} quads of {room}, {listed} listed of {list_room}");
        assert!(wanted <= room, "level {level} wanted {wanted} quads, room for {room}");
        assert!(listed <= list_room, "level {level} listed {listed} quads, room for {list_room}");
        if listed > 0 {
            levels += 1;
        }
    }
    levels
}

/// Every level's quads and listed quads fit their rooms, and the cut over the field of 64
/// copies takes at least three levels.
#[test]
#[ignore = "needs a GPU"]
fn a_cut_holds_every_quad_it_wants() {
    let mut host = host("clusters-rooms");
    frame(&mut host, 0.5, 8);
    let levels = levels_listed(&mut host);
    assert!(levels >= 3, "the cut took {levels} levels");
}

/// Over 9 copies (the finest level drawn everywhere, 669,000 quads a copy, fits its room for 9,
/// not 64), a cut of at
/// least three levels, against the finest level drawn everywhere (no error allowed), shows the sky nowhere
/// the finest level covers a pixel and all eight round it (a crack), and every pixel the two
/// cover differently is beside the finest level's silhouette (within a pixel of it).
#[test]
#[ignore = "needs a GPU"]
fn a_cut_opens_no_crack_and_keeps_its_silhouettes() {
    let mut host = host("clusters-cracks");
    let cut_frame = frame(&mut host, 0.5, 3);
    let levels = levels_listed(&mut host);
    assert!(levels >= 3, "the cut took {levels} levels");
    let finest_frame = frame(&mut host, 0.0, 3);
    if let Ok(dir) = std::env::var("WRELA_CLUSTERS_DUMP") {
        std::fs::write(format!("{dir}/cut.rgba"), &cut_frame).unwrap();
        std::fs::write(format!("{dir}/finest.rgba"), &finest_frame).unwrap();
    }
    let cut = covered(&cut_frame);
    let finest = covered(&finest_frame);
    let (w, h) = (W as usize, H as usize);
    let at = |m: &[bool], x: usize, y: usize| m[y * w + x];
    let (mut cracks, mut far, mut differ) = (0, 0, 0);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let around = |m: &[bool], want: bool| {
                (0..9).all(|k| at(m, x + k % 3 - 1, y + k / 3 - 1) == want)
            };
            if !at(&cut, x, y) && around(&finest, true) {
                cracks += 1;
            }
            if at(&cut, x, y) != at(&finest, x, y) {
                differ += 1;
                // A pixel the two cover differently, with no pixel round it that the finest level
                // covers differently from it: off the finest level's silhouette.
                if around(&finest, at(&finest, x, y)) {
                    far += 1;
                }
            }
        }
    }
    let shown = finest.iter().filter(|&&c| c).count();
    eprintln!(
        "{shown} pixels covered; {differ} covered differently, {far} away from a silhouette, {cracks} cracks"
    );
    assert!(shown > w * h / 10, "the copies cover too little to test: {shown} pixels");
    assert_eq!(cracks, 0, "the cut opened {cracks} cracks");
    assert_eq!(far, 0, "{far} pixels differ away from the finest level's silhouettes");
}

/// Chrome draws the moving eye's cut of the 64 copies as the native host does: the same state
/// hash, and frames within a mean of 0.5/255 (Tint takes the clusters' shaders, and its atomics
/// keep each cluster's quads together as wgpu's do).
#[test]
#[ignore = "needs a GPU and Chrome"]
fn the_browser_draws_the_native_hosts_clusters() {
    let (dir, rel) = page("examples/clusters", "clusters-browser");
    let browser = wrela_tests::run_in_chrome(&rel, 3, W, H, 60.0);
    let run = Host::load(&dir)
        .expect("load")
        .run_frames(&[0.0, 1.0 / 60.0, 2.0 / 60.0], W, H)
        .expect("run");
    assert_eq!(browser.hash, run.hash_hex(), "the hosts' state hashes differ");
    let diff = wrela_host::image::compare(&browser.frame, &run.frame).expect("same size");
    eprintln!("Chrome against the native host: mean {:.4}/255, max {}/255", diff.mean, diff.max);
    assert!(diff.mean <= 0.5, "mean difference {:.4}", diff.mean);
}
