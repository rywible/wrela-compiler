//! Hot reload (language.md §22, #51 AC9), in the native host: compiler/tests/reload, built
//! lifted, run in lockstep with a script of Space presses.
//!
//! - A literal set by the host (`__lift_set`) reaches the CPU and the GPU at the next frame;
//!   what the program cooked from literals is cooked again only when one it reads changes
//!   (`std::lift::Watch`).
//! - A new build swapped in (`Host::reload`) starts where the old one was: what it kept
//!   (`std::reload`) is what the new one's `kept()` gives, its ticker runs the old one's ticks
//!   again with their records (so the sim is the same, or what the new code makes of the same
//!   input), and the screen shows the new code at the next frame. In the same process, on
//!   the same device.

use std::path::{Path, PathBuf};
use wrela_host::{Host, Scripted, Value, parse_script};
use wrela_tests::{copy_dir, repo_root};

const SIZE: u32 = 64;
const FPS: f64 = 60.0;

fn package() -> PathBuf {
    repo_root().join("compiler/tests/reload")
}

/// The program at `pkg`, built lifted, into a scratch directory `name`.
fn built(name: &str, pkg: &Path) -> PathBuf {
    super::lift::build_into(name, pkg, &["main"], false)
}

/// A literal's index in the build at `dir`: the first on `line` of main.wrela, `nth` along.
fn literal(dir: &Path, line: u64, nth: usize) -> u32 {
    let r = super::lift::report(dir);
    let on: Vec<u32> = r["literals"]
        .as_array()
        .expect("literals")
        .iter()
        .enumerate()
        .filter(|(_, l)| l["line"].as_u64() == Some(line))
        .map(|(i, _)| i as u32)
        .collect();
    on[nth]
}

/// The line in main.wrela that holds `text`, from 1.
fn line_of(pkg: &Path, text: &str) -> u64 {
    let src = std::fs::read_to_string(pkg.join("main.wrela")).expect("main.wrela");
    src.lines().position(|l| l.contains(text)).expect("the line") as u64 + 1
}

fn script() -> Vec<Scripted> {
    parse_script(
        r#"[{"frame":10,"type":"key","key":"Space"},{"frame":50,"type":"key","key":"Space"}]"#,
    )
    .expect("a script")
}

fn vec4(host: &mut Host, name: &str) -> [f32; 4] {
    match host.call_export(name, &[]).expect(name).as_slice() {
        [Value::F32(a), Value::F32(b), Value::F32(c), Value::F32(d)] => [*a, *b, *c, *d],
        other => panic!("{name} returned {other:?}"),
    }
}

/// The screen's pixel at (x, y), RGB.
fn pixel(host: &mut Host, x: u32, y: u32) -> [u8; 3] {
    let s = host.read_screen().expect("the screen");
    let i = ((y * SIZE + x) * 4) as usize;
    [s[i], s[i + 1], s[i + 2]]
}

/// Runs frames `from..to` in lockstep.
fn frames(host: &mut Host, from: u32, to: u32, script: &[Scripted]) {
    for i in from..to {
        host.lockstep_frame(i, FPS, SIZE, SIZE, script)
            .unwrap_or_else(|e| panic!("frame {i}: {e}"));
    }
}

#[test]
#[ignore = "needs a GPU"]
fn a_literal_set_by_the_host_reaches_the_next_frame_and_what_was_cooked_from_it() {
    let dir = built("reload-literal", &package());
    let mut host = Host::load(&dir).expect("load");
    let s = script();
    frames(&mut host, 0, 5, &s);
    // The left half drawn by the GPU, the right cleared by the CPU at half.
    assert_eq!(pixel(&mut host, 8, 32), [64, 128, 191]);
    assert_eq!(pixel(&mut host, 56, 32), [32, 64, 96]);
    let tint_r = literal(&dir, line_of(&package(), "vec3(0.25, 0.5, 0.75)"), 0);
    let cook = literal(&dir, line_of(&package(), "1.5 * 2.0"), 0);
    host.set_literal(tint_r, 1.0).expect("set");
    frames(&mut host, 5, 6, &s);
    assert_eq!(pixel(&mut host, 8, 32), [255, 128, 191], "the GPU's");
    assert_eq!(pixel(&mut host, 56, 32), [128, 64, 96], "the CPU's");
    // The tint isn't cooked: nothing is cooked again.
    let c = vec4(&mut host, "carried");
    assert_eq!(c[3], 3.0);
    assert_eq!(host.call_export("cooks", &[]).expect("cooks"), vec![Value::I32(1)]);
    assert_eq!(vec4(&mut host, "report")[3], 1.0, "one change");
    // The cooked value's literal: cooked again, once.
    host.set_literal(cook, 2.0).expect("set");
    frames(&mut host, 6, 8, &s);
    assert_eq!(vec4(&mut host, "carried")[3], 4.0);
    assert_eq!(host.call_export("cooks", &[]).expect("cooks"), vec![Value::I32(2)]);
}

