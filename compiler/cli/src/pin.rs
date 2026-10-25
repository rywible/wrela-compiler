//! The toolchain a workspace runs (language.md §3.6): a workspace's manifest pins a release with
//! `toolchain = "X.Y.Z"` under `[workspace]`, and a `wrela` of another version, run in it, runs
//! that release instead, with the same arguments. A release is kept in
//! `~/.wrela/toolchains/X.Y.Z`; one that isn't there is downloaded from the toolchain's GitHub
//! releases (or `WRELA_TOOLCHAIN_RELEASES`), checked against the release's `SHA256SUMS` and
//! unpacked into a temporary directory that is then renamed into place.
//!
//! `WRELA_TOOLCHAIN` overrides the pin: the path of a `wrela-compiler` checkout, built with Cargo
//! and run from its `target/release`, or of an unpacked toolchain, run from its `bin/`. The
//! `wrela` chosen runs with `WRELA_TOOLCHAIN_CHOSEN` set to why, so it runs as itself and never
//! chooses again. `wrela toolchain` says which toolchain runs, where it is and why.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// This toolchain's release.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The source revision a release was built from (`tools/package.py` sets it); `None` in a
/// development build.
pub const REVISION: Option<&str> = option_env!("WRELA_REVISION");
/// Where the toolchain's source is, and its releases.
pub const REPOSITORY: &str = "https://github.com/rywible/wrela-compiler";
/// Set, in the environment of the `wrela` chosen, to why it was: that `wrela` runs as itself.
const CHOSEN: &str = "WRELA_TOOLCHAIN_CHOSEN";
/// The most a download or unpacked tar stream may be.
const MOST: u64 = 1 << 30;
/// A SHA256SUMS file contains a few short lines, never a binary payload.
const SUMS_MOST: u64 = 1 << 20;

/// `wrela --version`'s line: the release and the revision it was built from.
pub fn version_line() -> String {
    format!("wrela {VERSION} ({})", REVISION.unwrap_or("development build"))
}

/// Which toolchain runs.
#[derive(Debug, PartialEq)]
pub enum Choice {
    /// This `wrela`, and why.
    Here(String),
    /// The release `version`, which the workspace's manifest at `manifest` pins.
    Pinned { version: String, manifest: PathBuf },
    /// A `wrela-compiler` checkout, from `WRELA_TOOLCHAIN`.
    Checkout(PathBuf),
    /// An unpacked toolchain, from `WRELA_TOOLCHAIN`.
    Unpacked(PathBuf),
}

/// Chooses the toolchain for a `wrela` of `version` run in `dir`, given `WRELA_TOOLCHAIN`'s
/// value: the override, else the pin of the workspace at or above `dir` if it isn't `version`,
/// else this one. A manifest that doesn't read pins nothing here: the command reports it.
pub fn choose(overridden: Option<&OsStr>, dir: &Path, version: &str) -> Result<Choice, String> {
    if let Some(path) = overridden.filter(|p| !p.is_empty()) {
        let at = std::path::absolute(path).map_err(|e| format!("WRELA_TOOLCHAIN: {e}"))?;
        return if at.join("Cargo.toml").is_file() {
            Ok(Choice::Checkout(at))
        } else if at.join("bin/wrela").is_file() {
            Ok(Choice::Unpacked(at))
        } else {
            Err(format!(
                "WRELA_TOOLCHAIN is `{}`, which is neither a wrela-compiler checkout (it has no \
                 Cargo.toml) nor a toolchain (it has no bin/wrela)",
                at.display()
            ))
        };
    }
    let Some(manifest) = wrela_driver::workspace::find(dir) else {
        return Ok(Choice::Here(format!("no workspace is at or above `{}`", dir.display())));
    };
    let pin = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|t| wrela_driver::manifest::parse(wrela_diag::FileId(0), &t).ok())
        .and_then(|m| m.workspace?.toolchain)
        .map(|(v, _)| v);
    Ok(match pin {
        Some(v) if v != version => Choice::Pinned { version: v, manifest },
        Some(v) => Choice::Here(format!("`{}` pins this release, {v}", manifest.display())),
        None => Choice::Here(format!("`{}` pins no toolchain", manifest.display())),
    })
}

/// Runs the toolchain chosen for this directory instead of this `wrela`, if it isn't this one:
/// `None` to go on here, or the status to exit with (an error is printed).
pub fn redirect() -> Option<ExitCode> {
    if std::env::var_os(CHOSEN).is_some() {
        return None;
    }
    let dir = std::env::current_dir().ok()?;
    let chosen = choose(std::env::var_os("WRELA_TOOLCHAIN").as_deref(), &dir, VERSION)
        .and_then(|c| locate(&c).map(|exe| (c, exe)));
    let (choice, exe) = match chosen {
        Ok((_, None)) => return None,
        Ok((c, Some(exe))) => (c, exe),
        Err(e) => {
            eprintln!("error: {e}");
            return Some(ExitCode::from(2));
        }
    };
    Some(run_instead(&exe, &why(&choice)))
}

