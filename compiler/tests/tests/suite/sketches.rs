//! AC11: the programs the language was designed from, sketches 01–03 (design-archive-2026-10:
//! docs/design/sketches/), written in the language as built, with the engine they're written
//! against (the top-level `engine/` package). Each change from a sketch is noted next to its
//! code ("Changed:"), with its reason.

use crate::built;
use wrela_host::{CpuBuild, CpuHost, Value};

fn cpu(pkg: &str) -> CpuHost {
    CpuBuild::load(built(pkg)).expect("load").start_with(1).expect("start")
}

fn vec2_of(v: &[Value]) -> [f32; 2] {
    match v {
        [Value::F32(a), Value::F32(b)] => [*a, *b],
        other => panic!("expected a vec2, got {other:?}"),
    }
}

/// Sketch 01's creature is M1's tier-0 grazer (compiler/tests/fields/grazer.wrela), built the
/// sketch's way: a skeleton, parts in bone space, `.blend(k)`. Their distances agree within
/// 1e-5 m at 10⁶ points around each of three individuals.
#[test]
fn sketch_01s_creature_is_m1s_grazer() {
    let mut host = cpu("sketches/01-check");
    for id in [1, 2, 7] {
        let n = 1_000_000 / 3 + 1;
        let r = host.call_export("max_gap", &[Value::I32(id), Value::I32(n)]).expect("max_gap");
        let [gap, near] = vec2_of(&r);
        println!(
            "grazer {id}: {n} points ({near} within 1 cm of the surface), largest gap {gap:e} m"
        );
        assert!(gap <= 1e-5, "grazer {id}: the distances differ by {gap} m");
        assert!(near > 1000.0, "grazer {id}: only {near} points near the surface");
    }
}

