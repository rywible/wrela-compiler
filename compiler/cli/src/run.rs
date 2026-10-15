//! `wrela run <package-dir> [--port N] [--rooms-port N] [--lift <package>]... [--debug]`: builds
//! the program lifted (by default its own package's literals) and serves it to Chrome on
//! 127.0.0.1, with hot reload (language.md §22): an edit of a literal (by `wrela edit`, or a saved
//! file) shows in the running program at its next frame, and any other edit builds it again and
//! swaps the new build in, in the same page (compiler/driver/src/live.rs). The builds go in
//! `<package>/build/run/`.
//!
//! A program with a server role gets its rooms beside the page (`wrela-serve`, on
//! `--rooms-port`, 4433 by default), with a development certificate: each build's `net.json`
//! names them, and a build whose server changed replaces the rooms' build. Rooms' journals go in
//! `<package>/build/rooms/`, each server build's apart, so they last from run to run.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

pub fn run(args: &[String]) -> ExitCode {
    let mut port: u16 = 8080;
    let mut rooms_port: u16 = 4433;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--port" || a == "--rooms-port" {
            match it.next().and_then(|p| p.parse().ok()) {
                Some(p) if a == "--port" => port = p,
                Some(p) => rooms_port = p,
                None => {
                    eprintln!("error: {a} takes a number");
                    return ExitCode::from(2);
                }
            }
        } else {
            rest.push(a.clone());
        }
    }
    let args = match crate::package_args(&rest, true) {
        Ok(a) => a,
        Err(status) => return status,
    };
    if args.out.is_some() {
        eprintln!("error: `wrela run` builds into <package>/build/run");
        return ExitCode::from(2);
    }
    let out = args.dir.join("build").join("run");
    if let Err(why) = crate::generated_path(&args.dir, &out) {
        eprintln!("error: {why}");
        return ExitCode::from(2);
    }
    let kind = wrela_driver::BuildKind { debug: args.debug, testing: false };
    let mut rooms = Rooms {
        port: rooms_port,
        data: args.dir.join("build").join("rooms"),
        runtime: None,
        certificate: None,
        server: None,
    };
    let built = move |dir: &Path| rooms.built(dir);
    match wrela_driver::live::serve(&args.dir, &out, port, &args.lift, kind, false, built) {
        Ok(server) => {
            println!(
                "`{}` with hot reload: http://127.0.0.1:{}/ (edits to its files show as you save them)",
                args.dir.display(),
                server.port
            );
            server.wait();
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("error: {why}");
            ExitCode::from(1)
        }
    }
}

/// The rooms beside the page: started with the first build that has a server, and given each
/// server build after it that differs.
struct Rooms {
    port: u16,
    data: PathBuf,
    runtime: Option<tokio::runtime::Runtime>,
    /// The development certificate's hash, once the rooms have started.
    certificate: Option<[u8; 32]>,
    /// The rooms, and their build's WASM hash.
    server: Option<(wrela_serve::Server, u64)>,
}

impl Rooms {
    /// A build was made in `dir`: if it has a server, the rooms run it, and its `net.json` says
    /// where they are.
    fn built(&mut self, dir: &Path) {
        let Ok(wasm) = std::fs::read(dir.join("server/game.wasm")) else { return };
        if let Err(why) = self.run(&wasm) {
            eprintln!("error: the rooms: {why}");
            return;
        }
        let net = wrela_serve::net_json(self.port, self.certificate.as_ref());
        if let Err(e) = std::fs::write(dir.join("net.json"), net) {
            eprintln!("error: can't write {}: {e}", dir.join("net.json").display());
        }
    }

    /// The rooms run the server build `wasm`: started, or given it if it isn't theirs.
    fn run(&mut self, wasm: &[u8]) -> Result<(), String> {
        let hash = wrela_abi::ticks::wasm_hash(wasm);
        if self.server.as_ref().is_some_and(|(_, h)| *h == hash) {
            return Ok(());
        }
        let build =
            Arc::new(wrela_host::server::ServerBuild::from_wasm(wasm).map_err(|e| e.to_string())?);
        if let Some((server, h)) = &mut self.server {
            server.set_build(build, hash);
            *h = hash;
            println!("the rooms run the new server build; their connections closed");
            return Ok(());
        }
        let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
        let (identity, certificate) = wrela_serve::dev_identity().map_err(|e| e.to_string())?;
        let config = wrela_serve::Config {
            build,
            build_hash: hash,
            data: self.data.clone(),
            port: self.port,
            identity,
            limits: wrela_host::server::Limits::default(),
        };
        let server = wrela_serve::start(config, &runtime)
            .map_err(|e| format!("can't serve on port {}: {e}", self.port))?;
        println!("its rooms: https://localhost:{} (WebTransport)", self.port);
        self.runtime = Some(runtime);
        self.certificate = Some(certificate);
        self.server = Some((server, hash));
        Ok(())
    }
}
