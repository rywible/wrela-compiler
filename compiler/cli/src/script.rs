//! `wrela script <package-dir> <module>::<NAME> [-o <file>]`: a `std::input::Script` constant
//! (language.md §13.2) in runtime/abi `input`'s script format, printed or written to `file`,
//! for the native host to play (`wrela-host <build> --input <file>`). Exit status 1 if the
//! package has errors or the name isn't a `Script` constant.

use std::path::PathBuf;
use std::process::ExitCode;

pub fn run(args: &[String]) -> ExitCode {
    let (mut words, mut out) = (Vec::new(), None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" | "--out" => match it.next() {
                Some(o) => out = Some(PathBuf::from(o)),
                None => return crate::usage(),
            },
            _ if !a.starts_with('-') && words.len() < 2 => words.push(a.as_str()),
            _ => return crate::unknown(a),
        }
    }
    let [dir, name] = words[..] else { return crate::usage() };
    let dir = PathBuf::from(dir);
    if let Err(status) = crate::package_dir(&dir) {
        return status;
    }
    let found = wrela_driver::script::script(&dir, name);
    eprint!("{}", wrela_diag::render::render_all(&found.sources, &found.diagnostics));
    let json = match found.script {
        Ok(json) => json,
        Err(why) => {
            eprintln!("error: {why}");
            return ExitCode::from(1);
        }
    };
    match out {
        None => println!("{json}"),
        Some(path) => {
            if let Err(e) = std::fs::write(&path, format!("{json}\n")) {
                eprintln!("error: can't write `{}`: {e}", path.display());
                return ExitCode::from(2);
            }
        }
    }
    ExitCode::SUCCESS
}