/// Why `choice` was made, as `wrela toolchain` says it.
fn why(choice: &Choice) -> String {
    match choice {
        Choice::Here(why) => why.clone(),
        Choice::Pinned { version, manifest } => {
            format!("`{}` pins `toolchain = \"{version}\"`", manifest.display())
        }
        Choice::Checkout(dir) => format!("WRELA_TOOLCHAIN is the checkout `{}`", dir.display()),
        Choice::Unpacked(dir) => format!("WRELA_TOOLCHAIN is the toolchain `{}`", dir.display()),
    }
}

/// The `wrela` to run for `choice`, made ready (downloaded, or built): `None` for this one.
fn locate(choice: &Choice) -> Result<Option<PathBuf>, String> {
    Ok(Some(match choice {
        Choice::Here(_) => return Ok(None),
        Choice::Pinned { version, .. } => {
            let base = std::env::var("WRELA_TOOLCHAIN_RELEASES")
                .ok()
                .filter(|b| !b.is_empty())
                .unwrap_or_else(|| format!("{REPOSITORY}/releases/download"));
            install(version, base.trim_end_matches('/'), &toolchains()?)?.join("bin/wrela")
        }
        Choice::Checkout(dir) => build_checkout(dir)?,
        Choice::Unpacked(dir) => dir.join("bin/wrela"),
    }))
}

/// Runs `exe` with this process's arguments, chosen because of `why`, and exits as it does.
fn run_instead(exe: &Path, why: &str) -> ExitCode {
    let mut command = Command::new(exe);
    command.args(std::env::args_os().skip(1)).env(CHOSEN, why);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let e = command.exec();
        eprintln!("error: can't run the toolchain `{}` ({why}): {e}", exe.display());
        ExitCode::from(2)
    }
    #[cfg(not(unix))]
    match command.status() {
        Ok(s) => ExitCode::from(s.code().map_or(2, |c| c as u8)),
        Err(e) => {
            eprintln!("error: can't run the toolchain `{}` ({why}): {e}", exe.display());
            ExitCode::from(2)
        }
    }
}

/// Where downloaded releases are kept: `~/.wrela/toolchains`.
fn toolchains() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .ok_or("HOME isn't set, so there's no ~/.wrela/toolchains to keep the toolchain in")?;
    Ok(PathBuf::from(home).join(".wrela/toolchains"))
}

/// The target triple of this machine's releases, if it has them.
fn target() -> Result<&'static str, String> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => Ok("x86_64-unknown-linux-gnu"),
        ("aarch64", "macos") => Ok("aarch64-apple-darwin"),
        (arch, os) => Err(format!(
            "the toolchain has no releases for {arch} {os}: set WRELA_TOOLCHAIN to a \
             wrela-compiler checkout to build one"
        )),
    }
}

