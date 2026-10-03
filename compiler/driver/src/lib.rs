//! The compiler's driver: loads a package, then checks it (names, types, the memory model, the
//! GPU rules, and the per-instantiation effects found while lowering) or builds it.
//!
//! Each call compiles the package from its files; nothing is cached between calls. Checking the
//! examples takes tens of milliseconds, and the LSP (milestone 4) will decide what incremental
//! reuse has to look like.
//!
//! The compiler recurses over syntax trees, so it runs on its own thread with a stack sized for
//! the deepest tree the parser accepts (`wrela_syntax::MAX_NESTING`, `MAX_EXPR_DEPTH`).

pub mod build;
pub mod package;

use std::path::Path;
use wrela_diag::{Diagnostic, SourceMap, Span, codes, has_errors, sort_and_dedup};
use wrela_sema::{STD_SOURCES, SourceUnit};

/// What compiling a package produced.
#[derive(Debug)]
pub struct Output {
    /// Every source file read: std's, then the package's.
    pub sources: SourceMap,
    /// Every diagnostic, sorted.
    pub diagnostics: Vec<Diagnostic>,
    /// The build's files by path relative to the output directory, in a fixed order; empty
    /// unless building found no errors.
    pub files: Vec<(String, Vec<u8>)>,
}

impl Output {
    pub fn has_errors(&self) -> bool {
        has_errors(&self.diagnostics)
    }

    /// Writes the build's files into `dir`, and makes the directories they need.
    pub fn write_to(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        for (path, bytes) in &self.files {
            let p = dir.join(path);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(p, bytes)?;
        }
        Ok(())
    }
}

/// The compiler thread's stack, and the formatter's. Generous: it's reserved, not committed, and
/// the deepest trees the parser accepts need far less.
pub const STACK_SIZE: usize = 256 << 20;

/// Checks the package at `root`: every diagnostic, including those lowering finds.
pub fn check(root: &Path) -> Output {
    on_compiler_thread(|| compile(root, false, wrela_lower::Roots::of))
}

/// Builds the package at `root`: its diagnostics, and its files when there are no errors.
pub fn build(root: &Path) -> Output {
    on_compiler_thread(|| compile(root, true, wrela_lower::Roots::of))
}

/// [`build`], lowering every function of the package that can be lowered on its own, exported
/// or not ([`wrela_lower::Roots::every_fn`]): for tests of code that nothing calls.
pub fn build_every_fn(root: &Path) -> Output {
    on_compiler_thread(|| compile(root, true, wrela_lower::Roots::every_fn))
}

fn on_compiler_thread<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        let worker = std::thread::Builder::new()
            .name("wrela".into())
            .stack_size(STACK_SIZE)
            .spawn_scoped(s, f)
            .expect("can't start the compiler's thread");
        worker.join().unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}

fn compile(
    root: &Path,
    emit: bool,
    roots: fn(&wrela_sema::Checked) -> wrela_lower::Roots,
) -> Output {
    let (sources, mut diagnostics, units) = load(root);
    let Some(units) = units else {
        return Output { sources, diagnostics, files: Vec::new() };
    };
    let checked = wrela_sema::check_program(units, &mut diagnostics);
    let mut files = Vec::new();
    if !has_errors(&diagnostics) {
        let roots = roots(&checked);
        let (lowered, d) = wrela_lower::lower(&checked, &roots, emit);
        let ok = !has_errors(&d);
        diagnostics.extend(d);
        if ok && emit {
            let out = build::emit(&lowered, &sources);
            diagnostics.extend(out.diagnostics);
            files = out.files;
        }
    }
    sort_and_dedup(&mut diagnostics);
    if has_errors(&diagnostics) {
        files.clear();
    }
    Output { sources, diagnostics, files }
}

