//! `wrela run <package-dir> [--port N] [--lift <package>]... [--debug]`: builds the program
//! lifted (by default its own package's literals) and serves it to Chrome on 127.0.0.1, with
//! hot reload (language.md §22): an edit of a literal (by `wrela edit`, or a saved file) shows
//! in the running program at its next frame, and any other edit builds it again and swaps the
//! new build in, in the same page (compiler/driver/src/live.rs). The builds go in
//! `<package>/build/run/`.

use std::process::ExitCode;

pub fn run(args: &[String]) -> ExitCode {
    let mut port: u16 = 8080;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--port" {
            match it.next().and_then(|p| p.parse().ok()) {
                Some(p) => port = p,
                None => {
                    eprintln!("error: --port takes a number");
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
    let kind = wrela_driver::BuildKind { debug: args.debug, testing: false };
    match wrela_driver::live::serve(&args.dir, &out, port, &args.lift, kind, false) {
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