/// The release `version`'s directory in `root`, downloaded from `base` (`<base>/v<version>/`)
/// if it isn't there: its tarball for this machine, checked against the release's
/// `SHA256SUMS`, unpacked into a temporary directory that is then renamed into place.
pub fn install(version: &str, base: &str, root: &Path) -> Result<PathBuf, String> {
    let dir = root.join(version);
    if dir.join("bin/wrela").is_file() {
        return Ok(dir);
    }
    let name = format!("wrela-{version}-{}.tar.gz", target()?);
    let at = format!("{base}/v{version}");
    eprintln!("wrela: this workspace pins the toolchain {version}: downloading {at}/{name}");
    let sums = fetch(&format!("{at}/SHA256SUMS"), version, SUMS_MOST)?;
    let sums = String::from_utf8_lossy(&sums);
    let want = sums
        .lines()
        .find_map(|l| {
            let (sum, file) = l.split_once(char::is_whitespace)?;
            (file.trim_start().trim_start_matches('*') == name).then(|| sum.to_ascii_lowercase())
        })
        .ok_or_else(|| format!("the release {version}'s SHA256SUMS has no line for {name}"))?;
    let tarball = fetch(&format!("{at}/{name}"), version, MOST)?;
    let got = crate::sha256_hex(&tarball);
    if got != want {
        return Err(format!(
            "the download of {name} doesn't match the release's SHA256SUMS (it is {got}, the \
             release says {want}), so it isn't used"
        ));
    }
    std::fs::create_dir_all(root).map_err(|e| format!("can't make `{}`: {e}", root.display()))?;
    let temp = root.join(format!(".{version}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    let unpacked = unpack(&tarball, &temp).and_then(|()| {
        if temp.join("bin/wrela").is_file() {
            Ok(())
        } else {
            Err(format!("{name} has no bin/wrela"))
        }
    });
    if let Err(e) = unpacked {
        let _ = std::fs::remove_dir_all(&temp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&temp, &dir) {
        let _ = std::fs::remove_dir_all(&temp);
        // Another `wrela` may have put the release in place first.
        if !dir.join("bin/wrela").is_file() {
            return Err(format!("can't put the toolchain in `{}`: {e}", dir.display()));
        }
    }
    Ok(dir)
}

/// The body of `url`, the release `version`'s: over HTTPS, or HTTP to this machine (a test's
/// server). An HTTPS redirect must stay on HTTPS; a local HTTP response must not redirect.
fn fetch(url: &str, version: &str, limit: u64) -> Result<Vec<u8>, String> {
    let uri: ureq::http::Uri = url.parse().map_err(|_| format!("`{url}` isn't a URL"))?;
    let local = uri.scheme_str() == Some("http")
        && uri.port_u16().is_some()
        && matches!(uri.host(), Some("127.0.0.1" | "localhost" | "[::1]"));
    if uri.scheme_str() != Some("https") && !local {
        return Err(format!("`{url}` isn't HTTPS, so the toolchain isn't downloaded from it"));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(30)))
        .timeout_global(Some(std::time::Duration::from_secs(900)))
        .https_only(!local)
        .max_redirects(if local { 0 } else { 10 })
        .build()
        .into();
    let mut response = agent.get(url).call().map_err(|e| match e {
        ureq::Error::StatusCode(404) => {
            format!("there's no toolchain release {version} at {url}")
        }
        ureq::Error::StatusCode(code) => format!("{url} answered {code}"),
        e => format!("can't download {url} (is the network reachable?): {e}"),
    })?;
    if response.status().is_redirection() {
        return Err(format!("{url} redirected, so the toolchain isn't downloaded from it"));
    }
    response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| format!("can't download {url}: {e}"))
}

/// Unpacks the gzipped tarball `bytes` into `dir`, which it makes. An entry that would land
/// outside `dir` is refused.
fn unpack(bytes: &[u8], dir: &Path) -> Result<(), String> {
    unpack_with_limit(bytes, dir, MOST)
}

fn unpack_with_limit(bytes: &[u8], dir: &Path, limit: u64) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("can't make `{}`: {e}", dir.display()))?;
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz.take(limit + 1));
    let unpacked =
        archive.unpack(dir).map_err(|e| format!("can't unpack the toolchain's tarball: {e}"));
    let mut remaining = archive.into_inner();
    // What follows the archive must be gzip too. A drain that reaches the limit stops there
    // without an error, so one check of the limit covers the unpack and the drain.
    let drained = unpacked.and_then(|()| {
        std::io::copy(&mut remaining, &mut std::io::sink())
            .map(drop)
            .map_err(|e| format!("the toolchain's tarball isn't gzip: {e}"))
    });
    if remaining.limit() == 0 {
        return Err("the toolchain's tarball expands beyond the size limit".into());
    }
    drained
}