/// Reads and parses std and the package. The units are `None` when the package can't be
/// loaded. Files with syntax errors are still checked: the parser keeps what it could read,
/// with error nodes where it couldn't, and checking stays quiet about those.
fn load(root: &Path) -> (SourceMap, Vec<Diagnostic>, Option<Vec<SourceUnit>>) {
    let mut sources = SourceMap::new();
    let mut diags = Vec::new();
    let mut units = Vec::new();
    for (name, text) in STD_SOURCES {
        let path = name.split("::").map(String::from).collect();
        let name = format!("<{name}>");
        units.push(add_unit(&mut sources, &mut diags, name, text.to_string(), path, true));
    }
    let files = match package::find_files(root) {
        Ok(files) => files,
        Err(errors) => {
            for e in errors {
                let f = sources.add(e.path.clone(), "");
                let code = if e.symlink { codes::E0208 } else { codes::E0205 };
                let mut d = Diagnostic::new(code, Span::new(f, 0, 0), e.message);
                if let Some(h) = e.help {
                    d = d.with_help(h);
                }
                diags.push(d);
            }
            sort_and_dedup(&mut diags);
            return (sources, diags, None);
        }
    };
    for pf in &files {
        let text = std::fs::read(&pf.path).map_err(|e| e.to_string()).and_then(|b| {
            String::from_utf8(b).map_err(|_| "the file isn't valid UTF-8 (L1)".to_string())
        });
        let text = match text {
            Ok(t) => t,
            Err(why) => {
                let f = sources.add(pf.display.clone(), "");
                diags.push(Diagnostic::new(
                    codes::E0205,
                    Span::new(f, 0, 0),
                    format!("can't read `{}`: {why}", pf.display),
                ));
                continue;
            }
        };
        let (name, path) = (pf.display.clone(), pf.module.clone());
        units.push(add_unit(&mut sources, &mut diags, name, text, path, false));
    }
    // Without `main.wrela` the package is a library, with no exports: a `Main.wrela` on a file
    // system that ignores case is more likely a mistake than a module named `Main`.
    let package = || units.iter().filter(|u| !u.is_std);
    if !package().any(|u| u.path == ["main"])
        && let Some(u) =
            package().find(|u| u.path.len() == 1 && u.path[0].eq_ignore_ascii_case("main"))
    {
        let file = u.ast.span.file;
        diags.push(
            Diagnostic::new(
                codes::W0003,
                Span::new(file, 0, 0),
                format!(
                    "`{}.wrela` isn't the entry module, which is `main.wrela` in lower case: the package is a library, with no exports",
                    u.path[0]
                ),
            )
            .with_help("rename it `main.wrela`, or the module something else"),
        );
    }
    (sources, diags, Some(units))
}

/// Adds a file to `sources` and parses it as the module `path`. Its lexical and syntax
/// diagnostics go to `diags`.
fn add_unit(
    sources: &mut SourceMap,
    diags: &mut Vec<Diagnostic>,
    name: String,
    text: String,
    path: Vec<String>,
    is_std: bool,
) -> SourceUnit {
    let file = sources.add(name, text);
    let parsed = wrela_syntax::parse(file, &sources.file(file).text);
    let syntax_errors =
        parsed.diagnostics.iter().filter(|d| d.is_error()).filter_map(Diagnostic::span).collect();
    diags.extend(parsed.diagnostics);
    SourceUnit { path, ast: parsed.file, is_std, syntax_errors }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A module file that can't be read, or a file whose name can't be a module, is E0205.
    #[test]
    fn unreadable_module_files_are_e0205() {
        let d = std::env::temp_dir().join(format!("wrela-driver-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        std::fs::write(d.join("main.wrela"), "fn f() -> i32 {\n    1\n}\n").expect("write");
        std::fs::write(d.join("bad.wrela"), [0x66, 0x6e, 0xff, 0xfe]).expect("write");
        let out = check(&d);
        let codes: Vec<&str> = out.diagnostics.iter().map(|x| x.code.as_str()).collect();
        assert_eq!(codes, ["E0205"], "{:?}", out.diagnostics);
        assert!(out.diagnostics[0].message.contains("isn't valid UTF-8"));

        std::fs::remove_file(d.join("bad.wrela")).expect("remove");
        std::fs::write(d.join("my-module.wrela"), "").expect("write");
        let out = check(&d);
        let codes: Vec<&str> = out.diagnostics.iter().map(|x| x.code.as_str()).collect();
        assert_eq!(codes, ["E0205"], "{:?}", out.diagnostics);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The codes `check` reports for a package of these files.
    fn codes_of(name: &str, files: &[(&str, &str)]) -> Vec<&'static str> {
        let d = std::env::temp_dir().join(format!("wrela-driver-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        for (file, text) in files {
            std::fs::write(d.join(file), text).expect("write");
        }
        let out = check(&d);
        let _ = std::fs::remove_dir_all(&d);
        out.diagnostics.iter().map(|x| x.code.as_str()).collect()
    }

    /// A pipe named like a module would block a read forever: it's refused (E0205).
    #[cfg(unix)]
    #[test]
    fn a_pipe_is_no_module() {
        let d = std::env::temp_dir().join(format!("wrela-driver-pipe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        std::fs::write(
            d.join("main.wrela"),
            "pub fn frame(time: f32, width: u32, height: u32) {}\n",
        )
        .expect("write");
        let made = std::process::Command::new("mkfifo").arg(d.join("pipe.wrela")).status();
        if made.is_ok_and(|s| s.success()) {
            let out = check(&d);
            let codes: Vec<&str> = out.diagnostics.iter().map(|x| x.code.as_str()).collect();
            assert_eq!(codes, ["E0205"], "{:?}", out.diagnostics);
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A main.wrela with no items has no `frame` either (E0703); `Main.wrela` instead of it
    /// makes a library, which is likely a mistake (W0003).
    #[test]
    fn a_program_without_frame_is_e0703() {
        assert_eq!(codes_of("empty", &[("main.wrela", "")]), ["E0703"]);
        assert_eq!(codes_of("uses", &[("main.wrela", "use std::gpu::buffer\n")]), ["E0703"]);
        let upper = codes_of("upper", &[("Main.wrela", "pub fn f() -> i32 {\n    1\n}\n")]);
        assert_eq!(upper, ["W0003"]);
    }
}
