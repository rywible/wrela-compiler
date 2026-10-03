//! AC9: builds are reproducible. The same package gives the same bytes, built twice and built
//! from a copy at another path: no timestamps, absolute paths or iteration-order effects reach
//! the output.

use crate::scratch;
use wrela_tests::{build, copy_dir, repo_root};

#[test]
fn builds_are_reproducible() {
    let tmp = scratch("reproducible");
    let pkgs = ["examples/hello-field", "compiler/tests/fields"];
    // Each package three times: from where it is, twice, and from a copy at another path.
    let sources: Vec<_> = pkgs
        .iter()
        .flat_map(|pkg| {
            let src = repo_root().join(pkg);
            let moved = tmp.join("somewhere/else").join(pkg.replace('/', "-"));
            copy_dir(&src, &moved);
            [src.clone(), src, moved]
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
    for (pkg, out) in pkgs.iter().zip(outputs.chunks(3)) {
        let [first, second, third] = [0, 1, 2].map(|k| &out[k].files);
        assert!(first.iter().any(|(n, _)| n == "game.wasm"), "{pkg}: no game.wasm");
        assert_eq!(first, second, "{pkg}: two builds differ");
        assert_eq!(first, third, "{pkg}: a copy at another path builds differently");
    }
}
