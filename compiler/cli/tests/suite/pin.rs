//! The toolchain a workspace pins (language.md §3.6, the CLI's `pin`): a `wrela` of another
//! release runs the pinned one, downloaded from a release server (here a test's, on this
//! machine) and checked against its `SHA256SUMS`; `WRELA_TOOLCHAIN` runs a checkout or an
//! unpacked toolchain instead; and the `wrela` chosen runs as itself.

use crate::common::{package, wrela};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// A `wrela` that says what it was given and why it was chosen.
const FAKE: &str = "#!/bin/sh\necho \"fake 9.9.9: $*\"\necho \"chosen: $WRELA_TOOLCHAIN_CHOSEN\"\n";

/// This machine's releases' target triple.
fn target() -> &'static str {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        other => panic!("no releases for {other:?}"),
    }
}

/// A gzipped tarball of `files`, each a path, its mode and its bytes.
fn tarball(files: &[(&str, u32, &[u8])]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (path, mode, bytes) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(bytes.len() as u64);
        h.set_mode(*mode);
        h.set_cksum();
        b.append_data(&mut h, path, *bytes).expect("append");
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&b.into_inner().expect("tar")).expect("gzip");
    gz.finish().expect("gzip")
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Serves `files` (each a path and its body; any other path is 404) on this machine until the
/// test ends: its base URL.
fn serve(files: Vec<(String, Vec<u8>)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("address").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&buf[..n]),
                }
            }
            let text = String::from_utf8_lossy(&request);
            let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
            let found = files.iter().find(|(p, _)| *p == path);
            let (status, body) = match found {
                Some((_, b)) => ("200 OK", b.clone()),
                None => ("404 Not Found", b"not found".to_vec()),
            };
            let head = format!(
                "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// A release `9.9.9` of the fake `wrela`, served: the base URL. `sums` replaces its
/// `SHA256SUMS`.
fn release(sums: Option<String>) -> String {
    let name = format!("wrela-9.9.9-{}.tar.gz", target());
    let tar = tarball(&[("bin/wrela", 0o755, FAKE.as_bytes()), ("VERSION", 0o644, b"9.9.9\n")]);
    let sums = sums.unwrap_or_else(|| format!("{}  {name}\n", sha256(&tar)));
    serve(vec![(format!("/v9.9.9/{name}"), tar), ("/v9.9.9/SHA256SUMS".into(), sums.into_bytes())])
}

/// A workspace that pins `version`, with a home of its own: the workspace and the home.
fn workspace(name: &str, version: &str) -> (PathBuf, PathBuf) {
    let root = package(
        name,
        &[("ws/wrela.toml", &format!("[workspace]\nmembers = []\ntoolchain = \"{version}\"\n"))],
    );
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("home");
    (root.join("ws"), home)
}

/// `wrela` with `args` in `ws`, its home `home`, downloading releases from `releases`.
fn run_in(ws: &Path, home: &Path, releases: &str, args: &[&str]) -> Output {
    wrela()
        .args(args)
        .current_dir(ws)
        .env("HOME", home)
        .env("WRELA_TOOLCHAIN_RELEASES", releases)
        .env_remove("WRELA_TOOLCHAIN")
        .env_remove("WRELA_TOOLCHAIN_CHOSEN")
        .output()
        .expect("run wrela")
}

fn text(out: &Output) -> (String, String) {
    (String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into())
}

/// The release a workspace pins is downloaded, checked, unpacked in place and run with the
/// same arguments; the next run uses it without the server.
#[test]
fn a_pinned_release_is_downloaded_checked_and_run_with_the_same_arguments() {
    let (ws, home) = workspace("pin-download", "9.9.9");
    let out = run_in(&ws, &home, &release(None), &["check", "pkg", "--json"]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(stdout.starts_with("fake 9.9.9: check pkg --json\n"), "{stdout}");
    assert!(stdout.contains("pins `toolchain = \"9.9.9\"`"), "{stdout}");
    assert!(stderr.contains("downloading"), "{stderr}");
    let kept = home.join(".wrela/toolchains");
    assert!(kept.join("9.9.9/bin/wrela").is_file() && kept.join("9.9.9/VERSION").is_file());
    let left: Vec<_> = std::fs::read_dir(&kept).expect("kept").flatten().collect();
    assert_eq!(left.len(), 1, "only the release is left: {left:?}");
    // Kept: no server is needed now.
    let out = run_in(&ws, &home, "http://127.0.0.1:9", &["fmt", "x"]);
    let (stdout, stderr) = text(&out);
    assert!(stdout.starts_with("fake 9.9.9: fmt x\n") && stderr.is_empty(), "{stdout}{stderr}");
}

/// A download that doesn't match its checksum is refused, and nothing is kept.
#[test]
fn a_release_that_does_not_match_its_checksum_is_refused() {
    let (ws, home) = workspace("pin-checksum", "9.9.9");
    let name = format!("wrela-9.9.9-{}.tar.gz", target());
    let wrong = format!("{}  {name}\n", "0".repeat(64));
    let out = run_in(&ws, &home, &release(Some(wrong)), &["check", "pkg"]);
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(2), "{stdout}{stderr}");
    assert!(stderr.contains("doesn't match the release's SHA256SUMS"), "{stderr}");
    assert!(stdout.is_empty(), "nothing ran: {stdout}");
    assert!(!home.join(".wrela/toolchains/9.9.9").exists());
}

/// A release the server doesn't have, and a server that can't be reached, say so.
#[test]
fn a_missing_release_and_no_network_are_clear_errors() {
    let (ws, home) = workspace("pin-missing", "9.9.8");
    let out = run_in(&ws, &home, &release(None), &["check", "pkg"]);
    let (_, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("there's no toolchain release 9.9.8"), "{stderr}");
    // Port 9 (discard) has nothing listening on a test machine.
    let out = run_in(&ws, &home, "http://127.0.0.1:9", &["check", "pkg"]);
    let (_, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("is the network reachable?"), "{stderr}");
    let out = run_in(&ws, &home, "http://example.com", &["check", "pkg"]);
    assert!(text(&out).1.contains("isn't HTTPS"), "{:?}", text(&out));
}

/// A workspace that pins this release runs this `wrela`.
#[test]
fn a_pin_of_this_release_runs_this_wrela() {
    let (ws, home) = workspace("pin-same", env!("CARGO_PKG_VERSION"));
    let out = run_in(&ws, &home, "http://127.0.0.1:9", &["toolchain"]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    let version = format!("wrela {} (", env!("CARGO_PKG_VERSION"));
    assert!(stdout.starts_with(&version) && stdout.contains("pins this release"), "{stdout}");
    let out = run_in(&ws, &home, "http://127.0.0.1:9", &["--version"]);
    assert!(text(&out).0.starts_with(&version), "{:?}", text(&out));
}

/// `WRELA_TOOLCHAIN` runs an unpacked toolchain's `bin/wrela`, or builds a checkout with Cargo
/// and runs its `target/release/wrela`, over the workspace's pin.
#[test]
fn the_override_runs_an_unpacked_toolchain_or_a_built_checkout() {
    use std::os::unix::fs::PermissionsExt;
    let (ws, home) = workspace("pin-override", "9.9.8");
    let root = ws.parent().expect("root").to_path_buf();
    let executable = |path: &Path, text: &str| {
        std::fs::create_dir_all(path.parent().expect("dir")).expect("mkdir");
        std::fs::write(path, text).expect("write");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    };
    let unpacked = root.join("unpacked");
    executable(&unpacked.join("bin/wrela"), FAKE);
    let alone = run_in(&ws, &home, "http://127.0.0.1:9", &["check", "pkg"]);
    assert_eq!(alone.status.code(), Some(2), "the pin alone can't be downloaded");
    let with = |toolchain: &Path, path: Option<&std::ffi::OsStr>| {
        let mut c = wrela();
        c.args(["check", "pkg"])
            .current_dir(&ws)
            .env("HOME", &home)
            .env("WRELA_TOOLCHAIN_RELEASES", "http://127.0.0.1:9")
            .env("WRELA_TOOLCHAIN", toolchain)
            .env_remove("WRELA_TOOLCHAIN_CHOSEN")
            .env_remove("CARGO_TARGET_DIR");
        if let Some(p) = path {
            c.env("PATH", p);
        }
        c.output().expect("run wrela")
    };
    let (stdout, stderr) = text(&with(&unpacked, None));
    assert!(stdout.starts_with("fake 9.9.9: check pkg\n"), "{stdout}{stderr}");
    assert!(stdout.contains("WRELA_TOOLCHAIN is the toolchain"), "{stdout}");
    // A checkout: a fake `cargo` on PATH says what it was asked and leaves a `wrela` behind.
    let checkout = root.join("checkout");
    std::fs::create_dir_all(&checkout).expect("mkdir");
    std::fs::write(checkout.join("Cargo.toml"), "[workspace]\n").expect("write");
    let fake_bin = root.join("fake-bin");
    let cargo = format!(
        "#!/bin/sh\necho \"$PWD $*\" > {}\nmkdir -p target/release\ncp {} target/release/wrela\n",
        root.join("cargo-args").display(),
        unpacked.join("bin/wrela").display()
    );
    executable(&fake_bin.join("cargo"), &cargo);
    let path = std::env::join_paths(
        std::iter::once(fake_bin.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
    )
    .expect("PATH");
    let (stdout, stderr) = text(&with(&checkout, Some(&path)));
    assert!(stdout.starts_with("fake 9.9.9: check pkg\n"), "{stdout}{stderr}");
    assert!(stdout.contains("WRELA_TOOLCHAIN is the checkout"), "{stdout}");
    let args = std::fs::read_to_string(root.join("cargo-args")).expect("cargo ran");
    assert_eq!(
        args.trim(),
        format!(
            "{} build -q --release -p wrela -p wrela-host -p wrela-serve --bins",
            checkout.canonicalize().expect("canonical").display()
        )
    );
}

/// The `wrela` chosen runs as itself: a copy of this `wrela`, chosen by `WRELA_TOOLCHAIN`,
/// doesn't choose again (which would run it again and again), and says where it is and why.
#[test]
fn the_chosen_wrela_runs_as_itself() {
    let (ws, home) = workspace("pin-itself", "9.9.8");
    let unpacked = ws.parent().expect("root").join("unpacked");
    std::fs::create_dir_all(unpacked.join("bin")).expect("mkdir");
    let copy = unpacked.join("bin/wrela");
    if std::fs::hard_link(env!("CARGO_BIN_EXE_wrela"), &copy).is_err() {
        std::fs::copy(env!("CARGO_BIN_EXE_wrela"), &copy).expect("copy wrela");
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_wrela"))
        .arg("toolchain")
        .current_dir(&ws)
        .env("HOME", &home)
        .env("WRELA_TOOLCHAIN", &unpacked)
        .env("WRELA_TOOLCHAIN_RELEASES", "http://127.0.0.1:9")
        .env_remove("WRELA_TOOLCHAIN_CHOSEN")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run wrela");
    let start = Instant::now();
    while child.try_wait().expect("wait").is_none() {
        if start.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            panic!("the chosen `wrela` chose again");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().expect("output");
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    let at = unpacked.join("bin").canonicalize().expect("canonical");
    assert!(stdout.contains(&format!("at {}\n", at.display())), "{stdout}");
    assert!(stdout.contains("because WRELA_TOOLCHAIN is the toolchain"), "{stdout}");
}
