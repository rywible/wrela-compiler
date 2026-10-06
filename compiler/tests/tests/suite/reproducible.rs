//! AC9, and AC15 of #41: builds are reproducible. The same package gives the same bytes, built
//! twice and built from a copy at another path: no timestamps, absolute paths or iteration-order
//! effects reach the output. Packages that embed files and compute constants at build time
//! (§10), and that have dependencies (§3), are among them.

use crate::scratch;
use wrela_tests::{build, copy_dir, repo_root};

#[test]
fn builds_are_reproducible() {
    let tmp = scratch("reproducible");
    // (what's copied, the package inside it): a package with path dependencies is copied
    // with them. Sketch 01 embeds a file, computes constants at build time and depends on the
    // engine package; sketch 02 has GPU pipelines.
    let pkgs = [
        ("examples/hello-field", ""),
        ("compiler/tests/fields", ""),
        ("compiler/tests/run/embed", ""),
        ("compiler/tests/run/packages", ""),
        ("compiler/tests/sketches", "01-creature"),
        ("compiler/tests/sketches", "02-drawing"),
    ];
    // The sketches' engine, where their path dependency finds it from the copies:
    // `../../../../engine` from somewhere/else/compiler-tests-sketches/<sketch>.
    if !tmp.join("engine").exists() {
        copy_dir(&repo_root().join("engine"), &tmp.join("engine"));
    }
    // Each package three times: from where it is, twice, and from a copy at another path.
    let sources: Vec<_> = pkgs
        .iter()
        .flat_map(|(root, pkg)| {
            let src = repo_root().join(root).join(pkg);
            let moved = tmp.join("somewhere/else").join(root.replace('/', "-"));
            if !moved.exists() {
                copy_dir(&repo_root().join(root), &moved);
            }
            [src.clone(), src, moved.join(pkg)]
        })
        .collect();
    // The first build of each package, and the copy's, at once; the second in a later
    // second of the clock, so a timestamp would differ.
    let wave = |srcs: Vec<&std::path::PathBuf>| -> Vec<wrela_driver::Output> {
        std::thread::scope(|s| {
            let builds: Vec<_> = srcs
                .into_iter()
                .map(|src| {
                    s.spawn(move || {
                        build(src)
                            .unwrap_or_else(|e| panic!("{} doesn't build:\n{e}", src.display()))
                    })
                })
                .collect();
            builds
                .into_iter()
                .map(|b| b.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
                .collect()
        })
    };
    let now =
        || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let started = now().as_secs();
    let mut early =
        wave(sources.iter().enumerate().filter(|(k, _)| k % 3 != 1).map(|(_, s)| s).collect());
    while now().as_secs() == started {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let mut late = wave(sources.iter().skip(1).step_by(3).collect());
    // Back in the order of `sources`: for each package, first, second, copy.
    let mut outputs = Vec::new();
    for _ in &pkgs {
        let first = early.remove(0);
        let copy = early.remove(0);
        outputs.extend([first, late.remove(0), copy]);
    }
    for ((root, pkg), out) in pkgs.iter().zip(outputs.chunks(3)) {
        let pkg = format!("{root}/{pkg}");
        let [first, second, third] = [0, 1, 2].map(|k| &out[k].files);
        assert!(first.iter().any(|(n, _)| n == "game.wasm"), "{pkg}: no game.wasm");
        assert_eq!(first, second, "{pkg}: two builds differ");
        assert_eq!(first, third, "{pkg}: a copy at another path builds differently");
    }
}

/// A diagnostic in a dependency names the package and the path inside it (§3).
#[test]
fn a_dependency_diagnostic_names_its_package() {
    let tmp = scratch("dependency-diagnostic");
    let pkg = tmp.join("app");
    let dep = tmp.join("geo");
    for (path, text) in [
        (
            pkg.join("main.wrela"),
            "use geo::shapes::area\n\npub fn frame(time: f32, width: u32, height: u32) {}\n",
        ),
        (pkg.join("wrela.toml"), "[dependencies]\ngeo = { path = \"../geo\" }\n"),
        (dep.join("shapes/circle.wrela"), "pub fn area(r: f32) -> f32 {\n    r * true\n}\n"),
        (dep.join("shapes.wrela"), "pub use shapes::circle::area\n"),
    ] {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, text).expect("write");
    }
    let out = wrela_driver::check(&pkg);
    let shown = wrela_diag::render::render_all(&out.sources, &out.diagnostics);
    assert!(shown.contains("--> [geo] shapes/circle.wrela:2:"), "{shown}");
}