fn one(host: &mut CpuHost, name: &str, args: &[Value]) -> Vec<Value> {
    host.call_export(name, args).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn vec4_of(v: &[Value]) -> [f32; 4] {
    match v {
        [Value::F32(a), Value::F32(b), Value::F32(c), Value::F32(d)] => [*a, *b, *c, *d],
        other => panic!("expected a vec4, got {other:?}"),
    }
}

/// Sketch 01 §4: the physique is computed by `@deterministic` code from the field. The mass is
/// the grazer's volume times its tissue's density (hide 1050 kg/m³, hooves 1300): about a
/// tonne, a bison's. Measured: the 1 cm integration and capsule fit take about 10 s in
/// wasmtime (two octrees, each about 60,000 derived intervals of the 20-part creature, at about
/// 17 µs each): far over a spawn's budget. The sketch estimated 100,000 cells and left the
/// measurement to the D-067 spike.
#[test]
fn sketch_01s_physique() {
    let mut host = cpu("sketches/01-creature");
    let started = std::time::Instant::now();
    let [mass, bones, capsules, radius] = vec4_of(&one(&mut host, "physique", &[Value::I32(1)]));
    println!(
        "grazer 1: {mass:.1} kg over {bones} bones, {capsules} capsules (the first {radius:.3} m); {:.2} s",
        started.elapsed().as_secs_f64()
    );
    assert!((500.0..2000.0).contains(&mass), "a grazer of {mass} kg");
    assert!(bones >= 10.0 && capsules == bones, "{bones} bones with mass, {capsules} capsules");
}

/// Sketch 01 §6 and §7 running: three grazers stepped (`@deterministic`), animated and drawn
/// each frame, and their hooves rung on the audio thread. The world's hash is the same on two
/// runs, and the voice sounds.
#[test]
fn sketch_01s_herd_walks_and_rings() {
    let run = || {
        let mut host = cpu("sketches/01-creature");
        for i in 0..120 {
            host.frame(i as f32 / 60.0, 16, 16).expect("frame");
        }
        let hash = one(&mut host, "world_hash", &[]);
        let drawn = vec2_of(&one(&mut host, "drawn", &[]));
        let samples = host.render_audio(400).expect("audio");
        (hash, drawn, samples)
    };
    let (hash, [drawn, strikes], samples) = run();
    println!("{drawn} grazers drawn, {strikes} hoof strikes");
    assert_eq!(drawn, 3.0);
    assert!(strikes >= 3.0, "{strikes} hoof strikes");
    let peak = samples.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    assert!(peak > 1e-3 && peak <= 1.0 && samples.iter().all(|x| x.is_finite()), "peak {peak}");
    assert_eq!(run().0, hash, "the world's hash differs between runs");
}

/// Sketch 02: sketch 01's grazer realized on the GPU (its blocks culled by the derived interval,
/// a vertex placed in each crossed cell, quads joined through an `AtomicMap`, skin weights from
/// its parts' distances, counts kept on the GPU) and drawn each frame, skinned by a palette
/// that sways its neck and shaded per pixel by its field. Chrome and the native host give the
/// same frame, within a mean of 0.5/255 (AC11), and the grazer fills part of it.
#[test]
#[ignore = "needs Chrome, python3 and a GPU"]
fn sketch_02_draws_the_grazer_in_both_hosts() {
    use wrela_host::image;
    const FRAMES: u32 = 4;
    let (w, h) = (192, 128);
    let (dir, rel) = wrela_tests::page("compiler/tests/sketches/02-drawing", "sketch-02");
    let chrome = wrela_tests::run_in_chrome(&rel, FRAMES, w, h, 60.0);
    let times: Vec<f32> = (0..FRAMES).map(|i| wrela_host::frame_time(i, 60.0)).collect();
    let run = wrela_host::Host::load(&dir).expect("load").run_frames(&times, w, h).expect("run");
    run.write_png(dir.join("results/native.png")).expect("png");
    let diff = image::compare(&chrome.frame, &run.frame).expect("same size");
    println!(
        "sketch 02: Chrome against the native host: mean {:.4}/255, max {}/255",
        diff.mean, diff.max
    );
    assert!(diff.mean <= 0.5, "the hosts' frames differ by a mean of {}/255", diff.mean);
    // The sky is (0.55, 0.7, 0.85): count what isn't.
    let sky = [140u8, 178, 217];
    let creature =
        run.frame.chunks_exact(4).filter(|p| (0..3).any(|c| p[c].abs_diff(sky[c]) > 8)).count();
    let share = creature as f64 / f64::from(w * h);
    println!("the grazer covers {:.1}% of the frame", 100.0 * share);
    assert!(share > 0.05 && share < 0.8, "the grazer covers {:.1}% of the frame", 100.0 * share);
}

/// Sketch 02 §5: a herd shares its pipelines. The program draws two grazers of two seeds: one
/// type, so each entry point has one instantiation (6 pipelines: realization's five passes, its
/// skin weights one of them, and the draw's two shaders in one), and the seeds' numbers travel
/// as uniforms. Without a GPU.
#[test]
fn sketch_02s_herd_shares_its_pipelines() {
    let out =
        wrela_driver::check(&wrela_tests::repo_root().join("compiler/tests/sketches/02-drawing"));
    assert!(!out.has_errors());
    let names: Vec<String> = out.pipelines.iter().map(|p| p.entries.join(" + ")).collect();
    println!(
        "{} pipelines: {}",
        names.len(),
        names.iter().map(|n| n.split("::").next().unwrap_or("")).collect::<Vec<_>>().join(", ")
    );
    assert_eq!(out.pipelines.len(), 6, "{names:#?}");
    let draws = out.pipelines.iter().find(|p| p.names.contains(&"skin".to_string())).expect("skin");
    assert_eq!(draws.sites.len(), 1, "drawn from one site: {:?}", draws.sites);
}

fn u64_of(v: &[Value]) -> u64 {
    match v {
        [Value::I64(x)] => *x as u64,
        other => panic!("expected a u64, got {other:?}"),
    }
}

/// The tick's parallel loop gives the same world with 1, 2 and 4 threads (§6.12).
#[test]
fn sketch_03s_tick_doesnt_depend_on_the_workers() {
    let run = |workers: u32| {
        let mut host = CpuBuild::load(built("sketches/03-simulation"))
            .expect("load")
            .start_with(workers)
            .expect("start");
        one(&mut host, "run", &[Value::I32(120)]);
        u64_of(&one(&mut host, "checksum", &[]))
    };
    let one_thread = run(1);
    assert_eq!(run(2), one_thread);
    assert_eq!(run(4), one_thread);
}

/// The programs' own tests (`@test`, `wrela test`): sketch 01's torso bound and hoof modes,
/// sketch 03's timeline replays and saves, the gameplay paper test's scripted player (a frame
/// test of 360 frames), the examples': hello field's, and the wolf's and the grazer's walks
/// (frame tests of 120 frames, AC7).
#[test]
fn the_programs_own_tests_pass() {
    let programs = [
        "compiler/tests/sketches/01-creature",
        "compiler/tests/sketches/03-simulation",
        "compiler/tests/sketches/gameplay",
        "examples/hello-field",
        "examples/wolf",
        "examples/grazer",
    ];
    for pkg in programs {
        let dir = wrela_tests::repo_root().join(pkg);
        let out = wrela_driver::test(&dir, None);
        let shown = |ds: &[&wrela_diag::Diagnostic]| {
            let ds: Vec<wrela_diag::Diagnostic> = ds.iter().map(|&d| d.clone()).collect();
            wrela_diag::render::render_all(&out.sources, &ds)
        };
        let errors: Vec<_> = out.diagnostics.iter().filter(|d| d.is_error()).collect();
        assert!(errors.is_empty(), "{pkg}:\n{}", shown(&errors));
        let failed: Vec<_> = out.results.iter().filter_map(|r| r.failure.as_ref()).collect();
        assert!(failed.is_empty(), "{pkg}:\n{}", shown(&failed));
        assert!(!out.results.is_empty(), "{pkg}: no tests ran");
    }
}

const GAMEPLAY_FRAMES: u32 = 360;

/// The four systems in Chrome and in the native host: the same frames (their state hash covers
/// every command, the UI's draws among them).
#[test]
#[ignore = "needs Chrome, python3 and a GPU"]
fn the_gameplay_systems_run_in_both_hosts() {
    let (dir, rel) = wrela_tests::page("compiler/tests/sketches/gameplay", "gameplay-hosts");
    let chrome = wrela_tests::run_in_chrome(&rel, GAMEPLAY_FRAMES, 640, 480, 60.0);
    let times: Vec<f32> = (0..GAMEPLAY_FRAMES).map(|i| wrela_host::frame_time(i, 60.0)).collect();
    let run =
        wrela_host::Host::load(&dir).expect("load").run_frames(&times, 640, 480).expect("run");
    assert_eq!(chrome.hash, run.hash_hex(), "the hosts' state hashes differ");
    let diff = wrela_host::image::compare(&chrome.frame, &run.frame).expect("same size");
    println!(
        "gameplay: hash {} in both; frames differ by a mean of {:.4}/255",
        run.hash_hex(),
        diff.mean
    );
    assert!(diff.mean <= 0.5);
}
