//! `wrela-serve`: a server build's rooms over WebTransport.
//!
//! - `wrela-serve <build-dir> [--port N] [--data DIR] --cert CERT.pem --key KEY.pem`
//! - `wrela-serve <build-dir> [--port N] [--data DIR] --dev`: a development certificate, whose
//!   address and hash it writes to `<build-dir>/net.json` for the client build beside it.
//!
//! `<build-dir>` is a `wrela build` of a program with a server role: its `server/game.wasm`.
//! Rooms' journals go in `--data` (default `<build-dir>/rooms`), each server build's in a
//! directory of its own. The port defaults to 4433.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use wrela_host::server::{Limits, ServerBuild};

fn usage() -> ExitCode {
    eprintln!(
        "usage: wrela-serve <build-dir> [--port N] [--data DIR] (--cert CERT.pem --key KEY.pem | --dev)"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(dir) = args.first().map(PathBuf::from) else { return usage() };
    let mut port: u16 = 4433;
    let mut data = dir.join("rooms");
    let (mut cert, mut key, mut dev) = (None, None, false);
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
        let identity = match (cert, key, dev) {
            (Some(c), Some(k), false) => runtime
                .block_on(wtransport::Identity::load_pemfiles(c, k))
                .map_err(std::io::Error::other)?,
            (None, None, true) => {
                let (identity, hash) = wrela_serve::dev_identity()?;
                std::fs::write(dir.join("net.json"), wrela_serve::net_json(port, Some(&hash)))?;
                identity
            }
            _ => return Err(std::io::Error::other("give --cert and --key, or --dev")),
        };
        let config = wrela_serve::Config {
            build,
            build_hash: wrela_abi::ticks::wasm_hash(&wasm),
            data,
            port,
            identity,
            limits: Limits::default(),
        };
        let _server = wrela_serve::start(config, &runtime)?;
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