/// The package copied to a scratch directory `name`, with `edit` applied to main.wrela.
fn edited(name: &str, edit: impl Fn(&str) -> String) -> PathBuf {
    let dir = super::scratch(name);
    copy_dir(&package(), &dir);
    let main = dir.join("main.wrela");
    let src = std::fs::read_to_string(&main).expect("main.wrela");
    let new = edit(&src);
    assert_ne!(new, src, "the edit changes the source");
    std::fs::write(&main, new).expect("write main.wrela");
    dir
}

#[test]
#[ignore = "needs a GPU"]
fn a_new_build_takes_over_where_the_old_one_was() {
    let s = script();
    let old = built("reload-old", &package());
    // A structural edit that leaves the sim alone: the CPU's clear swizzles the tint, and a new
    // export.
    let pkg = edited("reload-new-src", |src| {
        src.replace("clear: vec4(tint() * 0.5, 1.0)", "clear: vec4(tint().zyx * 0.5, 1.0)")
            + "\npub fn version() -> u32 {\n    2\n}\n"
    });
    let new = built("reload-new", &pkg);
    let mut host = Host::load(&old).expect("load");
    frames(&mut host, 0, 120, &s);
    host.call_export("set_mark", &[Value::F32(3.5)]).expect("set_mark");
    frames(&mut host, 120, 121, &s);
    let before = vec4(&mut host, "report");
    assert_eq!(before[1], 2.0, "two Space presses reached the sim");
    let began = std::time::Instant::now();
    host.reload(Some(&new)).expect("reload");
    frames(&mut host, 121, 122, &s);
    let took = began.elapsed();
    let after = vec4(&mut host, "report");
    // The sim ran its ticks again with their records: where it was, a tick on.
    assert_eq!(after[1], 2.0, "the Space presses were replayed");
    assert_eq!(after[0], before[0] + 1.0);
    let mut fresh = Host::load(&old).expect("load");
    frames(&mut fresh, 0, 122, &s);
    assert_eq!(vec4(&mut fresh, "report"), after, "the sim as an unbroken run has it");
    // What the old build kept, carried; and the new code drawing.
    let c = vec4(&mut host, "carried");
    assert_eq!(c[..3], [122.0, 3.5, 1.0], "frames, mark, restored");
    assert_eq!(host.call_export("version", &[]).expect("version"), vec![Value::I32(2)]);
    assert_eq!(pixel(&mut host, 56, 32), [96, 64, 32], "the new clear");
    assert_eq!(pixel(&mut host, 8, 32), [64, 128, 191], "the kept pipeline");
    println!("the reload and its first frame took {:.0} ms", took.as_secs_f64() * 1000.0);
}

#[test]
#[ignore = "needs a GPU"]
fn a_new_sim_replays_the_old_ones_input() {
    let s = script();
    let old = built("reload-sim-old", &package());
    let pkg = edited("reload-sim-src", |src| {
        src.replace("const WEIGHT: u32 = 7", "const WEIGHT: u32 = 11")
    });
    let new = built("reload-sim-new", &pkg);
    let mut host = Host::load(&old).expect("load");
    frames(&mut host, 0, 90, &s);
    host.reload(Some(&new)).expect("reload");
    frames(&mut host, 90, 91, &s);
    // The new sim from the first tick, with the same input: as a run of the new build alone.
    let mut fresh = Host::load(&new).expect("load");
    frames(&mut fresh, 0, 91, &s);
    let (a, b) = (vec4(&mut host, "report"), vec4(&mut fresh, "report"));
    assert_eq!(a[..3], b[..3], "ticks, Space presses and the sum");
    let mut unchanged = Host::load(&old).expect("load");
    frames(&mut unchanged, 0, 91, &s);
    assert_ne!(vec4(&mut unchanged, "report")[2], a[2], "the new weight changed the sum");
}

