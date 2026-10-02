//! AC9: builds are reproducible. The same package gives the same bytes, built twice and built
//! from a copy at another path: no timestamps, absolute paths or iteration-order effects reach
//! the output.

use std::path::{Path, PathBuf};
use wrela_tests::{build, root};

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("dir");
    for e in std::fs::read_dir(from).expect("read dir") {
        let p = e.expect("entry").path();
        let name = p.file_name().expect("name");
        if name == "build" {
            continue;
        }
        if p.is_dir() {
            copy_dir(&p, &to.join(name));
        } else {
            std::fs::copy(&p, to.join(name)).expect("copy");
        }
    }
}

fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
        .expect("build dir")
        .map(|e| {
            let p = e.expect("entry").path();
            (
                p.file_name().expect("name").to_string_lossy().into_owned(),
                std::fs::read(&p).expect("read"),
            )
        })
        .collect();
    out.sort();
    out
}

#[test]
fn builds_are_reproducible() {
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("reproducible");
    let _ = std::fs::remove_dir_all(&tmp);
    for pkg in ["examples/hello-field", "compiler/tests/fields"] {
        let src = root().join(pkg);
        let name = pkg.replace('/', "-");
        let first = tmp.join(format!("{name}-1"));
        let second = tmp.join(format!("{name}-2"));
        let moved = tmp.join("somewhere/else").join(&name);
        let third = tmp.join(format!("{name}-3"));
        build(&src, &first).expect("builds");
        build(&src, &second).expect("builds");
        copy_dir(&src, &moved);
        build(&moved, &third).expect("builds");
        let a = files(&first);
        assert!(a.iter().any(|(n, _)| n == "game.wasm"), "{pkg}: no game.wasm");
        assert_eq!(a, files(&second), "{pkg}: two builds differ");
        assert_eq!(a, files(&third), "{pkg}: a copy at another path builds differently");
    }
}