/// Builds the checkout `dir`'s toolchain with Cargo: its `wrela`.
fn build_checkout(dir: &Path) -> Result<PathBuf, String> {
    let status = Command::new("cargo")
        .args(["build", "-q", "--release", "-p", "wrela", "-p", "wrela-host", "-p", "wrela-serve"])
        .arg("--bins")
        .current_dir(dir)
        .stdout(std::io::stderr())
        .status()
        .map_err(|e| format!("can't run `cargo` to build WRELA_TOOLCHAIN's checkout: {e}"))?;
    if !status.success() {
        return Err(format!("the checkout `{}` didn't build ({status})", dir.display()));
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .filter(|d| !d.is_empty())
        .map_or_else(|| dir.join("target"), |d| dir.join(d));
    Ok(target.join("release/wrela"))
}

/// `wrela toolchain`: which toolchain runs here, where it is and why.
pub fn run(args: &[String]) -> ExitCode {
    if let Some(a) = args.first() {
        return crate::unknown(a);
    }
    let why = match std::env::var(CHOSEN) {
        Ok(why) => why,
        Err(_) => {
            let dir = std::env::current_dir().unwrap_or_default();
            match choose(None, &dir, VERSION) {
                Ok(c) => why(&c),
                Err(e) => e,
            }
        }
    };
    let at = std::env::current_exe().and_then(|p| p.canonicalize()).map_or_else(
        |e| format!("unknown ({e})"),
        |p| p.parent().unwrap_or(&p).display().to_string(),
    );
    println!("{}\nat {at}\nbecause {why}", version_line());
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wrela-pin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir.canonicalize().expect("canonical")
    }

    #[test]
    fn the_nearest_workspace_pin_chooses_a_release_unless_it_is_this_one() {
        let dir = scratch("choose");
        let inner = dir.join("games/one");
        std::fs::create_dir_all(&inner).expect("mkdir");
        let manifest = dir.join("wrela.toml");
        std::fs::write(&manifest, "[workspace]\nmembers = []\ntoolchain = \"0.2.0\"\n").expect("w");
        assert_eq!(
            choose(None, &inner, "0.1.0"),
            Ok(Choice::Pinned { version: "0.2.0".into(), manifest: manifest.clone() })
        );
        assert!(
            matches!(choose(None, &inner, "0.2.0"), Ok(Choice::Here(w)) if w.contains("pins this release"))
        );
        std::fs::write(&manifest, "[workspace]\nmembers = []\n").expect("w");
        assert!(
            matches!(choose(None, &inner, "0.1.0"), Ok(Choice::Here(w)) if w.contains("pins no toolchain"))
        );
        // A manifest that doesn't read is the command's to report.
        std::fs::write(&manifest, "[workspace]\ntoolchain = \"two\"\n").expect("w");
        assert!(matches!(choose(None, &inner, "0.1.0"), Ok(Choice::Here(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_override_is_a_checkout_or_an_unpacked_toolchain() {
        let dir = scratch("override");
        std::fs::write(dir.join("wrela.toml"), "[workspace]\ntoolchain = \"9.0.0\"\n").expect("w");
        let (checkout, unpacked, neither) =
            (dir.join("checkout"), dir.join("unpacked"), dir.join("neither"));
        for d in [&checkout, &unpacked.join("bin"), &neither] {
            std::fs::create_dir_all(d).expect("mkdir");
        }
        std::fs::write(checkout.join("Cargo.toml"), "[workspace]\n").expect("w");
        std::fs::write(unpacked.join("bin/wrela"), "").expect("w");
        let given = |p: &Path| choose(Some(p.as_os_str()), &dir, "0.1.0");
        assert_eq!(given(&checkout), Ok(Choice::Checkout(checkout.clone())));
        assert_eq!(given(&unpacked), Ok(Choice::Unpacked(unpacked.clone())));
        assert!(given(&neither).is_err_and(|e| e.contains("neither")));
        // Empty is unset.
        assert!(matches!(choose(Some(OsStr::new("")), &dir, "0.1.0"), Ok(Choice::Pinned { .. })));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_https_or_this_machine_is_downloaded_from() {
        let e = fetch("http://example.com/x", "0.1.0", MOST).expect_err("plain HTTP");
        assert!(e.contains("isn't HTTPS"), "{e}");
        assert!(fetch("file:///etc/passwd", "0.1.0", MOST).is_err());
        for url in ["http://localhost:123@example.com/x", "http://127.0.0.1:123@example.com/x"] {
            let e = fetch(url, "0.1.0", MOST).expect_err("userinfo changes the actual host");
            assert!(e.contains("isn't HTTPS"), "{e}");
        }
    }

    #[test]
    fn local_http_release_cannot_redirect() {
        use std::io::Write;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/release", listener.local_addr().expect("address"));
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream
                .write_all(b"HTTP/1.1 302 Found\r\nLocation: http://example.com/release\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .expect("reply");
        });
        let e = fetch(&url, "0.1.0", MOST).expect_err("redirect");
        assert!(e.contains("redirected"), "{e}");
        server.join().expect("server");
    }

    #[test]
    fn an_entry_outside_the_directory_is_refused() {
        let dir = scratch("unpack");
        let mut header = tar::Header::new_gnu();
        let body = b"#!/bin/sh\n";
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        // `set_path` refuses `..`, so the name is written as a hostile tarball would.
        header.as_old_mut().name[..9].copy_from_slice(b"../escape");
        header.set_cksum();
        let mut b = tar::Builder::new(Vec::new());
        b.append(&header, &body[..]).expect("append");
        let tarball = gzip(&b.into_inner().expect("tar"));
        let _ = unpack(&tarball, &dir.join("in"));
        assert!(!dir.join("escape").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_archive_cannot_expand_beyond_the_limit() {
        let dir = scratch("expansion");
        let body = vec![0; 8192];
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        let mut tar = tar::Builder::new(Vec::new());
        tar.append_data(&mut header, "large", &body[..]).expect("append");
        let compressed = gzip(&tar.into_inner().expect("tar"));
        let e = unpack_with_limit(&compressed, &dir.join("in"), 2048).expect_err("expansion");
        assert!(e.contains("expands beyond the size limit"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(bytes).expect("gzip");
        gz.finish().expect("gzip")
    }
}