/// Waits until the server has had `n` changes shown, or panics after `limit`.
fn wait_shown(
    server: &wrela_driver::live::Server,
    n: usize,
    limit: std::time::Duration,
) -> wrela_driver::live::Shown {
    let began = std::time::Instant::now();
    loop {
        let shown = server.shown();
        if shown.len() >= n {
            return shown[n - 1].clone();
        }
        assert!(
            began.elapsed() < limit,
            "change {n} wasn't shown in {limit:?}: {:?}",
            server.changes()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Writes `edit` of main.wrela at `pkg`: when it was written.
fn write_edit(pkg: &Path, edit: impl Fn(&str) -> String) -> std::time::Instant {
    let main = pkg.join("main.wrela");
    let src = std::fs::read_to_string(&main).expect("main.wrela");
    let new = edit(&src);
    assert_ne!(new, src);
    std::fs::write(&main, new).expect("write main.wrela");
    std::time::Instant::now()
}

/// `wrela run` in Chrome: the page takes a literal edit at its next frame and swaps a new
/// build in without reloading, with its sim replayed: its printed sim matches the native host's
/// unbroken run, frame by frame.
#[test]
#[ignore = "long: needs Chrome and a GPU"]
fn wrela_run_reloads_the_page_in_place() {
    let pkg = edited("reload-chrome-src", |src| src.to_string() + "\n");
    let out = super::scratch("reload-chrome");
    let server =
        wrela_driver::live::serve(&pkg, &out, 0, &["main".into()], false, true).expect("serve");
    std::fs::write(
        out.join("1/script.json"),
        r#"[{"frame":10,"type":"key","key":"Space"},{"frame":50,"type":"key","key":"Space"}]"#,
    )
    .expect("the script");
    let url = format!(
        "http://127.0.0.1:{}/#test&frames=900&width={SIZE}&height={SIZE}&fps=60&input=script.json&nohash=1",
        server.port
    );
    let page = out.clone();
    let chrome = std::thread::spawn(move || wrela_tests::run_url_in_chrome(&url, &page, 180));
    let limit = std::time::Duration::from_secs(60);
    // A first edit, shown once the page runs.
    write_edit(&pkg, |s| s.replacen("vec3(0.25, 0.5, 0.75)", "vec3(0.3, 0.5, 0.75)", 1));
    wait_shown(&server, 1, limit);
    let t = write_edit(&pkg, |s| s.replacen("vec3(0.3, 0.5, 0.75)", "vec3(1.0, 0.5, 0.75)", 1));
    let literal = wait_shown(&server, 2, limit);
    let literal_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = write_edit(&pkg, |s| {
        s.replace("clear: vec4(tint() * 0.5, 1.0)", "clear: vec4(tint().zyx * 0.5, 1.0)")
            + "\npub fn version() -> u32 {\n    2\n}\n"
    });
    let structural = wait_shown(&server, 3, limit);
    let structural_ms = t.elapsed().as_secs_f64() * 1000.0;
    chrome.join().expect("the Chrome run");
    println!(
        "a literal edit shown at frame {} after {literal_ms:.0} ms; a structural one at frame {} after {structural_ms:.0} ms",
        literal.frame, structural.frame
    );
    assert_eq!((literal.kind.as_str(), structural.kind.as_str()), ("literals", "build"));
    assert!(literal_ms <= 500.0, "a literal edit took {literal_ms:.0} ms");
    assert!(structural_ms <= 3000.0, "a structural edit took {structural_ms:.0} ms");
    // The last frame: the edited tint drawn by the GPU, the new clear.
    let frame = std::fs::read(out.join("results/frame.rgba")).expect("frame.rgba");
    let at = |x: u32, y: u32| {
        let i = ((y * SIZE + x) * 4) as usize;
        [frame[i], frame[i + 1], frame[i + 2]]
    };
    assert_eq!(at(8, 32), [255, 128, 191]);
    assert_eq!(at(56, 32), [96, 64, 128]);
    // The sim printed across the swap, against the native host's unbroken run.
    let printed = std::fs::read_to_string(out.join("results/log.txt")).expect("log.txt");
    let lines: Vec<&str> = printed.lines().filter(|l| l.starts_with("frame ")).collect();
    assert!(lines.len() >= 25, "{printed}");
    let mut native = Host::load_with(
        built("reload-chrome-native", &package()),
        &wrela_host::Options { quiet: true, ..wrela_host::Options::default() },
    )
    .expect("load");
    let s = script();
    let mut expected = Vec::new();
    for i in 0..900 {
        native.lockstep_frame(i, FPS, SIZE, SIZE, &s).expect("a frame");
        expected.extend(native.take_logs().into_iter().filter(|l| l.starts_with("frame ")));
    }
    assert_eq!(lines, expected.iter().map(String::as_str).collect::<Vec<_>>());
}
