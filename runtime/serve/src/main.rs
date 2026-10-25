//! `wrela-serve`: a server build's rooms over WebTransport.
//!
//! - `wrela-serve <build-dir> [--port N] [--data DIR] --cert CERT.pem --key KEY.pem --origin PAGE_ORIGIN [--origin PAGE_ORIGIN ...]`
//! - `wrela-serve <build-dir> [--port N] [--data DIR] --dev`: a development certificate, whose
//!   address and hash it writes to `<build-dir>/net.json` for the client build beside it, once
//!   it listens (so `--port 0`, a free port, gives the port it has). This server listens on
//!   loopback addresses only; a certificate server listens on all interfaces.
//!
//! `<build-dir>` is a `wrela build` of a program with a server role: its `server/game.wasm`.
//! Rooms' journals go in `--data` (default `<build-dir>/rooms`), each server build's in a
//! directory of its own, and the server's store beside them (`store.sqlite`, language.md §19.4),
//! which every build shares. The port defaults to 4433. `wrela-serve --version` says its release and
//! source revision.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use wrela_host::server::{Limits, ServerBuild};

fn usage() -> ExitCode {
    eprintln!(
        "usage: wrela-serve <build-dir> [--port N] [--data DIR] (--cert CERT.pem --key KEY.pem --origin PAGE_ORIGIN [--origin PAGE_ORIGIN ...] | --dev)"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--version"] {
        let revision = option_env!("WRELA_REVISION").unwrap_or("development build");
        println!("wrela-serve {} ({revision})", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let Some(dir) = args.first().map(PathBuf::from) else { return usage() };
    let mut port: u16 = 4433;
    let mut data = dir.join("rooms");
    let (mut cert, mut key, mut dev) = (None, None, false);
    let mut origins = Vec::new();
    let mut i = 1;
    while i < args.len() {
        let value = args.get(i + 1);
        match (args[i].as_str(), value) {
            ("--port", Some(v)) => match v.parse() {
                Ok(p) => port = p,
                Err(_) => return usage(),
            },
            ("--data", Some(v)) => data = PathBuf::from(v),
            ("--cert", Some(v)) => cert = Some(PathBuf::from(v)),
            ("--key", Some(v)) => key = Some(PathBuf::from(v)),
            ("--origin", Some(v)) if !v.is_empty() => origins.push(v.to_string()),
            ("--dev", _) => {
                dev = true;
                i += 1;
                continue;
            }
            _ => return usage(),
        }
        i += 2;
    }
    let wasm = match std::fs::read(dir.join("server/game.wasm")) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("wrela-serve: {}: {e}", dir.join("server/game.wasm").display());
            return ExitCode::FAILURE;
        }
    };
    let build = match ServerBuild::from_wasm(&wasm) {
        Ok(b) => Arc::new(b),
        Err(e) => {
            eprintln!("wrela-serve: {e}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = tokio::runtime::Runtime::new().expect("an async runtime");
    let result: std::io::Result<()> = (|| {
        // A development certificate's hash goes in `net.json`.
        let (identity, origin_policy, dev_hash) = match (cert, key, dev) {
            (Some(c), Some(k), false) if !origins.is_empty() => (
                runtime
                    .block_on(wtransport::Identity::load_pemfiles(c, k))
                    .map_err(std::io::Error::other)?,
                wrela_serve::Origins::Exact(origins),
                None,
            ),
            (None, None, true) if origins.is_empty() => {
                let (identity, hash) = wrela_serve::dev_identity()?;
                (identity, wrela_serve::Origins::Local, Some(hash))
            }
            _ => return Err(std::io::Error::other("give --cert, --key and --origin, or --dev")),
        };
        // The store is the server's, beside the builds' journals, so it outlives a build.
        let services = wrela_host::services::Services::open(&data.join("store.sqlite"))
            .map_err(std::io::Error::other)?;
        let config = wrela_serve::Config {
            build,
            build_hash: wrela_abi::ticks::wasm_hash(&wasm),
            services: Some(Arc::new(services)),
            data,
            port,
            identity,
            limits: Limits::default(),
            bounds: wrela_serve::Bounds::default(),
            origins: origin_policy,
        };
        let server = match dev_hash {
            Some(hash) => wrela_serve::start_dev(config, &hash, &dir.join("net.json"), &runtime)?,
            None => wrela_serve::start(config, &runtime)?,
        };
        let port = server.local_addr().port();
        eprintln!("wrela-serve: rooms of {} on port {port}", dir.display());
        runtime.block_on(std::future::pending::<()>());
        Ok(())
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wrela-serve: {e}");
            ExitCode::FAILURE
        }
    }
}
